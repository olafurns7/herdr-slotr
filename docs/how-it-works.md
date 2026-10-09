# How it works

This file explains how slotr decides who runs, when it stops a run, and what it stores.

## Admission

A request is admitted when its pool has a free slot and, for memory-gated
pools, both of these hold:

```text
MemAvailable - sum(max(0, holder.cost_mib - holder.anon_mib)) - request.cost_mib >= reserve_mib
PSI full avg60 <= psi_full_avg60_max
```

- In a memory-gated pool, when MemTotal can be read, requests above MemTotal
  minus the reserve are rejected before queueing.
- Unknown anonymous memory gets zero resident credit; file cache gets none.
- Missing memory or PSI data fails closed for admission.
- The optional load gate compares load1 per logical core with
  `load1_per_core_max`. A zero load limit turns that gate off.
- Port probes check both 127.0.0.1 and ::1 when IPv6 is available. Port
  blocks across pools cannot overlap live holders. Probes are not
  reservations.
- Compatibility locks are held by the in-unit supervisor for the workload's
  life. Queued probes release the lock before sleeping, and status reads lock
  owners without acquiring a lock.
- A dead ticket cannot block FIFO; an inactive unit is reclaimed by the next
  active waiter or supervisor.
- Unit names are persisted before startup; unit-name collisions retry with a
  fresh admission sequence.

## Queue order and campaign caps

Within each pool, the effective head has the highest level among tickets
not blocked by the campaign cap. Equal levels stay FIFO by enqueue sequence.
The campaign cap still beats priority. A capped ticket keeps its enqueue time
and reports `campaign_cap`; only another campaign may pass it.
Tickets blocked by memory, PSI, load, slots, ports, locks, or recovery are
never passed over.

For example, if A holds one slot and A2 queues before B, B takes the free
slot ahead of capped A2. A2 is admitted when a slot is free and no other
campaign waits.

Only another campaign below its own cap blocks further admissions. When all
waiting campaigns already hold their caps, a spare slot can still be used.
A waiter whose campaign already holds its cap never earns a lease or yield
stop of another campaign's holder; it waits for a slot to free. This
stop-side rule applies even when no other campaign waits.

A queued ticket refreshes its heartbeat every poll. A stale head (five
seconds plus three polls) earns no stop, even while its flock is still held.

In a memory-gated pool, `priority_wait` holds a waiter while a strictly higher
level waiter can be admitted right now. That higher waiter must be live,
head of its memory-gated pool, and pass slots, memory, PSI, load, recovery,
ports, and its legacy lock. A higher waiter that cannot be admitted holds
nobody. Equal levels do not hold each other across pools.

A campaign name is a claim. Callers can name any campaign; priority does not
authenticate them. A hub can also assign levels by task ID in the optional
priority file. There is no starvation guard; the writer lifts priority.

## Leases, yield, and idle release

- A campaign may use spare slots when no eligible other campaign waits. Its
  newest holders beyond the cap yield when contention appears.
- An overdue lease continues when nobody waits.
- A lower-level holder may yield to its pool's higher-level head with reason
  `priority_yield`. Equal levels and same-campaign waiters never earn this
  reason. Existing waiter age, warning grace, and one-stop claims still apply.
- Under contention only the oldest eligible holder whose release admits the
  head waiter earns a stop. A durable ticket claim permits one stop per
  waiter. Its holder stays the candidate through grace while eligible, even
  if an older holder becomes eligible.
- At grace expiry all conditions are checked again. During grace,
  cancellation is limited to the waiter leaving or losing effective-head
  position; fit is checked at grace expiry. A temporary memory sample during
  grace cannot re-arm the warning.
- Status reports `stopping` as soon as the stop is committed.
- If releasing an overdue holder cannot admit the waiter, the holder only
  warns once for that waiter. `on_expiry = "warn"` never stops it.
- Idle release uses cgroup `cpu.stat` and applies only while a fitting
  waiter exists.
- The optional holder probe samples the run's pane every 60 seconds. An idle,
  done, or gone holder earns one warning without contention, and follows the
  same grace and stop path with contention. Probe errors clear its idle clock.
  Any working sample during grace resets that clock and cancels an idle-only
  warning at grace end. `slotr touch RUN` resets both idle clocks; it never
  renews the lease or clears a warning or stop claim. Holder-idle protection
  requires `holder_idle_minutes * 60 > grace_seconds`.
- Pools with `evictable = false` never stop automatically.

## Memory pressure

On pressure the lowest-level evictable started holder stops itself. Within
that level, the newest admission goes first. A stop records live
stats, holder costs and anonymous memory, and optional lock observations.
After a stop, memory-gated admission waits for the recovery interval
(`recovery_healthy_seconds`). Pools without the memory gate ignore pressure
recovery.

An automatic stop sends TERM, waits `term_grace_seconds`, then KILL; systemd
`TimeoutStopSec` adds 5 seconds. The run exits with code 75.

## A heavy pool

A pool for heavy one-shot commands (type checks, lint, tests, builds,
installs) works like any other pool. The memory budget is shared across
pools: a heavy check and a runtime count against the same available memory.
With priority configured, a non-priority check queues behind a priority
waiter that can be admitted now, and under pressure it is stopped before a
priority holder.

The yield path uses the global lease timings (`waiter_min_wait_seconds` and
`grace_seconds`). They are sized for long runtimes, so a check that finishes
in a few minutes never reaches a `priority_yield`. For short checks priority
acts through queue order and pressure order only. A priority check can still
wait behind two long builds that hold both slots.

A shared legacy lock lets old `flock LOCK CMD` callers keep working during a
transition. An old exclusive caller waits for the slotr holders to leave,
then excludes every slotr waiter, which reports `legacy_lock`. This has two
costs. An old caller ignores priority, so a non-priority `flock` command
blocks a priority slotr run for its whole length. And `flock` has no
fairness, so steady overlapping shared holders can keep an exclusive caller
waiting a long time. Move callers to `slotr run` together, and read the
`legacy_lock` wait reasons in status during the transition.

## Units

Each admitted command runs in a transient systemd user service with its own
supervisor. Units use `OOMScoreAdjust=500`, `OOMPolicy=kill`,
`KillMode=control-group`, memory accounting, no restart, and a bounded stop.
systemd-run receives only valid environment variable names via
`--setenv=NAME`, keeping values off its command line.

## State and events

- State is locked and atomically replaced in
  `${XDG_RUNTIME_DIR:-/run/user/UID}/slotr`.
- Events append to `${XDG_STATE_HOME:-~/.local/state}/slotr/events.jsonl`.
- Neither persists workload argv or environment contents.
- State timing uses CLOCK_BOOTTIME (including suspend); displayed and logged
  timestamps remain UTC RFC 3339. Older wall-time state timestamps are
  accepted and converted on read.
- `status` observes without creating, changing, or deleting files and
  without signalling any process.
- A failed manager, state, or logging tick is reported and retried; it never
  kills a workload or discards a waiting ticket.

## Limits

- A 2-second poll cannot beat a multi-GB burst; the kernel OOM killer remains
  the final protection.
- Port probes are not reservations: applications must use strict port
  binding.
- Detached containers and workloads escaping the user unit are unsupported.
- There is no hosted queue, campaign authentication, auto-restart, or Mac run
  backend.
