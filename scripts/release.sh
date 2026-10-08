#!/bin/sh
set -eu
if [ "$(uname -s)" != Linux ] || [ "$(uname -m)" != x86_64 ]; then
    echo "slotr: releases require a Linux x86_64 host" >&2
    exit 2
fi
if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
    echo "usage: sh scripts/release.sh VERSION [--publish]" >&2
    exit 2
fi
version=$1
case "$version" in ""|*[!0-9A-Za-z.-]*) echo "slotr: invalid version" >&2; exit 2;; esac
publish=${2:-}
case "$publish" in ""|--publish) ;; *) echo "slotr: expected --publish" >&2; exit 2;; esac
cd "$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
if [ -n "$(git status --porcelain)" ]; then
    echo "slotr: refusing a dirty tree" >&2
    exit 2
fi
# --publish may append this machine's asset to an existing release. A local
# pre-existing tag without a release is refused rather than silently reused.
if git rev-parse --verify --quiet "refs/tags/v$version" >/dev/null; then
    echo "slotr: tag v$version already exists" >&2
    exit 2
fi
package_version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)
if [ "$version" != "$package_version" ]; then
    echo "slotr: VERSION must match Cargo.toml ($package_version)" >&2
    exit 2
fi
target=x86_64-unknown-linux-musl
sh scripts/notices.sh --check
cargo_home=${CARGO_HOME:-$HOME/.cargo}
rustup_home=${RUSTUP_HOME:-$HOME/.rustup}
RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$cargo_home=/cargo --remap-path-prefix=$rustup_home=/rustup --remap-path-prefix=$(pwd -P)=/slotr" \
    cargo build --release --locked --target "$target"
mkdir -p dist
asset="slotr-$version-$target.tar.gz"
tar --owner=0 --group=0 --numeric-owner --sort=name -czf "dist/$asset" \
    LICENSE THIRD-PARTY-NOTICES -C "target/$target/release" slotr
strings "target/$target/release/slotr" > dist/binary-strings.txt
tar -tvzf "dist/$asset" > dist/archive-list.txt
for listing in dist/binary-strings.txt dist/archive-list.txt; do
    if grep -Eq '/home/|/Users/|/mnt/' "$listing" || grep -Fq "$(id -un)" "$listing" ||
        grep -Fq -e "$cargo_home" -e "$rustup_home" -e "$(pwd -P)" "$listing"; then
        echo "slotr: release contains a builder path or login name" >&2
        exit 1
    fi
done
(cd dist && sha256sum "$asset" > "$asset.sha256")
if [ "$publish" = --publish ]; then
    if gh release view "v$version" >/dev/null 2>&1; then
        gh release upload "v$version" "dist/$asset" "dist/$asset.sha256"
    else
        gh release create "v$version" --target "$(git rev-parse HEAD)" "dist/$asset" "dist/$asset.sha256" --title "slotr $version" --notes "Linux x86_64 musl build."
    fi
fi
