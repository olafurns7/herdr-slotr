use crate::{
    admission,
    config::Config,
    manager::{self, Observation},
    state::{self, Holder, State, StopRecord},
    stats,
};
use anyhow::{Result, ensure};
use serde::Deserialize;
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
#[derive(Clone, Copy, Debug, PartialEq)]
enum HolderActivity {
    Idle,
    Active,
    Unknown,
}
fn holder_activity(cfg: &Config, pane: &str) -> HolderActivity {
    if cfg.lease.holder_probe.is_empty() || pane.is_empty() {
        return HolderActivity::Unknown;
    }
    let probe = || -> Result<HolderActivity> {
        #[derive(Deserialize)]
        struct Listing {
            result: Agents,
        }
        #[derive(Deserialize)]
        struct Agents {
            agents: Vec<Agent>,
        }
        #[derive(Deserialize)]
        struct Agent {
            pane_id: String,
            agent_status: String,
        }
        let output = manager::capture(
            Command::new(&cfg.lease.holder_probe[0])
                .args(&cfg.lease.holder_probe[1..])
                .process_group(0),
            Duration::from_secs(5),
            true,
        )?;
        ensure!(output.status.success(), "holder probe failed");
        let listing: Listing = serde_json::from_slice(&output.stdout)?;
        let matches: Vec<_> = listing
            .result
            .agents
            .iter()
            .filter(|a| a.pane_id == pane)
            .collect();
        Ok(match matches.as_slice() {
            [] => HolderActivity::Idle,
            [agent] => match agent.agent_status.as_str() {
                "idle" | "done" => HolderActivity::Idle,
                "working" | "blocked" | "waiting" | "starting" => HolderActivity::Active,
                _ => HolderActivity::Unknown,
            },
            _ => HolderActivity::Unknown,
        })
    };
    probe().unwrap_or(HolderActivity::Unknown)
}
fn update_holder_activity(holder: &mut Holder, activity: HolderActivity, now: f64) {
    match activity {
        HolderActivity::Idle => {
            holder.holder_idle_since.get_or_insert(now);
        }
        HolderActivity::Active | HolderActivity::Unknown => holder.holder_idle_since = None,
    }
}
fn holder_idle(holder: &Holder, cfg: &Config, now: f64) -> bool {
    !cfg.lease.holder_probe.is_empty()
        && !holder.request.pane.is_empty()
        && holder
            .holder_idle_since
            .is_some_and(|t| now - t >= cfg.lease.holder_idle_minutes * 60.0)
}
fn touch_holder(s: &mut State, run: &str, now: f64) -> Result<()> {
    let holder = s
        .holders
        .iter_mut()
        .find(|h| h.run == run)
        .ok_or_else(|| anyhow::anyhow!("unknown run: {run}"))?;
    ensure!(
        holder.started && holder.stopping_at.is_none(),
        "run is not running: {run}"
    );
    holder.holder_idle_since = holder.holder_idle_since.map(|_| now);
    holder.idle_since = None;
    holder.cpu_usage_usec = None;
    holder.cpu_sample_at = None;
    Ok(())
}
pub fn touch(run: &str) -> Result<()> {
    state::transaction(|s| touch_holder(s, run, manager::now()))?;
    let _ = manager::event(json!({"event":"touch", "run":run}));
    Ok(())
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
                || holder_idle(holder, cfg, now)
                || head
                    .as_ref()
                    .is_some_and(|q| holder.request.level < q.level)
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
                .find(|holder| q.stop_claimed_by.as_deref() == Some(holder.run.as_str()))
                .or_else(|| {
                    candidates()
                        .filter(|holder| {
                            admission::fits_without(s, holder, q, cfg, obs, sample, now)
                        })
                        .min_by_key(|holder| holder.admit_seq)
                        .or_else(|| candidates().min_by_key(|holder| holder.admit_seq))
                })
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
            } else if holder_idle(&h, cfg, now) {
                "holder_idle"
            } else if head.as_ref().is_some_and(|q| h.request.level < q.level) {
                "priority_yield"
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
        } else if holder_idle(&h, cfg, now) {
            "holder_idle"
        } else if h.request.level < q.level {
            "priority_yield"
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
            action.warn = Some((s.holders[index].clone(), reason.into()));
        } else if !h.warned_waiters.contains(&q.enqueue_seq) {
            action.warn = Some((s.holders[index].clone(), reason.into()));
        }
        // History suppresses only warn-only repeats, never a new stop claim.
        if action.warn.is_some() && !h.warned_waiters.contains(&q.enqueue_seq) {
            s.holders[index].warned_waiters.push(q.enqueue_seq);
        }
    }
    if head.is_none() && holder_idle(&h, cfg, now) && !h.holder_idle_warned {
        s.holders[index].holder_idle_warned = true;
        action.warn = Some((s.holders[index].clone(), "holder_idle".into()));
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
        let mut previous_error = None;
        let mut tick = Instant::now();
        let mut last_probe = None;
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
                let activity = if !cfg.lease.holder_probe.is_empty()
                    && last_probe.is_none_or(|at| manager::now() - at >= 60.0)
                {
                    let activity = holder_activity(cfg, &h.request.pane);
                    last_probe = Some(manager::now());
                    Some(activity)
                } else {
                    None
                };
                let sample = stats::read();
                let now = manager::now();
                let level = cfg.priority.level(&h.request.campaign, &h.request.task);
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
                let (obs, query_error) = match manager::observations(&state::read(&state::root())?)
                {
                    Ok(obs) => (obs, None),
                    Err(error) => (BTreeMap::new(), Some(error)),
                };
                let action = state::transaction(|s| {
                    manager::reconcile(s, &obs);
                    // The supervisor owns this live workload even after state.json is reset.
                    if !s.holders.iter().any(|holder| holder.run == run) {
                        s.enqueue_seq = s.enqueue_seq.max(h.request.enqueue_seq);
                        s.admit_seq = s.admit_seq.max(h.admit_seq);
                        s.holders.push(h.clone());
                    }
                    if let Some(activity) = activity {
                        update_holder_activity(
                            s.holders.iter_mut().find(|h| h.run == run).unwrap(),
                            activity,
                            now,
                        );
                    }
                    s.holders
                        .iter_mut()
                        .find(|h| h.run == run)
                        .unwrap()
                        .request
                        .level = level;
                    admission::update_recovery(s, &sample, cfg, now);
                    let fitting_waiter = query_error.is_none()
                        && admission::head(s, &h.request.pool, cfg).is_some_and(|q| {
                            q.campaign != h.request.campaign
                                && admission::fits_without(s, &h, q, cfg, &obs, &sample, now)
                        });
                    if query_error.is_none()
                        && let Some(holder) = s.holders.iter_mut().find(|h| h.run == run)
                    {
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
                    let victim = s
                        .holders
                        .iter()
                        .filter(|h| {
                            h.started
                                && h.stopping_at.is_none()
                                && cfg.pools.get(&h.request.pool).is_some_and(|p| p.evictable)
                        })
                        .min_by_key(|h| (h.request.level, std::cmp::Reverse(h.admit_seq)));
                    let mut action = Action::default();
                    if cfg.pools[&h.request.pool].evictable
                        && victim.is_some_and(|h| h.run == run)
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
                    if query_error.is_none() && action.stop.is_none() && action.warn.is_none() {
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
                if let Some(error) = query_error {
                    report_tick_error(run, &error, &mut previous_error);
                } else {
                    previous_error = None;
                }
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
                    eprintln!("slotr: stopped {}: {}", holder.run, reason);
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
                Err(error) => report_tick_error(run, &error, &mut previous_error),
            }
        }
    })();
    // Retain the record until systemd confirms the unit has gone. A unit still
    // owns descendants after its supervisor exits (KillMode=control-group).
    result
}

fn report_tick_error(run: &str, error: &anyhow::Error, previous: &mut Option<String>) {
    let message = error.to_string();
    if previous.as_ref() != Some(&message) {
        eprintln!("slotr: supervisor tick: {error:#}");
        let _ = manager::event(json!({"event":"supervisor_tick_error","run":run,"error":message}));
        *previous = Some(message);
    }
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
        level: 0,
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
        holder_idle_since: None,
        holder_idle_warned: false,
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

#[cfg(test)]
mod holder_tests {
    use super::*;
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    struct Fixture {
        cfg: Config,
        state: State,
        script: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let script = std::env::temp_dir().join(format!(
                "slotr-probe-{}-{}.sh",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::write(&script, "printf '%s\\n' \"$1\"\nexit \"$2\"\n").unwrap();
            let mut cfg: Config = toml::from_str(include_str!("../config.example.toml")).unwrap();
            cfg.pools.get_mut("default").unwrap().memory_gated = false;
            cfg.lease.grace_seconds = 5.0;
            cfg.lease.waiter_min_wait_seconds = 0.0;
            cfg.lease.holder_idle_minutes = 1.0;
            cfg.lease.holder_probe = vec![
                "sh".into(),
                script.display().to_string(),
                String::new(),
                "0".into(),
            ];
            let holder: Holder = serde_json::from_value(json!({
                "enqueue_seq":1, "pool":"default", "campaign":"holder", "purpose":"synthetic",
                "pane":"test-pane", "cwd":"/tmp", "cost_mib":0, "lease_seconds":1000.0,
                "since":0.0, "seen_at":0.0, "run":"slotr-default-1", "admit_seq":1,
                "slot":0, "admitted_at":0.0, "lease_expires_at":1000.0, "started":true
            }))
            .unwrap();
            Self {
                cfg,
                state: State {
                    holders: vec![holder],
                    ..State::default()
                },
                script,
            }
        }
        fn probe(&mut self, agents: serde_json::Value, now: f64) -> HolderActivity {
            self.cfg.lease.holder_probe[2] = json!({"result":{"agents":agents}}).to_string();
            let activity = holder_activity(&self.cfg, "test-pane");
            update_holder_activity(&mut self.state.holders[0], activity, now);
            activity
        }
        fn status(&mut self, status: &str, now: f64) -> HolderActivity {
            self.probe(json!([{"pane_id":"test-pane", "agent_status":status}]), now)
        }
        fn waiter(&mut self) {
            let mut q = self.state.holders[0].request.clone();
            q.enqueue_seq = 2;
            q.campaign = "waiter".into();
            self.state.queue.push(q);
        }
        fn action(&mut self, now: f64) -> Action {
            for q in &mut self.state.queue {
                q.seen_at = now;
            }
            lease_action(
                &mut self.state,
                "slotr-default-1",
                &self.cfg,
                &BTreeMap::new(),
                &stats::Stats::default(),
                now,
            )
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_file(&self.script).unwrap();
        }
    }

    #[test]
    fn priority_yield_warns_and_stops_one_oldest_fitting_holder() {
        let mut f = Fixture::new();
        f.cfg.lease.holder_probe.clear();
        f.cfg.lease.waiter_min_wait_seconds = 5.0;
        f.cfg.pools.get_mut("default").unwrap().slots = 2;
        let mut newer = f.state.holders[0].clone();
        newer.run = "slotr-default-2".into();
        newer.admit_seq = 2;
        newer.slot = 1;
        newer.request.campaign = "newer".into();
        f.state.holders.push(newer);
        f.waiter();
        f.state.queue[0].level = 1;
        assert!(f.action(4.0).warn.is_none());
        assert_eq!(f.action(5.0).warn.unwrap().1, "priority_yield");
        assert_eq!(
            f.state.queue[0].stop_claimed_by.as_deref(),
            Some("slotr-default-1")
        );
        assert!(
            lease_action(
                &mut f.state,
                "slotr-default-2",
                &f.cfg,
                &BTreeMap::new(),
                &stats::Stats::default(),
                5.0
            )
            .warn
            .is_none()
        );
        assert!(f.action(9.0).stop.is_none());
        assert_eq!(f.action(10.0).stop.unwrap().1, "priority_yield");
        assert_eq!(f.state.holders[1].warned_for, None);
    }

    #[test]
    fn priority_arrival_cancels_claim_and_rewarns_for_new_head() {
        let mut f = Fixture::new();
        f.cfg.lease.holder_probe.clear();
        f.cfg.lease.waiter_min_wait_seconds = 5.0;
        f.state.holders[0].lease_expires_at = Some(1.0);
        f.waiter();
        assert_eq!(f.action(10.0).warn.unwrap().1, "lease_expired");
        let mut priority = f.state.queue[0].clone();
        priority.enqueue_seq = 3;
        priority.campaign = "priority".into();
        priority.level = 1;
        priority.since = 11.0;
        priority.stop_claimed_by = None;
        f.state.queue.push(priority);
        let cancelled = f.action(11.0);
        assert!(cancelled.cancelled.is_some());
        assert!(cancelled.warn.is_none() && cancelled.stop.is_none());
        assert!(f.state.queue[0].stop_claimed_by.is_none());
        assert!(f.action(15.0).warn.is_none());
        assert!(f.action(16.0).warn.is_some());
        assert_eq!(f.state.holders[0].warned_for, Some(3));
        assert!(f.action(20.0).stop.is_none());
        assert!(f.action(21.0).stop.is_some());
        assert!(f.state.queue[0].stop_claimed_by.is_none());
    }

    #[test]
    fn idle_and_gone_warn_then_stop_under_contention() {
        for agents in [
            json!([{"pane_id":"test-pane","agent_status":"idle"}]),
            json!([]),
        ] {
            let mut f = Fixture::new();
            assert_eq!(f.probe(agents.clone(), 0.0), HolderActivity::Idle);
            f.waiter();
            assert!(f.action(59.0).warn.is_none());
            f.probe(agents, 60.0);
            assert_eq!(f.action(60.0).warn.unwrap().1, "holder_idle");
            assert!(f.action(64.0).stop.is_none());
            assert_eq!(f.action(65.0).stop.unwrap().1, "holder_idle");
        }
    }

    #[test]
    fn uncontended_idle_warns_once_per_run_and_never_stops() {
        let mut f = Fixture::new();
        f.status("done", 0.0);
        assert_eq!(f.action(60.0).warn.unwrap().1, "holder_idle");
        assert!(f.action(65.0).stop.is_none());
        assert!(f.action(120.0).warn.is_none());
        f.status("working", 121.0);
        f.status("idle", 122.0);
        assert!(f.action(200.0).warn.is_none());
        f.waiter();
        assert_eq!(f.action(201.0).warn.unwrap().1, "holder_idle");
        assert_eq!(f.action(206.0).stop.unwrap().1, "holder_idle");
    }

    #[test]
    fn working_and_failed_probe_clear_the_idle_clock_and_cancel_reclaim() {
        for failed in [false, true] {
            let mut f = Fixture::new();
            f.status("idle", 0.0);
            f.waiter();
            assert!(f.action(60.0).warn.is_some());
            if failed {
                f.cfg.lease.holder_probe[3] = "1".into();
            }
            assert_eq!(
                f.status("working", 65.0),
                if failed {
                    HolderActivity::Unknown
                } else {
                    HolderActivity::Active
                }
            );
            assert!(f.state.holders[0].holder_idle_since.is_none());
            let action = f.action(65.0);
            assert!(action.stop.is_none());
            assert!(action.warn.is_none());
            assert!(action.cancelled.is_some());
            assert!(f.state.queue[0].stop_claimed_by.is_none());
            f.cfg.lease.holder_probe[3] = "0".into();
            f.status("idle", 70.0);
            assert!(f.action(129.0).warn.is_none());
            assert!(f.action(130.0).warn.is_some());
        }
    }

    #[test]
    fn malformed_listing_missing_pane_and_unknown_status_never_act() {
        let mut f = Fixture::new();
        for agents in [
            json!(null),
            json!([{}]),
            json!([{"pane_id":"test-pane","agent_status":"unexpected"}]),
            json!([
                {"pane_id":"test-pane","agent_status":"idle"}, {"pane_id":"test-pane","agent_status":"working"}
            ]),
        ] {
            assert_eq!(f.probe(agents, 0.0), HolderActivity::Unknown);
            assert!(f.action(120.0).warn.is_none());
        }
        f.cfg.lease.holder_probe[2] = "invalid json".into();
        assert_eq!(
            holder_activity(&f.cfg, "test-pane"),
            HolderActivity::Unknown
        );
        assert_eq!(holder_activity(&f.cfg, ""), HolderActivity::Unknown);
        f.cfg.lease.holder_probe.clear();
        assert_eq!(
            holder_activity(&f.cfg, "test-pane"),
            HolderActivity::Unknown
        );
    }

    #[test]
    fn touch_resets_only_idle_clocks_and_grace_decides_cancellation() {
        let mut f = Fixture::new();
        f.status("idle", 0.0);
        f.waiter();
        assert!(f.action(60.0).warn.is_some());
        f.state.holders[0].idle_since = Some(0.0);
        f.state.holders[0].cpu_usage_usec = Some(42);
        f.state.holders[0].cpu_sample_at = Some(0.0);
        touch_holder(&mut f.state, "slotr-default-1", 64.0).unwrap();
        let h = &f.state.holders[0];
        assert_eq!(h.holder_idle_since, Some(64.0));
        assert_eq!(h.lease_expires_at, Some(1000.0));
        assert!(h.idle_since.is_none() && h.cpu_usage_usec.is_none() && h.cpu_sample_at.is_none());
        assert_eq!(h.warned_at, Some(60.0));
        assert_eq!(h.warned_for, Some(2));
        assert_eq!(
            f.state.queue[0].stop_claimed_by.as_deref(),
            Some("slotr-default-1")
        );
        let action = f.action(65.0);
        assert!(action.stop.is_none());
        assert!(action.cancelled.is_some());
        assert!(f.state.queue[0].stop_claimed_by.is_none());
        assert!(f.action(123.0).warn.is_none());
        assert!(f.action(124.0).warn.is_some());
        f.state.holders[0].stopping_at = Some(125.0);
        assert!(touch_holder(&mut f.state, "slotr-default-1", 126.0).is_err());
        assert!(touch_holder(&mut f.state, "unknown", 126.0).is_err());
        f.state.holders[0].stopping_at = None;
        f.state.holders[0].request.lease_seconds = 0.0;
        f.state.holders[0].lease_expires_at = None;
        touch_holder(&mut f.state, "slotr-default-1", 127.0).unwrap();
        assert!(f.state.holders[0].lease_expires_at.is_none());
    }

    #[test]
    fn repeated_touch_cannot_extend_max_lease_under_contention() {
        let mut f = Fixture::new();
        f.cfg.pools.get_mut("default").unwrap().max_lease = "90m".into();
        assert_eq!(f.cfg.pools["default"].slots, 1);
        f.cfg.lease.holder_idle_minutes = 20.0;
        f.cfg.lease.grace_seconds = 300.0;
        f.state.holders[0].request.lease_seconds = 5400.0;
        f.state.holders[0].lease_expires_at = Some(5400.0);
        f.waiter();
        let mut warnings = 0;
        for step in 0..=95 {
            let now = step as f64 * 60.0;
            update_holder_activity(&mut f.state.holders[0], HolderActivity::Idle, now);
            let action = f.action(now);
            if let Some((_, reason, _)) = action.stop {
                assert_eq!(now, 5400.0 + f.cfg.lease.grace_seconds);
                assert_eq!(reason, "lease_expired");
                assert!(warnings > 1);
                return;
            }
            if action.warn.is_some() {
                warnings += 1;
                touch_holder(&mut f.state, "slotr-default-1", now + 60.0).unwrap();
                assert_eq!(f.state.holders[0].lease_expires_at, Some(5400.0));
                assert_eq!(f.state.holders[0].warned_at, Some(now));
                assert_eq!(
                    f.state.queue[0].stop_claimed_by.as_deref(),
                    Some("slotr-default-1")
                );
            }
        }
        panic!("repeated touch defeated the lease ceiling");
    }

    #[test]
    fn idle_holder_with_an_unfitting_waiter_preserves_contention_reason() {
        let mut f = Fixture::new();
        f.cfg.pools.get_mut("default").unwrap().memory_gated = true;
        f.status("idle", 0.0);
        f.waiter();
        f.state.queue[0].seen_at = 60.0;
        let sample = stats::Stats {
            available_mib: Some(1000.0),
            ..stats::Stats::default()
        };
        let action = lease_action(
            &mut f.state,
            "slotr-default-1",
            &f.cfg,
            &BTreeMap::new(),
            &sample,
            60.0,
        );
        assert_eq!(action.warn.unwrap().1, "overdue_stop_would_not_help");
        assert!(action.stop.is_none());
        assert!(!f.state.holders[0].holder_idle_warned);
        assert!(f.state.queue[0].stop_claimed_by.is_none());
    }

    #[test]
    fn probe_descendant_holding_stdout_cannot_delay_the_tick() {
        let mut f = Fixture::new();
        f.cfg.lease.holder_probe = vec!["sh".into(), "-c".into(),
            "sleep 12 & printf '%s' '{\"result\":{\"agents\":[{\"pane_id\":\"test-pane\",\"agent_status\":\"working\"}]}}'".into()];
        let start = Instant::now();
        assert_eq!(holder_activity(&f.cfg, "test-pane"), HolderActivity::Active);
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn off_and_non_evictable_pools_do_not_warn_or_stop() {
        for policy_off in [true, false] {
            let mut f = Fixture::new();
            f.status("idle", 0.0);
            if policy_off {
                f.cfg.lease.on_expiry = "off".into();
            } else {
                f.cfg.pools.get_mut("default").unwrap().evictable = false;
            }
            assert!(f.action(60.0).warn.is_none());
            f.waiter();
            let action = f.action(65.0);
            assert!(action.warn.is_none() && action.stop.is_none());
        }
    }
}
