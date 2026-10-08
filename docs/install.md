# Install

Install from a release or build from source.

## Requirements

- Linux x86_64.
- `curl`, `tar`, and `sha256sum`.
- To use `run`, `stop`, and the internal `_supervise` command: systemd 255.4
  or newer (the verified minimum for name-only `--setenv`) and a reachable
  systemd user bus. Start workloads from a terminal in your user session.
  A sandbox without that bus gets a clear error and exit 2.
- `status` and `config` need no systemd.

## Install a release

```sh
curl -fsSL https://github.com/olafurns7/herdr-slotr/releases/latest/download/install.sh | sh
```

The script:

- Installs the exact version pinned in its `VERSION` line, currently 0.1.2.
- Downloads the archive and checksum into a temporary directory.
- Computes the hash itself and requires one checksum line naming that archive.
- Accepts only regular files, exactly one `slotr`, and optional `LICENSE`
  and `THIRD-PARTY-NOTICES`, without duplicates. Extracts only `slotr`
  into an empty directory and refuses symlinks.
- Writes a temporary binary beside `~/.local/bin/slotr`, sets mode 0755,
  and renames it into place. Prints the version and install location.
- Removes temporary files on exit and reports failures as `slotr install: REASON`.

Add `--config` with `sh -s -- --config`, or run `sh install.sh --config`
after downloading the script. It creates `~/.config/slotr/config.toml`
from the release's `config.example.toml` only when absent, then checks that
file. It never overwrites an existing config.

When `~/.local/bin` is outside `PATH`, the script prints the command to add it.
Rerun the install command to update.

## Manual install

These steps do by hand what the script does. Run them in one shell and stop
if a command fails.

1. Download the archive and its checksum into an empty directory.

   ```sh
   cd "$(mktemp -d)"
   curl -fsSLO https://github.com/olafurns7/herdr-slotr/releases/download/v0.1.2/slotr-0.1.2-x86_64-unknown-linux-musl.tar.gz
   curl -fsSLO https://github.com/olafurns7/herdr-slotr/releases/download/v0.1.2/slotr-0.1.2-x86_64-unknown-linux-musl.tar.gz.sha256
   ```

2. Verify the download. Go on only when it prints `OK`.

   ```sh
   sha256sum -c slotr-0.1.2-x86_64-unknown-linux-musl.tar.gz.sha256
   ```

3. Look at what the archive holds. It should list only `slotr`, `LICENSE`
   and `THIRD-PARTY-NOTICES`, each on a line that starts with `-`.

   ```sh
   tar -tvzf slotr-0.1.2-x86_64-unknown-linux-musl.tar.gz
   ```

4. Extract the binary. Only `slotr` is taken out.

   ```sh
   tar -xzf slotr-0.1.2-x86_64-unknown-linux-musl.tar.gz slotr
   ```

5. Install it for your user.

   ```sh
   install -D -m 0755 slotr ~/.local/bin/slotr
   ```

6. Check that it runs.

   ```sh
   ~/.local/bin/slotr --version
   ```

## Mirrors and tests

`SLOTR_INSTALL_BASE_URL` overrides the release download directory. Mirrors
must serve the exact archive name, its `.sha256`, and `config.example.toml`
for `--config`. The version still comes from the script. The script trusts that
location completely. The checksum comes from the same place, so it catches
a damaged download, not a changed one. Leave the variable unset unless you
mean to use a mirror. Local tests use
`file://` URLs, which curl supports:

```sh
SLOTR_INSTALL_BASE_URL=file:///tmp/slotr-release sh install.sh --config
```

## Build from source

Building needs Rust 1.99 or newer (edition 2024) and the
`x86_64-unknown-linux-musl` target:

```sh
cargo build --release --locked --target x86_64-unknown-linux-musl
install -m755 target/x86_64-unknown-linux-musl/release/slotr ~/.local/bin/slotr
```

## Configuration

A fresh install needs no config file; built-in defaults apply. See
[configuration.md](configuration.md) to add one.
