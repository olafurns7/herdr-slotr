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
`SLOTR_CLOCK_OFFSET_FILE` (seconds added to the CLOCK_BOOTTIME clock used
for state timing). Production ignores all of them.

## Release

Releases are built locally on a clean Linux x86_64 checkout:

```sh
sh scripts/release.sh VERSION
sh scripts/release.sh VERSION --publish
```

- VERSION must match the version in `Cargo.toml`.
- The script refuses dirty trees, an existing local `vVERSION` tag, and
  other hosts, with exit 2.
- Before building, `sh scripts/notices.sh --check` must confirm that the
  checked-in notices match the locked target dependencies. Run
  `sh scripts/notices.sh` to refresh them from cached crate and toolchain
  licence files after a dependency update.
- The build remaps cargo home, rustup home, and checkout paths. After packing,
  the script refuses builder paths or the login name in the binary strings
  or archive listing.
- Assets in `dist/` are `slotr-VERSION-x86_64-unknown-linux-musl.tar.gz` and
  its `.sha256` file. The archive holds three regular files in this order: `LICENSE`,
  `THIRD-PARTY-NOTICES`, and `slotr`, with numeric owner and group 0.
- `--publish` explicitly creates a GitHub release with `gh`, targeting the
  exact local HEAD that was built. If the release already exists, it only
  attaches the assets to it and does not check or change its target.
- There are no GitHub Actions workflows.

herdr-setup's installer installs a pinned release; see
[agent-setup.md](agent-setup.md).
