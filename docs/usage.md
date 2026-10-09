# Usage

This file explains the slotr commands, their options, and what a workload sees when it runs.

## Commands

```sh
slotr run --pool default --campaign docs --purpose "local preview" -- npm run dev
slotr status
slotr status --json
slotr stop slotr-default-1
slotr touch slotr-default-1
slotr config show
slotr config check examples/devbox.toml
slotr --version
```

- `run` waits in priority order, then FIFO within each level in a pool. It
  accounts for reserved memory and runs the admitted command in a transient
  systemd user service. Each service
  contains its own supervisor; there is no daemon or remote service.
- `status` shows holders and the queue. `--json` prints JSON. It observes
  without creating, changing, or deleting files and without signalling any
  process.
- `stop RUN` targets only a registered run and uses systemd to stop the
  complete unit.
- `touch RUN` resets a running holder's inactivity and CPU-idle clocks,
  defending against idle reclaim only. It never renews the lease or clears
  a pending warning or stop claim; eligibility is checked at grace end.
  Overdue leases, pressure and campaign yield still apply. It cannot revive
  a run already stopping. A `touch` event is appended to `events.jsonl` when
  event logging is available.
- `config show` and `config check` are described in
  [configuration.md](configuration.md).

`status` and `config` are portable. `run`, `stop`, `touch`, and the internal
`_supervise` command need Linux and a systemd user bus; see
[install.md](install.md#requirements).

## Options for `run`

- `--campaign` and `--purpose` are required, nonempty free text. Campaign
  names are supplied by the caller; use one consistent name for each campaign.
- `--pool NAME` selects the pool. The default is `default`.
- `--kind NAME` selects a configured cost, or `--cost MIB` supplies it
  directly. Otherwise the pool's `default_cost_mib` applies. You cannot pass
  both `--kind` and `--cost`.
- `--lease 30m` requests a shorter lease; the pool's `max_lease` caps it.
  Duration suffixes are `s`, `m`, `h`, and `d`; a bare number is seconds and
  `"0"` means unlimited.
- `--task ID` is an opaque string used in hook substitutions and exact
  priority-file matches. `--pane PANE`
  also identifies the holder for the optional liveness probe. Pass the pane
  ID (`--pane "$HERDR_PANE_ID"`) on the same Herdr server, not an agent name.
  Moving a pane changes its ID; the original ID then reads as gone.
- The command follows `--`. Commands and their arguments keep their literal
  argv boundaries; slotr itself never invokes a shell.

## What the workload sees

The workload receives these environment variables:

- `SLOTR_RUN`
- `SLOTR_POOL`
- `SLOTR_SLOT`
- `SLOTR_PORT_BASE`, when a port block is configured

On admission stderr prints exactly `slotr: admitted RUN`.
Every automatic stop prints `slotr: stopped RUN: REASON` on stderr and
returns exit code 75. Use the reason when deciding when to rerun the command.

Status includes `level` for each holder and queue entry in text and JSON.
Each pool's queue is listed in effective order: uncapped tickets first,
highest level first, then enqueue sequence. The `priority` block reports
file `state` as `off`, `ok` with `age_seconds`, or `missing` for an unreadable
or absent file. Levels are the last values refreshed by each run.

## Example: a development stack

To wrap a development stack, copy `examples/devbox.toml` into your config
location, then have the lead start:

```sh
slotr run --pool runtime --campaign my-feature --purpose "UI smoke" \
  --task 123 --pane "$HERDR_PANE_ID" -- sh -c \
  'PORT=$((SLOTR_PORT_BASE+0)) ./start-dev.sh'
```

Port probes are not reservations: applications must use strict port binding.

## Heavy one-shot commands

Type checks, lint, tests, builds, and installs can run through a heavy pool,
such as the `heavy` pool in `examples/devbox.toml`. They then queue by memory
and priority, and the watchdog can stop a low-priority check before a
priority runtime.

```sh
slotr run --pool heavy --kind tsc --campaign team-a --purpose "type check" \
  -- pnpm tsc --noEmit
```

- Output and the exit code pass through, and the working directory is kept.
  A script reads the result as it would without slotr.
- Exit 75 with a `slotr: stopped RUN: REASON` line on stderr means slotr
  stopped the command. It is not a failure of the check. Run it again.
- Installs and builds are stopped like any other kind. Re-run them after a
  slotr stop; follow the tool's recovery steps if it left partial outputs.

Run at most three attempts:

This example retries every exit 75, including a workload's own. Check each
attempt's stop line before reporting it as stopped by slotr.

```sh
n=1
while :; do
  rc=0
  slotr run --pool heavy --kind tsc --campaign team-a --purpose "type check" \
    -- pnpm tsc --noEmit || rc=$?
  if [ "$rc" -ne 75 ] || [ "$n" -ge 3 ]; then break; fi
  n=$((n + 1))
done
# $rc is the result; classify 75 as stopped only with the stop line.
```

Never retry without a cap. Under sustained pressure an uncapped loop is
restarted and stopped again every recovery window.

## Exit codes

- Normal workload exit codes are relayed.
- A signal returns 128 + the signal number.
- 2 means a CLI, config, platform, or manager failure.
- 75 means slotr stopped the workload automatically (pressure, lease, yield,
  or idle release) when stderr also has the `slotr: stopped RUN: REASON`
  line. A workload's own exit 75 is relayed like any other code.
