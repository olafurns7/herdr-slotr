use crate::{
    admission,
    config::Config,
    manager::{self, Observation},
    state::{self, Holder, State, StopRecord},
    stats,
};
use anyhow::{Result, ensure};
use serde_json::json;
use std::{
    collections::BTreeMap,
    os::unix::process::CommandExt,
    process::Command,
    thread,
    time::{Duration, Instant},
};

#[derive(Default)]
struct Action {
    warn: Option<(Holder, String)>,
    stop: Option<(Holder, String, State)>,
    cancelled: Option<String>,
}
fn lease_action(
    s: &mut State,
    run: &str,
    cfg: &Config,
    obs: &BTreeMap<String, Observation>,
    sample: &stats::Stats,
    now: f64,
) -> Action {
    let mut action = Action::default();
    let Some(index) = s.holders.iter().position(|h| h.run == run) else {
        return action;
    };
    let h = s.holders[index].clone();
    let pool = &cfg.pools[&h.request.pool];
    if !pool.evictable || cfg.lease.on_expiry == "off" || h.stopping_at.is_some() {
        return action;
    }
    let head = admission::head(s, &h.request.pool, cfg).cloned();
    let eligible = |holder: &Holder| {
        holder.started
            && holder.stopping_at.is_none()
            && (admission::overdue(holder, now)
                || admission::yielding(holder, s, pool)
                || (cfg.lease.idle_release_minutes > 0.0
                    && holder
                        .idle_since
                        .is_some_and(|t| now - t >= cfg.lease.idle_release_minutes * 60.0)))
    };
    let other = head.as_ref().filter(|q| {
        q.campaign != h.request.campaign
            && (pool.campaign_cap == 0
                || admission::campaign_held(s, &q.pool, &q.campaign) < pool.campaign_cap as usize)
            && now - q.since >= cfg.lease.waiter_min_wait_seconds
            && now - q.seen_at <= 5.0 + 3.0 * cfg.admission.queue_poll_ms as f64 / 1000.0
            && eligible(&h)
    });
    let fits = other.is_some_and(|q| admission::fits_without(s, &h, q, cfg, obs, sample, now));
    let candidate = other
        .and_then(|q| {
            let candidates = || {
                s.holders.iter().filter(|holder| {
                    holder.request.pool == h.request.pool
                        && holder.request.campaign != q.campaign
                        && eligible(holder)
                })
            };
            candidates()
                .filter(|holder| admission::fits_without(s, holder, q, cfg, obs, sample, now))
                .min_by_key(|holder| holder.admit_seq)
                .or_else(|| candidates().min_by_key(|holder| holder.admit_seq))
        })
        .map(|holder| holder.run.as_str());
    let may_stop = fits
        && candidate == Some(run)
        && cfg.lease.on_expiry == "stop"
        && other.is_some_and(|q| {
            q.stop_claimed_by
                .as_deref()
                .is_none_or(|claim| claim == run)
        });
    if let Some(waiter) = h.warned_for {
        let same_head = head.as_ref().is_some_and(|q| q.enqueue_seq == waiter);
        let grace_ended = now - h.warned_at.unwrap_or(now) >= cfg.lease.grace_seconds;
        if !same_head || (grace_ended && !may_stop) {
            s.holders[index].warned_at = None;
            s.holders[index].warned_for = None;
            if let Some(q) = s
                .queue
                .iter_mut()
                .find(|q| q.enqueue_seq == waiter && q.stop_claimed_by.as_deref() == Some(run))
            {
                q.stop_claimed_by = None;
            }
            action.cancelled = Some(h.run.clone());
        } else if grace_ended {
            let reason = if admission::yielding(&h, s, pool) {
                "campaign_yield"
            } else if admission::overdue(&h, now) {
                "lease_expired"
            } else {
                "idle_release"
            };
            action.stop = Some((h, reason.into(), s.clone()));
        }
        return action;
    }
    if let Some(q) = other
        && candidate == Some(run)
        && q.stop_claimed_by.is_none()
    {
        let reason = if !fits {
            "overdue_stop_would_not_help"
        } else if admission::yielding(&h, s, pool) {
            "campaign_yield"
        } else if admission::overdue(&h, now) {
            "lease_expired"
        } else {
            "idle_release"
        };
        if may_stop {
            s.queue
                .iter_mut()
                .find(|w| w.enqueue_seq == q.enqueue_seq)
                .unwrap()
                .stop_claimed_by = Some(run.into());
            s.holders[index].warned_at = Some(now);
            s.holders[index].warned_for = Some(q.enqueue_seq);
        }
        if !h.warned_waiters.contains(&q.enqueue_seq) {
            s.holders[index].warned_waiters.push(q.enqueue_seq);
            action.warn = Some((s.holders[index].clone(), reason.into()));
        }
    }
    action
}
pub fn supervise(run: &str, cmd: &[String], cfg: &Config) -> Result<i32> {
    manager::signals();
    let h = state::transaction(|s| {
        let h = s
            .holders
            .iter_mut()
            .find(|h| h.run == run)
            .ok_or_else(|| anyhow::anyhow!("supervisor has no admission record"))?;
        ensure!(!h.started, "supervisor already started");
        h.started = true;
        Ok(h.clone())
    })?;
    let (legacy_ok, _legacy_file) = manager::legacy(&cfg.pools[&h.request.pool], false)?;
    ensure!(legacy_ok, "legacy lock changed before supervisor start");
    let mut command = Command::new(&cmd[0]);
    command
        .args(&cmd[1..])
        .env("SLOTR_RUN", &h.run)
        .env("SLOTR_POOL", &h.request.pool)
        .env("SLOTR_SLOT", h.slot.to_string());
    if let Some(base) = h.port_base {
        command.env("SLOTR_PORT_BASE", base.to_string());
    } else {
        command.env_remove("SLOTR_PORT_BASE");
    }
    // setsid contains ordinary descendants in the fake-manager harness; systemd
    // KillMode=control-group additionally contains descendants that call setsid.
    unsafe {
        command.pre_exec(|| {
            rustix::process::setsid()
                .map(|_| ())
                .map_err(std::io::Error::from)
        });
    }
    let mut child = command.spawn()?;
    eprintln!("slotr: admitted {}", h.run);
    let result = (|| {
        let _ = manager::event(
            json!({"event":"admit","run":h.run,"pool":h.request.pool,"campaign":h.request.campaign,"cost_mib":h.request.cost_mib}),
        );
        manager::hook(cfg, "on_admit", &h, "admitted");
        let mut memory_count: u32 = 0;
        let mut psi_count: u32 = 0;
        let mut pressure_warned = false;
        let mut tick = Instant::now();
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(manager::code(status));
            }
            if manager::cancelled() != 0 {
                manager::terminate(&mut child, cfg.watchdog.term_grace_seconds)?;
                return Ok(128 + manager::cancelled());
            }
            if tick.elapsed() < Duration::from_millis(cfg.watchdog.interval_ms) {
                thread::sleep(Duration::from_millis(10));
                continue;
            }
            tick = Instant::now();
            let tick_result = (|| -> Result<Option<i32>> {
                let sample = stats::read();
                let now = manager::now();
                memory_count = if sample
                    .available_mib
                    .is_some_and(|v| v < cfg.watchdog.stop_available_mib as f64)
                {
                    memory_count.saturating_add(1)
                } else {
                    0
                };
                psi_count = if sample
                    .psi_full_avg10
                    .is_some_and(|v| v >= cfg.watchdog.stop_psi_full_avg10_min)
                {
                    psi_count.saturating_add(1)
                } else {
                    0
                };
                let pressure = if sample
                    .available_mib
                    .is_some_and(|v| v < cfg.watchdog.emergency_available_mib as f64)
                {
                    Some("emergency_available_mib")
                } else if memory_count >= cfg.watchdog.stop_available_samples {
                    Some("stop_available_mib")
                } else if psi_count >= cfg.watchdog.stop_psi_samples {
                    Some("stop_psi_full_avg10")
                } else {
                    None
                };
                let obs = manager::observations(&state::read(&state::root())?)?;
                let action = state::transaction(|s| {
                    manager::reconcile(s, &obs);
                    admission::update_recovery(s, &sample, cfg, now);
                    let fitting_waiter =
                        admission::head(s, &h.request.pool, cfg).is_some_and(|q| {
                            q.campaign != h.request.campaign
                                && admission::fits_without(s, &h, q, cfg, &obs, &sample, now)
                        });
                    if let Some(holder) = s.holders.iter_mut().find(|h| h.run == run) {
                        if let Some(cpu) = obs.get(run).and_then(|o| o.cpu_usec) {
                            if let Some((last, at)) =
                                holder.cpu_usage_usec.zip(holder.cpu_sample_at)
                            {
                                if fitting_waiter
                                    && now > at
                                    && cpu >= last
                                    && (cpu - last) as f64 / 1000.0
                                        < cfg.lease.idle_cpu_ms_per_min * (now - at) / 60.0
                                {
                                    holder.idle_since.get_or_insert(at);
                                } else {
                                    holder.idle_since = None;
                                }
                            }
                            holder.cpu_usage_usec = Some(cpu);
                            holder.cpu_sample_at = Some(now);
                        } else {
                            holder.idle_since = None;
                        }
                    }
                    let newest = s
                        .holders
                        .iter()
                        .filter(|h| {
                            h.started
                                && h.stopping_at.is_none()
                                && cfg.pools.get(&h.request.pool).is_some_and(|p| p.evictable)
                        })
                        .max_by_key(|h| h.admit_seq);
                    let mut action = Action::default();
                    if cfg.pools[&h.request.pool].evictable
                        && newest.is_some_and(|h| h.run == run)
                        && s.last_stop.as_ref().is_none_or(|stop| {
                            now - stop.at
                                >= cfg
                                    .admission
                                    .recovery_healthy_seconds
                                    .max(cfg.watchdog.interval_ms as f64 / 1000.0)
                        })
                        && let Some(reason) = pressure
                    {
                        if cfg.watchdog.on_pressure == "stop" {
                            action.stop = Some((h.clone(), reason.into(), s.clone()));
                        } else if cfg.watchdog.on_pressure == "warn" && !pressure_warned {
                            action.warn = Some((h.clone(), reason.into()));
                        }
                    }
                    if action.stop.is_none() && action.warn.is_none() {
                        action = lease_action(s, run, cfg, &obs, &sample, now);
                    }
                    if let Some((holder, reason, _)) = &action.stop {
                        if let Some(record) =
                            s.holders.iter_mut().find(|record| record.run == holder.run)
                        {
                            record.stopping_at = Some(now);
                        }
                        s.last_stop = Some(StopRecord {
                            at: now,
                            run: holder.run.clone(),
                            reason: reason.clone(),
                        });
                        s.healthy_since = None;
                    }
                    Ok(action)
                })?;
                if pressure.is_none() {
                    pressure_warned = false;
                }
                if let Some(run) = action.cancelled {
                    let _ = manager::event(json!({"event":"warn_cancelled","run":run}));
                }
                if let Some((holder, reason)) = action.warn {
                    let _ = manager::event(
                        json!({"event":"warn","run":holder.run,"reason":reason,"cpu_usage_usec":obs.get(run).and_then(|o|o.cpu_usec)}),
                    );
                    manager::hook(cfg, "on_warn", &holder, &reason);
                    if pressure == Some(reason.as_str()) {
                        pressure_warned = true;
                    }
                }
                if let Some((holder, reason, snapshot)) = action.stop {
                    manager::kill_group(&child, rustix::process::Signal::TERM)?;
                    let deadline =
                        Instant::now() + Duration::from_secs_f64(cfg.watchdog.term_grace_seconds);
                    let _ = manager::stop_event(cfg, &holder, &reason, &snapshot, &obs, &sample);
                    let hook_cfg = cfg.clone();
                    let hook_holder = holder.clone();
                    let hook = thread::spawn(move || {
                        manager::hook(&hook_cfg, "on_stop", &hook_holder, &reason)
                    });
                    manager::finish_termination(&mut child, deadline)?;
                    let _ = hook.join();
                    return Ok(Some(crate::ExitCode::Stopped.value()));
                }
                Ok(None)
            })();
            match tick_result {
                Ok(Some(code)) => return Ok(code),
                Ok(None) => {}
                Err(error) => {
                    eprintln!("slotr: supervisor tick: {error:#}");
                    let _ = manager::event(
                        json!({"event":"supervisor_tick_error","run":run,"error":error.to_string()}),
                    );
                }
            }
        }
    })();
    // Retain the record until systemd confirms the unit has gone. A unit still
    // owns descendants after its supervisor exits (KillMode=control-group).
    result
}

#[test]
fn warn_once_when_an_effective_head_returns() {
    let mut cfg: Config = toml::from_str(include_str!("../config.example.toml")).unwrap();
    cfg.lease.on_expiry = "warn".into();
    cfg.lease.waiter_min_wait_seconds = 0.0;
    let request = |seq, campaign: &str| crate::state::Request {
        enqueue_seq: seq,
        pool: "default".into(),
        campaign: campaign.into(),
        purpose: "check".into(),
        task: String::new(),
        pane: String::new(),
        cwd: String::new(),
        cost_mib: 0,
        lease_seconds: 1.0,
        since: 0.0,
        seen_at: 10.0,
        stop_claimed_by: None,
    };
    let holder = Holder {
        request: request(0, "A"),
        run: "slotr-default-1".into(),
        admit_seq: 1,
        slot: 0,
        port_base: None,
        port_end: None,
        admitted_at: 0.0,
        lease_expires_at: Some(1.0),
        started: true,
        stopping_at: None,
        warned_at: None,
        warned_for: None,
        warned_waiters: vec![],
        idle_since: None,
        cpu_usage_usec: None,
        cpu_sample_at: None,
    };
    let mut s = State {
        holders: vec![holder],
        queue: vec![request(2, "C")],
        ..State::default()
    };
    let obs = BTreeMap::new();
    let sample = stats::Stats::default();
    assert!(
        lease_action(&mut s, "slotr-default-1", &cfg, &obs, &sample, 10.0)
            .warn
            .is_some()
    );
    // An older, formerly capped ticket becomes effective head, then departs.
    s.queue.insert(0, request(1, "B"));
    assert!(
        lease_action(&mut s, "slotr-default-1", &cfg, &obs, &sample, 11.0)
            .warn
            .is_some()
    );
    s.queue.remove(0);
    assert!(
        lease_action(&mut s, "slotr-default-1", &cfg, &obs, &sample, 12.0)
            .warn
            .is_none()
    );
}
