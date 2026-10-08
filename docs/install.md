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

Run these steps in the same shell. Stop if a command fails.

1. Make a temporary download directory.

   ```sh
   d=$(mktemp -d)
   ```

2. Choose the release archive.

   ```sh
   a=slotr-0.1.2-x86_64-unknown-linux-musl.tar.gz
   ```

3. Set the release URL.

   ```sh
   base=https://github.com/olafurns7/herdr-slotr/releases/download/v0.1.2
   ```

4. Download the binary archive.

   ```sh
   curl -fsSL "$base/$a" -o "$d/$a"
   ```

5. Download its checksum.

   ```sh
   curl -fsSL "$base/$a.sha256" -o "$d/$a.sha256"
   ```

6. Read the checksum; proceed only if it has one line naming exactly `$a`.

   ```sh
   cat "$d/$a.sha256"
   ```

7. Verify the archive; proceed only when it prints `OK`.

   ```sh
   (cd "$d" && sha256sum -c "$a.sha256")
   ```

8. Read the archive listing; stop if tar reports an error.

   ```sh
   m=$(tar -tvzf "$d/$a")
   ```

9. Check regular files, allowed names, and duplicates before extraction.

   ```sh
   printf '%s\n' "$m" | awk '$1 !~ /^-/ || NF != 6 || ($6 != "slotr" && $6 != "LICENSE" && $6 != "THIRD-PARTY-NOTICES") {bad=1} {if (seen[$6]++) bad=1} $6 == "slotr" {binary++} END {exit bad || binary != 1}'
   ```

10. Make an empty extraction directory.

   ```sh
   mkdir "$d/bin"
   ```

11. Extract only the checked binary.

    ```sh
    tar -xzf "$d/$a" -C "$d/bin" slotr
    ```

12. Create the install directory.

    ```sh
    mkdir -p ~/.local/bin
    ```

13. Install the binary with executable permissions.

    ```sh
    install -m 0755 "$d/bin/slotr" ~/.local/bin/slotr
    ```

14. Check the installed version.

    ```sh
    ~/.local/bin/slotr --version
    ```

## Mirrors and tests

`SLOTR_INSTALL_BASE_URL` overrides the release download directory. Mirrors
must serve the exact archive name, its `.sha256`, and `config.example.toml`
for `--config`. The version still comes from the script. Local tests use
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
