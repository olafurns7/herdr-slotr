# Troubleshooting

This file lists the errors slotr and its install steps produce, and what to do about each.

slotr prints its own errors on stderr as `slotr: MESSAGE` and exits 2.

## Install

**`gh` is not signed in.** `gh release download` needs `gh` signed in with
access to olafurns7/herdr-slotr. The agent setup block prints
`slotr setup: gh is missing or not signed in`. Sign in with `gh auth login`
or export `GH_TOKEN`, then run the block again.

**The download fails.** The agent setup block prints
`slotr setup: the release download failed`. Check that your `gh` account can
see olafurns7/herdr-slotr and that release v0.1.0 exists.

**Checksum mismatch.** The quick install block stops before installing and
prints no version line. The manual steps do not print `checksum OK`. The
agent setup block prints
`slotr setup: the checksum did not verify`. Do not install that file;
download it again.

**`slotr: command not found`.** `~/.local/bin` is not on your `PATH`. Run
`~/.local/bin/slotr`, or add `~/.local/bin` to `PATH`.

## Running

**Exit 75.** slotr stopped the workload automatically: memory pressure, an
expired lease, a yield to another campaign, or idle release. The `on_stop`
hook, when one is configured, can send a note, and `events.jsonl` (under
`${XDG_STATE_HOME:-~/.local/state}/slotr/`) normally records the reason.
slotr does not restart it. Queue it again with the original `slotr run`
command when the work still needs it.

**`no systemd user bus here; run it in a terminal with a systemd user session`.**
`run` and `stop` need a reachable systemd user bus. Sandboxed agent shells
and some remote shells have none. Start the command from a terminal in your
user session, or a Herdr pane. On a platform other than Linux, `run` and
`stop` print `run and stop require Linux and a systemd user bus`.

**`unknown pool: NAME`.** The pool is not in the effective config. A
configured `pools` table replaces the built-in `default` pool, so
`--pool default` fails once you define your own pools. Check with
`slotr config show`.

**`unknown kind: NAME`.** `--kind` names a kind the pool does not define
under `kinds`.

**`campaign must not be empty` or `purpose must not be empty`.** Pass
nonempty `--campaign` and `--purpose` text.

**`cost exceeds MemTotal minus admission.reserve_mib`.** The request can
never fit on this machine, so it is rejected before queueing. This check
applies to memory-gated pools when MemTotal can be read. Lower `--cost`,
choose another `--kind`, or lower `admission.reserve_mib`.

**`expected duration, e.g. 4h, 300s or 0`.** A `--lease` or `max_lease`
value is not a valid duration.

**`unknown run: RUN`** from `slotr stop`. Only registered runs can be
stopped. `slotr status` lists them.

**A run waits for a long time.** While it waits, `slotr run` prints
`slotr: waiting: REASON` each time the reason changes, and `slotr status`
shows each waiter's reason. The reasons are `fifo` (another ticket is ahead),
`no_slot`, `campaign_cap`, `memory_budget`, `psi`, `load`, `ports_busy`,
`legacy_lock` (with the holder's pid when known), and `recovery` (waiting
after a stop). `slotr status` alone also shows `ready`: the waiter is first
and the admission checks pass right now. See [how-it-works.md](how-it-works.md).

## Config

`slotr config check` prints `slotr: config OK` or the first error. Errors
name the bad key, for example:

- `config KEY: unknown key`, for example `config pools.NAME.slotz: unknown key`
- `config pools.NAME.slots: must be positive`
- `config pools.NAME.max_lease` followed by the duration error
- `config pools.NAME.ports: invalid block range` (base must be at least
  1024, stride and probe positive, probe at most stride, and
  base + stride * slots at most 65536)
- `config pools.NAME.legacy_lock: invalid path or mode`
- `config watchdog.on_pressure: expected stop, warn or off`, and the same for
  `lease.on_expiry`
- `config pools: at least one pool required`
- `config KEY: invalid environment value` for a bad `SLOTR_...__...`
  override

`config check FILE` fails when FILE does not exist, with
`config FILE: No such file or directory`. Without FILE, a missing config is
fine and the defaults apply.
