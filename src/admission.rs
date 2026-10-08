use crate::{
    config::{Config, Pool},
    manager::Observation,
    state::{Holder, Request, State},
    stats::Stats,
};
use std::collections::BTreeMap;

pub fn outstanding(s: &State, obs: &BTreeMap<String, Observation>) -> f64 {
    s.holders
        .iter()
        .map(|h| {
            (h.request.cost_mib as f64 - obs.get(&h.run).and_then(|o| o.anon_mib).unwrap_or(0.0))
                .max(0.0)
        })
        .sum()
}
pub fn healthy(sample: &Stats, cfg: &Config) -> bool {
    sample
        .available_mib
        .is_some_and(|v| v >= cfg.watchdog.stop_available_mib as f64)
        && sample
            .psi_full_avg10
            .is_some_and(|v| v < cfg.watchdog.stop_psi_full_avg10_min)
        && sample
            .psi_full_avg60
            .is_some_and(|v| v <= cfg.admission.psi_full_avg60_max)
}
pub fn update_recovery(s: &mut State, sample: &Stats, cfg: &Config, now: f64) {
    if s.last_stop.is_some() {
        if !healthy(sample, cfg) {
            s.healthy_since = None;
        } else if s.healthy_since.is_none() {
            s.healthy_since = Some(now);
        }
    }
}
pub fn recovery(s: &State, sample: &Stats, cfg: &Config, now: f64) -> bool {
    s.last_stop.as_ref().is_some_and(|stop| {
        !healthy(sample, cfg)
            || s.healthy_since
                .is_none_or(|t| now - t < cfg.admission.recovery_healthy_seconds)
            || now - stop.at < cfg.admission.recovery_healthy_seconds
    })
}
pub fn cap_blocked(s: &State, q: &Request, cfg: &Config) -> bool {
    let pool = &cfg.pools[&q.pool];
    pool.campaign_cap > 0
        && s.holders
            .iter()
            .filter(|h| h.request.pool == q.pool && h.request.campaign == q.campaign)
            .count()
            >= pool.campaign_cap as usize
        && s.queue
            .iter()
            .any(|w| w.pool == q.pool && w.campaign != q.campaign)
}
pub fn head<'a>(s: &'a State, pool: &str, cfg: &Config) -> Option<&'a Request> {
    s.queue
        .iter()
        .find(|q| q.pool == pool && !cap_blocked(s, q, cfg))
}
pub fn decide(
    s: &State,
    q: &Request,
    cfg: &Config,
    obs: &BTreeMap<String, Observation>,
    sample: &Stats,
    now: f64,
    ports_free: impl Fn(u32) -> bool,
) -> (Option<u32>, Option<&'static str>) {
    let pool = &cfg.pools[&q.pool];
    if cap_blocked(s, q, cfg) {
        return (None, Some("campaign_cap"));
    }
    if recovery(s, sample, cfg, now) {
        return (None, Some("recovery"));
    }
    let slots: Vec<_> = (0..pool.slots)
        .filter(|slot| {
            !s.holders
                .iter()
                .any(|h| h.request.pool == q.pool && h.slot == *slot)
        })
        .collect();
    if slots.is_empty() {
        return (None, Some("no_slot"));
    }
    if pool.memory_gated {
        if sample.available_mib.is_none_or(|v| {
            v - outstanding(s, obs) - (q.cost_mib as f64) < cfg.admission.reserve_mib as f64
        }) {
            return (None, Some("memory_budget"));
        }
        if sample
            .psi_full_avg60
            .is_none_or(|v| v > cfg.admission.psi_full_avg60_max)
        {
            return (None, Some("psi"));
        }
        if cfg.admission.load1_per_core_max > 0.0
            && sample
                .load1
                .is_none_or(|v| v / sample.cores as f64 > cfg.admission.load1_per_core_max)
        {
            return (None, Some("load"));
        }
    }
    for slot in slots {
        if let Some(p) = &pool.ports {
            let base = p.base + slot * p.stride;
            let end = base + p.stride - 1;
            if s.holders.iter().any(|h| {
                h.port_base
                    .zip(h.port_end)
                    .is_some_and(|(b, e)| base <= e && end >= b)
            }) {
                continue;
            }
            if !ports_free(slot) {
                continue;
            }
        }
        return (Some(slot), None);
    }
    (None, Some("ports_busy"))
}
pub fn yielding(h: &Holder, s: &State, pool: &Pool) -> bool {
    pool.campaign_cap > 0
        && s.queue
            .iter()
            .any(|q| q.pool == h.request.pool && q.campaign != h.request.campaign)
        && s.holders
            .iter()
            .filter(|other| {
                other.request.pool == h.request.pool
                    && other.request.campaign == h.request.campaign
                    && other.admit_seq < h.admit_seq
            })
            .count()
            >= pool.campaign_cap as usize
}
pub fn overdue(h: &Holder, now: f64) -> bool {
    h.lease_expires_at.is_some_and(|t| now >= t)
}
pub fn holder_state(h: &Holder, s: &State, cfg: &Config, now: f64) -> &'static str {
    if h.warned_at.is_some() {
        "warned"
    } else if yielding(h, s, &cfg.pools[&h.request.pool]) {
        "yielding"
    } else if overdue(h, now) {
        "overdue"
    } else {
        "running"
    }
}
pub fn fits_without(
    s: &State,
    h: &Holder,
    q: &Request,
    cfg: &Config,
    obs: &BTreeMap<String, Observation>,
    sample: &Stats,
    now: f64,
) -> bool {
    let mut without = s.clone();
    without.holders.retain(|other| other.run != h.run);
    // Releasing a holder may uncap an older ticket. Do not evict it for a
    // waiter that would immediately lose its effective-head position.
    if head(&without, &q.pool, cfg).is_none_or(|first| first.enqueue_seq != q.enqueue_seq) {
        return false;
    }
    let mut sample = sample.clone();
    if let Some(available) = &mut sample.available_mib {
        *available += obs.get(&h.run).and_then(|o| o.anon_mib).unwrap_or(0.0);
    }
    let legacy_ok = cfg.pools[&q.pool].legacy_lock.as_ref().is_none_or(|l| {
        l.mode == "shared" || !without.holders.iter().any(|h| h.request.pool == q.pool)
    });
    legacy_ok
        && decide(&without, q, cfg, obs, &sample, now, |slot| {
            slot == h.slot
                || cfg.pools[&q.pool]
                    .ports
                    .as_ref()
                    .is_none_or(|p| crate::manager::ports_free(p.base + slot * p.stride, p.probe))
        })
        .1
        .is_none()
}
