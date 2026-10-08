# Agent setup

This file explains how slotr is installed on a Herdr fleet host and how agents use it there.

## What the quick agent setup block does

The README's quick agent setup block installs the same pinned release with
the same checks as herdr-setup's installer, then adds a config file:

1. Stops unless the host is Linux x86_64.
2. Stops unless `gh` is installed and `gh auth status` succeeds.
3. Downloads the v0.1.0 musl archive and its `.sha256` file into a
   `mktemp -d` directory.
4. Computes the archive's SHA-256 itself. The `.sha256` file must be exactly
   one line, `HASH  ASSETNAME` (or `HASH *ASSETNAME`), naming this asset.
   It does not use `sha256sum -c`, which would check whatever paths the file
   names. The quick install block also computes the hash itself, but compares
   only the hash and does not check the line count or the name.
5. Checks that the archive holds exactly one member, a regular file named
   `slotr`, and extracts it into an empty directory. A symlink is refused.
6. Writes the binary to a temporary file in `~/.local/bin`, sets mode 0755,
   and renames it to `~/.local/bin/slotr`. A running slotr keeps its old
   binary and no reader sees a partial file.
7. Runs `~/.local/bin/slotr --version`.
8. If `~/.config/slotr/config.toml` does not exist, downloads
   `config.example.toml` from tag v0.1.0 and puts it there. An existing
   config is never overwritten. The example equals the built-in defaults.
9. Runs `slotr config check`, which prints `slotr: config OK`.

A check that uses `fail` prints `slotr setup:` and the reason, and the block
exits 1. Any other command that fails (such as `tar`, `mkdir`, `mktemp`, the
config download, or slotr itself) stops the block with its own message and
exit status.
The block installs even when v0.1.0 is already present.

The block reads and writes `~/.config/slotr/config.toml`. If
`XDG_CONFIG_HOME` or `SLOTR_CONFIG` is set, slotr reads its config from
another path; see [configuration.md](configuration.md).

## herdr-setup

herdr-setup's installer installs the same pinned release (v0.1.0) with the
same checks as the quick agent setup block. It needs `gh` signed in with
access to olafurns7/herdr-slotr; sign in with `gh auth login` or export
`GH_TOKEN`.

## Using slotr in a fleet

Configure slotr on each host where you want to use it. A host that runs heavy long-lived runtimes
lists its pools in `~/.config/slotr/config.toml`. Write the wrapped command
into the worker's brief:

```sh
slotr run --pool runtime --campaign <campaign> --purpose "<what for>" \
  --task <lead task id> --pane <lead pane> -- <command>
```

- Exit 75 means slotr stopped the runtime (pressure, lease, or yield). A
  configured `on_stop` hook can send a note, and `events.jsonl` normally
  records the reason. Queue again when the work still needs it.
- `slotr status` shows holders, the queue, and each waiter's wait reason.
- A lead may wait for admission with
  `herdr pane wait-output <pane> --match 'slotr: admitted'`.
- `slotr run` needs a systemd user bus, so it runs in a Herdr pane, not in a
  sandboxed agent tool shell.
- Heavy checks stay on the host's heavy-command lock; slotr is for runtimes
  that stay up.

`examples/devbox.toml` shows a `runtime` pool with hooks that send task notes
through `taskr`; see [configuration.md](configuration.md#hooks).
