# Usage

This file explains the slotr commands, their options, and what a workload sees when it runs.

## Commands

```sh
slotr run --pool default --campaign docs --purpose "local preview" -- npm run dev
slotr status
slotr status --json
slotr stop slotr-default-1
slotr config show
slotr config check examples/devbox.toml
slotr --version
```

- `run` waits in FIFO order within a pool, accounts for reserved memory, and
  runs the admitted command in a transient systemd user service. Each service
  contains its own supervisor; there is no daemon or remote service.
- `status` shows holders and the queue. `--json` prints JSON. It observes
  without creating, changing, or deleting files and without signalling any
  process.
- `stop RUN` targets only a registered run and uses systemd to stop the
  complete unit.
- `config show` and `config check` are described in
  [configuration.md](configuration.md).

`status` and `config` are portable. `run`, `stop`, and the internal
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
- `--task ID` and `--pane PANE` are opaque strings used only in hook
  substitutions.
- The command follows `--`. Commands and their arguments keep their literal
  argv boundaries; slotr itself never invokes a shell.

## What the workload sees

The workload receives these environment variables:

- `SLOTR_RUN`
- `SLOTR_POOL`
- `SLOTR_SLOT`
- `SLOTR_PORT_BASE`, when a port block is configured

On admission stderr prints exactly `slotr: admitted RUN`.

## Example: a development stack

To wrap a development stack, copy `examples/devbox.toml` into your config
location, then have the lead start:

```sh
slotr run --pool runtime --campaign my-feature --purpose "UI smoke" \
  --task 123 --pane w1:p1 -- sh -c \
  'PORT=$((SLOTR_PORT_BASE+0)) ./start-dev.sh'
```

Port probes are not reservations: applications must use strict port binding.

## Exit codes

- Normal workload exit codes are relayed.
- A signal returns 128 + the signal number.
- 2 means a CLI, config, platform, or manager failure.
- 75 means slotr stopped the workload automatically (pressure, lease, yield,
  or idle release).
