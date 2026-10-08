# Install

This file explains how to install slotr from a release or build it from source, and what the host needs to run it.

## Requirements

- Linux x86_64. Release builds exist only for this platform.
- `gh` signed in with access to olafurns7/herdr-slotr.
  Downloads go through `gh release download`.
- To use `run`, `stop`, and the internal `_supervise` command: systemd 255.4
  or newer (the verified minimum for name-only `--setenv`) and a reachable
  systemd user bus. Start workloads from a terminal in your user session.
  A sandbox without that bus gets a clear error and exit 2.
- `status` and `config` are portable and need no systemd.

## Install a release

Release v0.1.1 has two assets:

- `slotr-0.1.1-x86_64-unknown-linux-musl.tar.gz`, which holds `LICENSE`,
  `THIRD-PARTY-NOTICES`, and `slotr`.
- `slotr-0.1.1-x86_64-unknown-linux-musl.tar.gz.sha256`, one line of the form
  `HASH  ASSETNAME`.

The quick install block in the README does these steps:

1. Make a temporary directory with `mktemp -d`.
2. Download both assets into it with `gh release download`.
3. Compute the archive's SHA-256 and compare it with the hash in the
   `.sha256` file. The block stops here when they differ, whatever file name
   the `.sha256` file states.
4. Check that all members are regular files, exactly one is `slotr`, and
   the others are only `LICENSE` or `THIRD-PARTY-NOTICES`. Extract `slotr`
   and install it to `~/.local/bin/slotr` with mode 0755.
5. Run `~/.local/bin/slotr --version`, which prints `slotr 0.1.1`.

The same steps by hand:

```sh
d=$(mktemp -d) &&
a=slotr-0.1.1-x86_64-unknown-linux-musl.tar.gz &&
gh release download v0.1.1 --repo olafurns7/herdr-slotr --pattern "$a" --pattern "$a.sha256" --dir "$d" &&
h=$(sha256sum <"$d/$a" | cut -d' ' -f1) && [ "${#h}" -eq 64 ] && [ "$h" = "$(cut -d' ' -f1 "$d/$a.sha256")" ] && echo "checksum OK" &&
m=$(tar -tvzf "$d/$a") &&
printf '%s\n' "$m" | awk '$1 !~ /^-/ || NF != 6 || ($6 != "slotr" && $6 != "LICENSE" && $6 != "THIRD-PARTY-NOTICES") {bad=1} $6 == "slotr" {binary++} END {exit bad || binary != 1}' &&
tar -xzf "$d/$a" -C "$d" slotr &&
mkdir -p ~/.local/bin &&
install -m 0755 "$d/slotr" ~/.local/bin/slotr &&
~/.local/bin/slotr --version
```

Go on only when the checksum line prints `checksum OK` and the archive check succeeds.

The last step uses the full path, so it works even when `~/.local/bin` is not
on your `PATH`. Add `~/.local/bin` to `PATH` to type `slotr` alone.

To update, run the same steps with the new version number.

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
