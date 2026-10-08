# Validate and release

This file explains how to test slotr locally and how to publish a release.

## Validate locally

The integration tests use a fake systemd driven by Python 3.11+ stdlib.

```sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo build --release --locked --target x86_64-unknown-linux-musl
file target/x86_64-unknown-linux-musl/release/slotr
```

A shared host may wrap the cargo commands in its own lock
(`flock /path/to/heavy.lock cargo test --locked`).

Only `SLOTR_TEST=1` enables the test seams: `SLOTR_PROC_ROOT`,
`SLOTR_CGROUP_ROOT`, `SLOTR_SYSTEMD_RUN`, `SLOTR_SYSTEMCTL`, and
`SLOTR_CLOCK_OFFSET_FILE` (seconds added to the real wall clock). Production
ignores all of them.

## Release

Releases are built locally on a clean Linux x86_64 checkout:

```sh
sh scripts/release.sh VERSION
sh scripts/release.sh VERSION --publish
```

- VERSION must match the version in `Cargo.toml`.
- The script refuses dirty trees, an existing local `vVERSION` tag, and
  other hosts, with exit 2.
- Assets in `dist/` are `slotr-VERSION-x86_64-unknown-linux-musl.tar.gz` and
  its `.sha256` file. The archive holds one file, `slotr`.
- `--publish` explicitly creates a GitHub release with `gh`, or uploads to an
  existing one, targeting the exact local HEAD that was built.
- There are no GitHub Actions workflows.

herdr-setup's installer installs a pinned release; see
[agent-setup.md](agent-setup.md).
