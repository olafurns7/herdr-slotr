# slotr

A standalone, host-local admission queue for machine resources. `slotr run`
waits in FIFO order within a pool, accounts for reserved memory, and
runs the admitted command in a transient systemd user service. Each service
contains its own supervisor; there is no daemon or remote service.

Build and install on Linux (Rust 1.99+, edition 2024):

```sh
cargo build --release --locked --target x86_64-unknown-linux-musl
install -m755 target/x86_64-unknown-linux-musl/release/slotr ~/.local/bin/slotr
```

`status` and `config` are portable. `run`, `_supervise`, and `stop` require
Linux, systemd 255.4 or newer (the verified minimum for name-only `--setenv`),
and a reachable systemd user bus. Start workloads from a terminal in
your user session. A sandbox without that bus gets a clear error and exit 2.

```sh
slotr run --pool default --campaign docs --purpose "local preview" -- npm run dev
slotr status
slotr status --json
slotr stop slotr-default-1
slotr config show
slotr config check examples/devbox.toml
```

`--campaign` and `--purpose` are required, nonempty free text. Campaign names
are supplied by the caller; use one consistent name for each campaign.
Optional `--kind NAME` selects a configured cost, or `--cost MIB` supplies it
directly. Otherwise the pool's `default_cost_mib` applies. `--lease 30m`
requests a shorter lease; `max_lease` caps it. Duration suffixes are `s`, `m`,
`h`, and `d`; a bare number is seconds and `"0"` means unlimited. `--task ID`
and `--pane PANE` are opaque strings used only in hook substitutions.

To wrap a development stack, copy `examples/devbox.toml` into your config
location, then have the lead start:

```sh
slotr run --pool runtime --campaign planner-ui --purpose "UI smoke" \
  --task 2987 --pane wN4:p1 -- sh -c \
  'API_PORT=$((SLOTR_PORT_BASE+2)) STUDIO_PORT=$((SLOTR_PORT_BASE+3)) ./start-stack.sh'
```

The workload receives `SLOTR_RUN`, `SLOTR_POOL`, `SLOTR_SLOT`, and, when a port
block is configured, `SLOTR_PORT_BASE`. On admission stderr prints exactly
`slotr: admitted RUN`. Commands and their arguments keep their literal argv
boundaries; slotr itself never invokes a shell.

Configuration is read from `${XDG_CONFIG_HOME:-~/.config}/slotr/config.toml`,
or the path in `SLOTR_CONFIG`. Missing config uses the defaults below.
A configured `pools` table replaces the built-in `default` pool; missing
values within each pool inherit the defaults. Other sections merge with
defaults. Unknown keys, invalid values, and wrong types name the bad key and
exit 2. `config check FILE` requires that file to exist. `config show` emits
JSON containing effective values and each value's source: `default`, `file`,
or `env`.

Every scalar has an environment override: uppercase the full dotted path,
separating its parts with `__`, e.g. `SLOTR_POOLS__RUNTIME__SLOTS=3` or
`SLOTR_LEASE__ON_EXPIRY=warn`. Environment values override the file. Pool and
kind names in environment paths are normalized to lowercase. Hook and
observation arrays can also be overridden with TOML array syntax.

The following reference matches `config.example.toml`, which is also the
built-in default configuration.

| Pool key (`pools.NAME`) | Default | Meaning |
| --- | --- | --- |
| `slots` | `1` | Concurrent holders in this pool, positive integer |
| `memory_gated` | `true` | Apply the memory, PSI, and optional load gates |
| `evictable` | `true` | Allow pressure, lease, yield, or idle stops |
| `default_cost_mib` | `7680` | Estimated anonymous memory cost per request |
| `max_lease` | `"4h"` | Lease default and upper bound; `"0"` has no cap |
| `campaign_cap` | `1` | Slots per campaign when another campaign waits; 0 disables |
| `ports.base` | absent | Optional first block's starting port, at least 1024 |
| `ports.stride` | absent | Optional block width; example 32 |
| `ports.probe` | absent | Optional first N ports to bind-probe; example 7 |
| `legacy_lock.path` | absent | Optional compatibility flock file |
| `legacy_lock.mode` | absent | `shared` or `exclusive`; both are nonblocking probes |
| `kinds.K.cost_mib` | absent | Optional named request cost |

Optional port tables default to base 31000, stride 32, and probe 7 when enabled.
A legacy lock requires a path; its mode defaults to `shared`.

| Admission key (`admission`) | Default | Meaning |
| --- | --- | --- |
| `reserve_mib` | `5120` | Memory that must remain after admission |
| `psi_full_avg60_max` | `5.0` | Maximum full-memory PSI avg60 on admission |
| `load1_per_core_max` | `0.0` | Maximum load1 / logical cores; 0 disables |
| `recovery_healthy_seconds` | `30.0` | Continuous observed health required after a stop |
| `queue_poll_ms` | `1000` | Positive waiter polling interval |

| Watchdog key (`watchdog`) | Default | Meaning |
| --- | --- | --- |
| `interval_ms` | `2000` | Positive supervisor sampling interval |
| `on_pressure` | `"stop"` | `stop`, `warn`, or `off` |
| `stop_available_mib` | `4000` | Stop below this available memory after N samples |
| `stop_available_samples` | `2` | Positive consecutive low-memory sample count |
| `emergency_available_mib` | `2000` | Stop below this after one sample |
| `stop_psi_full_avg10_min` | `20.0` | Stop at or above this PSI avg10 after N samples |
| `stop_psi_samples` | `5` | Positive consecutive PSI sample count |
| `term_grace_seconds` | `15.0` | Positive TERM-to-KILL grace; systemd TimeoutStopSec adds 5 s |
| `observe_locks` | `[]` | Optional paths probed with shared nonblocking flock at stop |

| Lease key (`lease`) | Default | Meaning |
| --- | --- | --- |
| `on_expiry` | `"stop"` | `stop`, `warn`, or `off` for leases, yield, and idle |
| `grace_seconds` | `300.0` | Warning grace before contention stops |
| `waiter_min_wait_seconds` | `300.0` | Minimum head-waiter age before a warning |
| `idle_release_minutes` | `0.0` | Idle release interval; 0 disables |
| `idle_cpu_ms_per_min` | `100.0` | cgroup CPU usage rate below which a holder is idle |

| Hook key (`hooks`) | Default | Meaning |
| --- | --- | --- |
| `on_admit` | `[]` | argv run once after workload startup |
| `on_warn` | `[]` | argv run on a pressure or contention warning |
| `on_stop` | `[]` | argv run after TERM, during an automatic stop's grace |

Hooks run outside the state lock with a 20-second timeout and no retries.
Each argv element substitutes `{run}`, `{pool}`, `{campaign}`, `{purpose}`,
`{task}`, `{pane}`, `{reason}`, `{slot}`, `{port_base}`, and `{events}`.
Replacement text stays literal, even if it contains placeholder syntax.
A hook mentioning `{task}` or `{pane}` is skipped when that value is empty.
Hook exits and failures are logged without hook argv or environment values.
To send both a task note and a pane prompt, write a small wrapper script that
runs both commands and configure its argv as the single hook, for example
`on_stop = ["/path/to/notify-both", "{task}", "{pane}", "slotr: stopped {run}: {reason}"]`.
The script takes task, pane, and message as separate arguments and invokes
`taskr note --as "$1" "$3"` followed by
`herdr agent prompt "$2" --text "$3"`. The devbox
example shows each command separately; neither integration is compiled in.
The on-stop example includes a requeue template; replace PURPOSE and COMMAND
with your original request, which slotr deliberately does not persist.

Admission requires a free pool slot and, for memory-gated pools:

```text
MemAvailable - sum(max(0, holder.cost_mib - holder.anon_mib)) - request.cost_mib >= reserve_mib
PSI full avg60 <= psi_full_avg60_max
```

Requests above MemTotal minus the reserve are rejected before queueing.
Port probes check both 127.0.0.1 and ::1 when IPv6 is available.
Unknown anonymous memory gets zero resident credit; file cache gets none.
Missing memory or PSI data fails closed for admission. A zero load limit
turns that gate off. Port blocks across pools cannot overlap live holders.
Compatibility locks are held by the in-unit supervisor for the workload's
life; queued probes release the lock before sleeping, and status reads lock
owners without acquiring a lock. A dead ticket cannot block FIFO; an inactive
unit is reclaimed by the
next active waiter or supervisor. Unit names are persisted before startup;
unit-name collisions retry with a fresh admission sequence.

Within each pool, the effective head is the oldest live ticket that is not
blocked by the campaign cap. A capped ticket keeps its enqueue time and
position and reports `campaign_cap`; only another campaign may pass it.
Tickets blocked by memory, PSI, load, slots, ports, locks, or recovery are
never passed over. For example, if A holds one slot and A2 queues before B,
B takes the free slot ahead of capped A2; A2 is admitted when a slot is free
and no other campaign waits.

Only another campaign below its own cap blocks further admissions. When all
waiting campaigns already hold their caps, a spare slot can still be used.
A waiter whose campaign already holds its cap never earns a lease or yield
stop of another campaign's holder; it waits for a slot to free. This stop-side
rule applies even when no other campaign waits. A queued ticket refreshes
its heartbeat every poll; a stale head (five seconds plus three polls) earns
no stop, even while its flock is still held.

A campaign may use spare slots when no eligible other campaign waits. Its
newest holders beyond the cap yield when contention appears. An overdue lease
continues when nobody waits. Under contention only the oldest eligible
holder whose release admits the head waiter earns a stop. A durable ticket
claim permits one stop per waiter. Its holder stays the candidate through grace
while eligible, even if an older holder becomes eligible. At grace expiry all conditions are checked
again. During grace, cancellation is limited to the waiter leaving or losing
effective-head position; fit is checked at grace expiry. A temporary memory
sample during grace cannot re-arm the warning. Status reports `stopping` as
soon as the stop is committed.
If releasing an overdue holder cannot admit the waiter, the holder only
warns once for that waiter. `on_expiry = "warn"` never stops it.
Idle release uses cgroup `cpu.stat` and applies only while a fitting waiter
exists. Pools with `evictable = false` never stop automatically.

On pressure the newest evictable holder stops itself. A stop records live
stats, holder costs and anonymous memory, and optional lock observations;
memory-gated admission waits for the recovery interval. Pools without the
memory gate ignore pressure recovery. State is locked and atomically
replaced in `${XDG_RUNTIME_DIR:-/run/user/UID}/slotr`. Events append to
`${XDG_STATE_HOME:-~/.local/state}/slotr/events.jsonl`. Neither persists
workload argv or environment contents. systemd-run receives only valid
environment variable names via --setenv=NAME, keeping values off its command
line. State timing uses CLOCK_BOOTTIME (including suspend); displayed and
logged timestamps remain UTC RFC 3339. Older wall-time state timestamps are
accepted and converted on read. `status` observes without creating,
changing, or deleting files and without signalling any process.

Exit codes: normal workload codes are relayed; signals return 128 + signal;
2 means CLI/config/platform/manager failure; 75 means an automatic slotr
stop. Explicit `stop RUN` targets only a registered run and uses systemd to
stop the complete unit.

Limits: a 2-second poll cannot beat a multi-GB burst; the kernel OOM killer
remains the final protection. Units use `OOMScoreAdjust=500`, `OOMPolicy=kill`,
`KillMode=control-group`, memory accounting, no restart, and a bounded stop.
Port probes are not reservations: applications must use strict port binding.
Detached containers and workloads escaping the user unit are unsupported.
There is no hosted queue, campaign authentication, auto-restart, or Mac run
backend. A warning hook can delay that supervisor by up to its timeout.
A failed manager/state/logging tick is reported and retried; it never kills a
workload or discards a waiting ticket. Stop hooks run after TERM and alongside
the grace timer, so a slow hook cannot delay TERM or KILL.

Validate locally (Python 3.11+ stdlib drives the fake-systemd integration tests):

```sh
cargo fmt --check
flock /tmp/trip-heavy.lock cargo clippy --all-targets --locked -- -D warnings
flock /tmp/trip-heavy.lock cargo test --locked
flock /tmp/trip-heavy.lock cargo build --release --locked --target x86_64-unknown-linux-musl
file target/x86_64-unknown-linux-musl/release/slotr
```

Only `SLOTR_TEST=1` enables test seams: `SLOTR_PROC_ROOT`, `SLOTR_CGROUP_ROOT`,
`SLOTR_SYSTEMD_RUN`, `SLOTR_SYSTEMCTL`, and `SLOTR_CLOCK_OFFSET_FILE` (seconds
added to the real wall clock). Production ignores all of them.

Local releases use `sh scripts/release.sh VERSION` on a clean Linux x86_64
checkout. VERSION must match Cargo.toml. It refuses dirty trees, existing local `vVERSION` tags, and other
hosts (exit 2). Assets in `dist/` are
`slotr-VERSION-x86_64-unknown-linux-musl.tar.gz` and its `.sha256` file.
`--publish` explicitly creates a GitHub release or uploads to an existing
one using `gh`, targeting the exact local HEAD that was built. There are no GitHub Actions workflows.
