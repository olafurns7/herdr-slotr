# Agent setup

This file explains how slotr is installed on a Herdr fleet host and how agents use it there.

## Quick agent setup

```sh
curl -fsSL https://github.com/olafurns7/herdr-slotr/releases/latest/download/install.sh | sh -s -- --config
```

The script downloads its pinned Linux x86_64 release, verifies the checksum
and archive layout, and installs it atomically to `~/.local/bin/slotr`.
It creates `~/.config/slotr/config.toml` from the release's example only
when no config exists, keeps an existing config, then checks that file.
It prints the installed version and tells you when to add `~/.local/bin`
to `PATH`. Running it again replaces the binary.

See [install.md](install.md) for requirements, manual steps, and mirrors.
If `XDG_CONFIG_HOME` or `SLOTR_CONFIG` is set, slotr normally reads another
config path; this installer still creates and checks the file above.
See [configuration.md](configuration.md).

## herdr-setup

herdr-setup installs the pinned release with its own checks.

## Using slotr in a fleet

Configure slotr on each host where you want to use it. A host that runs heavy long-lived runtimes
lists its pools in `~/.config/slotr/config.toml`. Write the wrapped command
into the worker's brief:

```sh
slotr run --pool runtime --campaign <campaign> --purpose "<what for>" \
  --task <lead task id> --pane <lead pane> -- <command>
```

- Exit 75 with a `slotr: stopped RUN: REASON` line on stderr means slotr
  stopped the runtime (pressure, lease, or yield). A configured `on_stop`
  hook can send a note, and `events.jsonl` normally records the reason.
  Queue again when the work still needs it.
- `slotr status` shows holders, the queue, and each waiter's wait reason.
- A lead may wait for admission with
  `herdr pane wait-output <pane> --match 'slotr: admitted'`.
- `slotr run` needs a systemd user bus, so it runs in a Herdr pane, not in a
  sandboxed agent tool shell.
- Heavy one-shot commands (type checks, lint, tests, builds, installs) go
  through the host's heavy pool when it has one:
  `slotr run --pool heavy --kind tsc --campaign <campaign> --purpose "<what for>" -- <command>`.
  See [usage.md](usage.md#heavy-one-shot-commands).
- Caller contract for exit 75 with `slotr: stopped RUN: REASON` on stderr:
  slotr stopped the command, and the result says nothing about the code.
  Re-run it, at most three attempts in all, and report it as "stopped by
  slotr", never as a failed check. Exit 75 without that line is the
  command's own result.

`examples/devbox.toml` shows a `runtime` pool with hooks that send task notes
through `taskr`; see [configuration.md](configuration.md#hooks).
