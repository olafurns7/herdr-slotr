# Configuration

This file explains where slotr reads its config, how to check it, and what every setting means.

## Where the config lives

slotr reads `${XDG_CONFIG_HOME:-~/.config}/slotr/config.toml`, or the path in
`SLOTR_CONFIG`. A missing config uses the built-in defaults below.
`config.example.toml` in this repository is the built-in default
configuration; copy it to start your own. `examples/devbox.toml` is a larger
example with a `runtime` pool, ports, a legacy lock, and hooks.

## Check and show

```sh
slotr config check
slotr config check examples/devbox.toml
slotr config show
```

- `config check` loads the config and prints `slotr: config OK`.
  `config check FILE` checks that file instead, and requires it to exist.
- `config show` emits JSON containing effective values and each value's
  source: `default`, `file`, or `env`.

Unknown keys, invalid values, and wrong types name the bad key and exit 2.

## How values combine

- A configured `pools` table replaces the built-in `default` pool; missing
  values within each pool inherit the defaults.
- Other sections merge with defaults.
- Environment values override the file.

## Environment overrides

Every scalar has an environment override: uppercase the full dotted path,
separating its parts with `__`, for example:

```sh
SLOTR_POOLS__RUNTIME__SLOTS=3
SLOTR_LEASE__ON_EXPIRY=warn
```

Pool and kind names in environment paths are normalized to lowercase. Hook,
observation, and priority campaign arrays use TOML array syntax.
For example, `SLOTR_PRIORITY__CAMPAIGNS='["team-a", "team-b-*"]'` and
`SLOTR_PRIORITY__FILE=~/priority` override both priority settings.

## Reference

These tables match `config.example.toml`. Durations use the suffixes `s`,
`m`, `h`, and `d`; a bare number is seconds.

### Pools

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

Optional port tables default to base 31000, stride 32, and probe 7 when
enabled. A legacy lock requires a path; its mode defaults to `shared`.

### Priority

| Priority key (`priority`) | Default | Meaning |
| --- | --- | --- |
| `campaigns` | `[]` | Exact campaign names or prefixes ending in `*`; matches get level 1 |
| `file` | `""` | Optional level file; empty disables it; `~` expands to the home directory |

The file uses one rule per line:

```text
# Higher numbers go first.
campaign team-a 1
campaign team-b-* 2
task 123 3
```

Levels are unsigned 32-bit integers. A run gets the highest matching level
from the static list and file. No match means level 0. Task IDs match exactly.
Blank lines and `#` comments are ignored. Malformed or truncated lines are
skipped; other lines still apply. A missing or unreadable file adds no levels.
There is no age limit. Waiters refresh each poll; holders refresh each watchdog
tick. Status shows stored levels and the current file state.

Writers must write a temp file beside the priority file, then rename it over
the destination. This atomic rename prevents readers seeing a partial update.
Remove the file or its rules to lift file priority. Static matches still apply.

### Admission

| Admission key (`admission`) | Default | Meaning |
| --- | --- | --- |
| `reserve_mib` | `5120` | Memory that must remain after admission |
| `psi_full_avg60_max` | `5.0` | Maximum full-memory PSI avg60 on admission |
| `load1_per_core_max` | `0.0` | Maximum load1 / logical cores; 0 disables |
| `recovery_healthy_seconds` | `30.0` | Continuous observed health required after a stop |
| `queue_poll_ms` | `1000` | Positive waiter polling interval |

### Watchdog

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

### Lease

| Lease key (`lease`) | Default | Meaning |
| --- | --- | --- |
| `on_expiry` | `"stop"` | `stop`, `warn`, or `off` for leases, yield, and idle |
| `grace_seconds` | `300.0` | Warning grace before contention stops |
| `waiter_min_wait_seconds` | `300.0` | Minimum head-waiter age before a warning |
| `idle_release_minutes` | `0.0` | Idle release interval; 0 disables |
| `idle_cpu_ms_per_min` | `100.0` | cgroup CPU usage rate below which a holder is idle |
| `holder_probe` | `[]` | argv listing holder agents; empty disables probing; fleet example `["herdr", "agent", "list"]` |
| `holder_idle_minutes` | `20.0` | Continuous observed holder inactivity before warning or contention reclaim |

When enabled, each supervisor probes every 60 seconds, outside the state lock,
with a 5-second timeout. It matches the run's `--pane` against
`result.agents[].pane_id` and reads `agent_status`. `idle`, `done`, or absence
from a successful listing counts as inactivity. Working agents reset the
clock. Failed or malformed probes are unknown and clear the clock, so they
cannot trigger holder reclaim. A run without `--pane` is unknown.

An inactive holder gets reason `holder_idle`. Without a waiter, it gets one
`on_warn` per run and continues running. Under contention it follows the
existing warning, `grace_seconds`, and exit-75 stop path. `on_expiry = "off"`
and non-evictable pools disable these actions. Any working sample during the
grace clears the holder-idle clock and cancels an idle-only warning when the
grace check runs. Use `slotr touch RUN` before grace expires to reset the idle
clocks; it never renews the lease or clears a pending warning or stop claim.
The grace check decides whether reclaim is still eligible. Touch defends
against idle reclaim only: overdue leases and campaign yield can still stop.
For holder-idle protection, keep `holder_idle_minutes * 60 > grace_seconds`.
The warning hook text should name both actions: `slotr touch {run}` to keep
an in-use stack against idle reclaim, or `slotr stop {run}` to release it.

### Hooks

| Hook key (`hooks`) | Default | Meaning |
| --- | --- | --- |
| `on_admit` | `[]` | argv run once after workload startup |
| `on_warn` | `[]` | argv run on a pressure or contention warning |
| `on_stop` | `[]` | argv run after TERM, during an automatic stop's grace |

Each hook is an argv array; no shell interprets it. Hooks run outside the
state lock with a 20-second timeout and no retries.
Each argv element substitutes `{run}`, `{pool}`, `{campaign}`, `{purpose}`,
`{task}`, `{pane}`, `{reason}`, `{slot}`, `{port_base}`, and `{events}`.
Replacement text stays literal, even if it contains placeholder syntax.
A hook mentioning `{task}` or `{pane}` is skipped when that value is empty.
Hook exits and failures are logged without hook argv or environment values.

A warning hook can delay that supervisor by up to its timeout. Stop hooks run
after TERM and alongside the grace timer, so a slow hook cannot delay TERM or
KILL.

To wake the agent that asked for the run, prompt its pane:

```toml
on_stop = ["herdr", "agent", "prompt", "{pane}", "slotr: {run} {reason}"]
```

This form works for any task id on the host, needs no taskr environment, and
fails harmlessly (a logged non-zero exit) when the pane is gone. The devbox
example uses it for all three hooks.

Closing the terminal tab that runs `slotr run` sends the client SIGHUP. The
client stops its unit and exits 129, even when `systemd-run` exits on the same
HUP first. The unit itself runs outside the tab and never sees that HUP, so
only the `slotr run` client relays it. A client started under `nohup` keeps
its inherited HUP ignore, and a client in another session gets no tab HUP;
stop either with `slotr stop RUN`.

If you also want a ledger note, use `taskr note` without `--as`:

```toml
on_stop = ["taskr", "note", "slotr: stopped {run}: {reason}"]
```

The unit forwards the caller's environment, so the note lands on the caller's
own `TASKR_TASK` (it fails once that task is done). Do not use `taskr note --as {task}`: taskr accepts `--as`
only for a root orchestrator on its own host, so the hook exits 6 whenever
`--task` is a worker lane id or a root on another host.

To send both, write a small wrapper script that runs both commands and
configure its argv as the single hook, for example
`["/path/to/notify-both", "{pane}", "slotr: stopped {run}: {reason}"]`, which
invokes `herdr agent prompt "$1" "$2"` followed by `taskr note "$2"`.
Neither integration is compiled in.
The on-stop example includes a requeue template; replace PURPOSE and COMMAND with your original request, which slotr deliberately does not
persist.
