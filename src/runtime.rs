use crate::{
    Run, admission,
    config::{self, Config},
    manager::{self},
    state::{self, Holder, Request},
    stats,
};
use anyhow::{Result, bail, ensure};
use rustix::fs::{FlockOperation, flock};
use serde_json::json;
use std::{
    collections::BTreeMap,
    io::{IsTerminal, Read, Write},
    process::{Command, Stdio},
    thread,
    time::Duration,
};

pub fn run(args: Run, cfg: &Config) -> Result<i32> {
    let pool = cfg
        .pools
        .get(&args.pool)
        .ok_or_else(|| anyhow::anyhow!("unknown pool: {}", args.pool))?;
    ensure!(
        !args.campaign.trim().is_empty(),
        "campaign must not be empty"
    );
    ensure!(!args.purpose.trim().is_empty(), "purpose must not be empty");
    let cost = if let Some(kind) = &args.kind {
        pool.kinds
            .get(kind)
            .ok_or_else(|| anyhow::anyhow!("unknown kind: {kind}"))?
            .cost_mib
    } else {
        args.cost.unwrap_or(pool.default_cost_mib)
    };
    let cap = config::duration(&pool.max_lease)?;
    let requested = args
        .lease
        .as_deref()
        .map(config::duration)
        .transpose()?
        .unwrap_or(cap);
    let lease = if cap > 0.0 && (requested == 0.0 || requested > cap) {
        cap
    } else {
        requested
    };
    ensure!(
        manager::ctl(&["show", "--property=Version"])?
            .status
            .success(),
        manager::BUS_ERROR
    );
    manager::signals();
    let (seq, ticket) = state::transaction(|s| {
        s.enqueue_seq += 1;
        let seq = s.enqueue_seq;
        let ticket = state::writable(&state::root().join(format!("ticket-{seq}")))?;
        flock(&ticket, FlockOperation::LockExclusive)?;
        s.queue.push(Request {
            enqueue_seq: seq,
            pool: args.pool.clone(),
            campaign: args.campaign.clone(),
            purpose: args.purpose.clone(),
            task: args.task.clone(),
            pane: args.pane.clone(),
            cwd: std::env::current_dir()?.display().to_string(),
            cost_mib: cost,
            lease_seconds: lease,
            since: manager::now(),
            stop_claimed_by: None,
        });
        Ok((seq, ticket))
    })?;
    let result = (|| {
        let mut previous = None;
        while manager::cancelled() == 0 {
            let obs = manager::observations(&state::read(&state::root())?)?;
            let sample = stats::read();
            let now = manager::now();
            let (legacy_ok, legacy_file) = manager::legacy(pool, false)?;
            let (holder, reason) = state::transaction(|s| {
                manager::reconcile(s, &obs);
                state::clean_tickets(s)?;
                admission::update_recovery(s, &sample, cfg, now);
                let q = s
                    .queue
                    .iter()
                    .find(|q| q.enqueue_seq == seq)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("ticket disappeared"))?;
                let (slot, mut reason) =
                    admission::decide(s, &q, cfg, &obs, &sample, now, |slot| {
                        pool.ports
                            .as_ref()
                            .is_none_or(|p| manager::ports_free(p.base + slot * p.stride, p.probe))
                    });
                if reason.is_none() && !legacy_ok {
                    reason = Some("legacy_lock");
                }
                if admission::head(s, &args.pool, cfg).is_none_or(|q| q.enqueue_seq != seq) {
                    reason = Some(reason.unwrap_or("fifo"));
                }
                if reason.is_some() {
                    return Ok((None, reason));
                }
                s.admit_seq += 1;
                let base = pool
                    .ports
                    .as_ref()
                    .map(|p| p.base + slot.unwrap() * p.stride);
                let holder = Holder {
                    request: q,
                    run: format!("slotr-{}-{}", args.pool, s.admit_seq),
                    admit_seq: s.admit_seq,
                    slot: slot.unwrap(),
                    port_base: base,
                    port_end: base.zip(pool.ports.as_ref()).map(|(b, p)| b + p.stride - 1),
                    admitted_at: now,
                    lease_expires_at: (lease > 0.0).then_some(now + lease),
                    started: false,
                    warned_at: None,
                    warned_for: None,
                    warned_waiters: vec![],
                    idle_since: None,
                    cpu_usage_usec: None,
                    cpu_sample_at: None,
                };
                s.holders.push(holder.clone());
                s.queue.retain(|q| q.enqueue_seq != seq);
                Ok((Some(holder), None))
            })?;
            // Keep shared locks through startup; exclusive mode needs a handoff.
            let _legacy_file = legacy_file
                .filter(|_| pool.legacy_lock.as_ref().is_none_or(|l| l.mode == "shared"));

            if let Some(holder) = holder {
                let (rc, collision) = launch(&holder, &args.cmd, cfg)?;
                if !collision {
                    return Ok(rc);
                }
                state::transaction(|s| {
                    s.holders.retain(|h| h.run != holder.run);
                    s.queue.push(holder.request.clone());
                    s.queue.sort_by_key(|q| q.enqueue_seq);
                    Ok(())
                })?;
            } else if reason != previous {
                eprintln!(
                    "slotr: waiting: {}{}",
                    reason.unwrap_or("fifo"),
                    if reason == Some("legacy_lock") {
                        manager::legacy_pid(pool)
                            .map(|pid| format!(" (holder pid {pid})"))
                            .unwrap_or_default()
                    } else {
                        String::new()
                    }
                );
                previous = reason;
            }
            thread::sleep(Duration::from_millis(cfg.admission.queue_poll_ms));
        }
        Ok(128 + manager::cancelled())
    })();
    drop(ticket);
    let cleanup = state::transaction(|s| {
        s.queue.retain(|q| q.enqueue_seq != seq);
        state::clean_tickets(s)
    });
    cleanup?;
    result
}
fn launch(h: &Holder, cmd: &[String], cfg: &Config) -> Result<(i32, bool)> {
    let mut command = Command::new(stats::setting("SYSTEMD_RUN", "systemd-run"));
    command.args([
        "--user",
        "--unit",
        &h.run,
        "--collect",
        "--wait",
        if std::io::stdin().is_terminal() {
            "--pty"
        } else {
            "--pipe"
        },
        "--expand-environment=no",
    ]);
    command.arg(format!("--working-directory={}", h.request.cwd));
    for prop in [
        "KillMode=control-group".into(),
        format!("TimeoutStopSec={}", cfg.watchdog.term_grace_seconds),
        "Restart=no".into(),
        "MemoryAccounting=yes".into(),
        "OOMScoreAdjust=500".into(),
        "OOMPolicy=kill".into(),
    ] {
        command.arg(format!("--property={prop}"));
    }
    for (key, value) in std::env::vars_os() {
        let mut arg = std::ffi::OsString::from("--setenv=");
        arg.push(key);
        arg.push("=");
        arg.push(value);
        command.arg(arg);
    }
    command
        .arg("--")
        .arg(std::env::current_exe()?)
        .arg("_supervise")
        .arg(&h.run)
        .arg("--")
        .args(cmd)
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let mut stderr = child.stderr.take().unwrap();
    let relay = thread::spawn(move || {
        let mut tail = vec![];
        let mut buf = [0; 4096];
        loop {
            match stderr.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let _ = std::io::stderr().write_all(&buf[..n]);
                    tail.extend_from_slice(&buf[..n]);
                    if tail.len() > 65536 {
                        tail.drain(..tail.len() - 65536);
                    }
                }
            }
        }
        tail
    });
    let mut stopped = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if manager::cancelled() != 0
            && !stopped
            && state::read(&state::root())?
                .holders
                .iter()
                .any(|other| other.run == h.run && other.started)
        {
            manager::ctl(&["stop", &h.run])?;
            stopped = true;
        }
        thread::sleep(Duration::from_millis(20));
    };
    let tail = relay.join().unwrap_or_default();
    let error = String::from_utf8_lossy(&tail).to_lowercase();
    if error.contains("connect to") && error.contains("bus") {
        bail!(manager::BUS_ERROR);
    }
    let started = state::read(&state::root())?
        .holders
        .iter()
        .any(|other| other.run == h.run && other.started);
    let collision = !started
        && !status.success()
        && (error.contains("already exists") || error.contains("unit exists"));
    Ok((
        if manager::cancelled() != 0 {
            128 + manager::cancelled()
        } else {
            manager::code(status)
        },
        collision,
    ))
}
pub fn stop(run: &str) -> Result<i32> {
    ensure!(
        state::read(&state::root())?
            .holders
            .iter()
            .any(|h| h.run == run),
        "unknown run: {run}"
    );
    ensure!(
        manager::ctl(&["stop", run])?.status.success(),
        "could not stop {run}"
    );
    Ok(crate::ExitCode::Success.value())
}
pub fn status(cfg: &Config, as_json: bool) -> Result<()> {
    let mut s = state::read(&state::root())?;
    let obs = manager::observations(&s)?;
    manager::reconcile(&mut s, &obs); // In-memory only: status never writes state.
    let sample = stats::read();
    let now = manager::now();
    let mut pools = BTreeMap::new();
    for (name, pool) in &cfg.pools {
        let (legacy_ok, _lock) = manager::legacy(pool, true)?;
        let holders: Vec<_> = s
            .holders
            .iter()
            .filter(|h| h.request.pool == *name)
            .map(|h| {
                let mut data = serde_json::to_value(h).unwrap();
                data["state"] = json!(admission::holder_state(h, &s, cfg, now));
                data["anon_mib"] = json!(obs.get(&h.run).and_then(|o| o.anon_mib));
                data["cpu_usage_usec"] = json!(obs.get(&h.run).and_then(|o| o.cpu_usec));
                data["notify"] = json!(if h.request.task.is_empty() && h.request.pane.is_empty() {
                    "none"
                } else {
                    "configured"
                });
                data
            })
            .collect();
        let queue: Vec<_> = s
            .queue
            .iter()
            .filter(|q| q.pool == *name)
            .enumerate()
            .map(|(i, q)| {
                let (_, mut reason) = admission::decide(&s, q, cfg, &obs, &sample, now, |slot| {
                    pool.ports
                        .as_ref()
                        .is_none_or(|p| manager::ports_free(p.base + slot * p.stride, p.probe))
                });
                if reason.is_none() && !legacy_ok {
                    reason = Some("legacy_lock");
                }
                let mut data = serde_json::to_value(q).unwrap();
                data["position"] = json!(i + 1);
                data["wait_reason"] = json!(
                    reason.unwrap_or(
                        if admission::head(&s, name, cfg)
                            .is_some_and(|first| first.enqueue_seq == q.enqueue_seq)
                        {
                            "ready"
                        } else {
                            "fifo"
                        }
                    )
                );
                if reason == Some("legacy_lock") {
                    data["legacy_holder_pid"] = json!(manager::legacy_pid(pool));
                }
                data
            })
            .collect();
        let reserved = admission::outstanding(&s, &obs);
        pools.insert(name,json!({"slots":pool.slots,"holders":holders,"queue":queue,"budget":{"reserve_mib":cfg.admission.reserve_mib,"outstanding_mib":reserved,"projected_free_mib":sample.available_mib.map(|v|v-reserved-pool.default_cost_mib as f64)}}));
    }
    let data = json!({"schema_version":1,"stats":sample,"pools":pools,"last_stop":s.last_stop,"events_path":manager::events_path()});
    if as_json {
        println!("{}", serde_json::to_string(&data)?);
    } else {
        println!("Stats: {}", data["stats"]);
        for (name, pool) in pools {
            println!(
                "Pool {name}: {} slots; budget {}",
                pool["slots"], pool["budget"]
            );
            for h in pool["holders"].as_array().unwrap() {
                println!(
                    "  {} {} campaign={} purpose={} task={} pane={} cwd={} seq={} admitted={} lease_expires={} cost={} anon={} ports={}-{} notify={}",
                    h["run"],
                    h["state"],
                    h["campaign"],
                    h["purpose"],
                    h["task"],
                    h["pane"],
                    h["cwd"],
                    h["admit_seq"],
                    h["admitted_at"],
                    h["lease_expires_at"],
                    h["cost_mib"],
                    h["anon_mib"],
                    h["port_base"],
                    h["port_end"],
                    h["notify"]
                );
            }
            for q in pool["queue"].as_array().unwrap() {
                println!(
                    "  queue #{} campaign={} purpose={} cost={} since={} reason={} holder_pid={}",
                    q["position"],
                    q["campaign"],
                    q["purpose"],
                    q["cost_mib"],
                    q["since"],
                    q["wait_reason"],
                    q["legacy_holder_pid"]
                );
            }
        }
        println!(
            "Last stop: {}\nEvents: {}",
            data["last_stop"],
            manager::events_path().display()
        );
    }
    Ok(())
}
