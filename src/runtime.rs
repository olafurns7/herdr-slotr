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
    if pool.memory_gated
        && let Some(total) = stats::read().total_mib
    {
        ensure!(
            cost as f64 <= (total - cfg.admission.reserve_mib as f64).max(0.0),
            "cost exceeds MemTotal minus admission.reserve_mib"
        );
    }
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
        let (seq, ticket) = loop {
            s.enqueue_seq += 1;
            let seq = s.enqueue_seq;
            let ticket = state::writable(&state::root().join(format!("ticket-{seq}")))?;
            match flock(&ticket, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => break (seq, ticket),
                Err(rustix::io::Errno::WOULDBLOCK) => continue,
                Err(error) => return Err(error.into()),
            }
        };
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
            seen_at: manager::now(),
            stop_claimed_by: None,
        });
        Ok((seq, ticket))
    })?;
    let result = (|| {
        let mut previous = None;
        let mut previous_error = None;
        while manager::cancelled() == 0 {
            let poll = (|| -> Result<_> {
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
                        .iter_mut()
                        .find(|q| q.enqueue_seq == seq)
                        .ok_or_else(|| anyhow::anyhow!("ticket disappeared"))?;
                    q.seen_at = now;
                    let q = q.clone();
                    let (slot, mut reason) =
                        admission::decide(s, &q, cfg, &obs, &sample, now, |slot| {
                            pool.ports.as_ref().is_none_or(|p| {
                                manager::ports_free(p.base + slot * p.stride, p.probe)
                            })
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
                        stopping_at: None,
                        warned_at: None,
                        warned_for: None,
                        warned_waiters: vec![],
                        idle_since: None,
                        cpu_usage_usec: None,
                        cpu_sample_at: None,
                        holder_idle_since: None,
                        holder_idle_warned: false,
                    };
                    s.holders.push(holder.clone());
                    s.queue.retain(|q| q.enqueue_seq != seq);
                    Ok((Some(holder), None))
                })?;
                Ok((holder, reason, legacy_file))
            })();
            let (holder, reason, legacy_file) = match poll {
                Ok(value) => {
                    previous_error = None;
                    value
                }
                Err(error) => {
                    let message = error.to_string();
                    if message == "ticket disappeared" {
                        return Err(error);
                    }
                    if previous_error.as_ref() != Some(&message) {
                        eprintln!("slotr: waiter tick: {error:#}");
                        let _ = manager::event(
                            json!({"event":"waiter_tick_error","ticket":seq,"error":message}),
                        );
                        previous_error = Some(message);
                    }
                    thread::sleep(Duration::from_millis(cfg.admission.queue_poll_ms));
                    continue;
                }
            };
            // Keep shared locks through startup; exclusive mode needs a handoff.
            let legacy_file = legacy_file
                .filter(|_| pool.legacy_lock.as_ref().is_none_or(|l| l.mode == "shared"));

            if let Some(holder) = holder {
                let _legacy_file = legacy_file;
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
            } else {
                drop(legacy_file);
                if reason != previous {
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
            }
            thread::sleep(Duration::from_millis(cfg.admission.queue_poll_ms));
        }
        Ok(128 + manager::cancelled())
    })();
    drop(ticket);
    let cleanup = state::transaction(|s| {
        s.queue.retain(|q| q.enqueue_seq != seq);
        s.holders
            .retain(|h| h.request.enqueue_seq != seq || h.started);
        state::clean_tickets(s)
    });
    if let Err(error) = cleanup {
        eprintln!("slotr: cleanup: {error:#}");
    }
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
        format!("TimeoutStopSec={}", cfg.watchdog.term_grace_seconds + 5.0),
        "Restart=no".into(),
        "MemoryAccounting=yes".into(),
        "OOMScoreAdjust=500".into(),
        "OOMPolicy=kill".into(),
    ] {
        command.arg(format!("--property={prop}"));
    }
    for (key, _) in std::env::vars_os() {
        if let Some(name) = key.to_str()
            && !name.is_empty()
            && name
                .bytes()
                .enumerate()
                .all(|(i, c)| c == b'_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit()))
        {
            command.arg(format!("--setenv={name}"));
        }
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
    let mut stop_failed = false;
    let status = loop {
        // Cancellation is checked after try_wait, so a launcher that exits on the
        // same group HUP cannot leave the unit running.
        let exited = child.try_wait()?;
        if manager::cancelled() != 0
            && !stopped
            && state::read(&state::root()).is_ok_and(|s| {
                s.holders
                    .iter()
                    .find(|other| other.run == h.run)
                    .is_none_or(|other| other.started)
            })
        {
            match manager::ctl(&["stop", "--no-block", &h.run]) {
                Ok(output) if output.status.success() => stopped = true,
                Ok(output) => {
                    stop_failed = true;
                    eprintln!(
                        "slotr: stop request: {}",
                        String::from_utf8_lossy(&output.stderr).trim()
                    )
                }
                Err(error) => {
                    stop_failed = true;
                    eprintln!("slotr: stop request: {error:#}")
                }
            }
        }
        if let Some(status) = exited {
            break status;
        }
        thread::sleep(Duration::from_millis(20));
    };
    // A unit that would not stop may hold the relay pipe open for ever.
    let tail = if stop_failed && !stopped {
        eprintln!(
            "slotr: {} may still be running; run: slotr stop {}",
            h.run, h.run
        );
        vec![]
    } else {
        relay.join().unwrap_or_default()
    };
    let error = String::from_utf8_lossy(&tail).to_lowercase();
    let started = state::read(&state::root())
        .map(|s| {
            s.holders
                .iter()
                .find(|other| other.run == h.run)
                .is_none_or(|other| other.started)
        })
        .unwrap_or(true);
    if !started && error.contains("connect to") && error.contains("bus") {
        bail!(manager::BUS_ERROR);
    }
    let collision = !started
        && !status.success()
        && (error.contains("already exists")
            || error.contains("unit exists")
            || error.contains("already loaded"));
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
        manager::ctl(&["stop", "--no-block", run])?.status.success(),
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
        let legacy_ok = manager::legacy_free(pool);
        let holders: Vec<_> = s
            .holders
            .iter()
            .filter(|h| h.request.pool == *name)
            .map(|h| {
                let mut data = serde_json::to_value(h).unwrap();
                let _ = crate::timestamp::display(&mut data);
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
                let _ = crate::timestamp::display(&mut data);
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
    let mut last_stop = serde_json::to_value(&s.last_stop)?;
    crate::timestamp::display(&mut last_stop)?;
    let data = json!({"schema_version":1,"stats":sample,"pools":pools,"last_stop":last_stop,"events_path":manager::events_path()});
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
