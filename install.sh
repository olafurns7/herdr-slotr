#!/bin/sh

main() {
    VERSION=0.2.0
    fail() { printf 'slotr install: %s\n' "$*" >&2; exit 1; }
    config_requested=false
    case "$#:${1:-}" in
        0:) ;;
        1:--config) config_requested=true ;;
        *) printf 'usage: sh install.sh [--config]\n' >&2; return 2 ;;
    esac
    export LC_ALL=C
    [ "$(uname -s) $(uname -m)" = "Linux x86_64" ] || fail "Linux x86_64 only"
    for tool in curl tar sha256sum; do
        command -v "$tool" >/dev/null 2>&1 || fail "missing $tool"
    done
    [ -n "${HOME:-}" ] || fail "HOME is not set"
    download_dir=$(mktemp -d 2>/dev/null) || fail "cannot create temporary directory"
    install_tmp=
    trap 'rm -f "$install_tmp"; rm -rf "$download_dir"' 0
    trap 'fail "interrupted"' HUP INT TERM
    asset=slotr-$VERSION-x86_64-unknown-linux-musl.tar.gz
    base=${SLOTR_INSTALL_BASE_URL:-https://github.com/olafurns7/herdr-slotr/releases/download/v$VERSION}
    curl -fsSL "$base/$asset" -o "$download_dir/$asset" 2>/dev/null &&
        curl -fsSL "$base/$asset.sha256" -o "$download_dir/$asset.sha256" 2>/dev/null || fail "the release download failed"
    hash=$(sha256sum <"$download_dir/$asset" 2>/dev/null) || fail "the checksum did not verify"
    hash=${hash%% *}
    checksum=$(cat "$download_dir/$asset.sha256" 2>/dev/null) || fail "the checksum did not verify"
    { [ "${#hash}" -eq 64 ] && [ "$(grep -c '' <"$download_dir/$asset.sha256")" = 1 ] &&
        { [ "$checksum" = "$hash  $asset" ] || [ "$checksum" = "$hash *$asset" ]; }; } || fail "the checksum did not verify"
    members=$(tar -tvzf "$download_dir/$asset" 2>/dev/null) || fail "the archive layout is invalid"
    printf '%s\n' "$members" | awk '
        $1 !~ /^-/ || NF != 6 || ($6 != "slotr" && $6 != "LICENSE" && $6 != "THIRD-PARTY-NOTICES") {bad=1}
        {if (seen[$6]++) bad=1}
        $6 == "slotr" {binary++}
        END {exit bad || binary != 1}
    ' || fail "the archive layout is invalid"
    { mkdir "$download_dir/bin" && tar -xzf "$download_dir/$asset" -C "$download_dir/bin" slotr &&
        [ -f "$download_dir/bin/slotr" ] && [ ! -L "$download_dir/bin/slotr" ]; } 2>/dev/null || fail "the archive holds no slotr binary"
    binary=$HOME/.local/bin/slotr
    mkdir -p "$HOME/.local/bin" 2>/dev/null || fail "cannot write ~/.local/bin/slotr"
    install_tmp=$(mktemp "$HOME/.local/bin/.slotr.XXXXXX" 2>/dev/null) || fail "cannot write ~/.local/bin/slotr"
    { cat "$download_dir/bin/slotr" >"$install_tmp" && chmod 0755 "$install_tmp" &&
        mv -fT "$install_tmp" "$binary"; } 2>/dev/null || fail "cannot write ~/.local/bin/slotr"
    "$binary" --version 2>/dev/null || fail "slotr --version failed"
    printf 'Installed to %s\n' "$binary"
    case ":${PATH:-}:" in
        *":$HOME/.local/bin:"*) ;;
        *) printf 'Add ~/.local/bin to PATH: export PATH="$HOME/.local/bin:$PATH"\n' ;;
    esac
    if "$config_requested"; then
        config=$HOME/.config/slotr/config.toml
        mkdir -p "$HOME/.config/slotr" 2>/dev/null || fail "cannot create config directory"
        if [ -e "$config" ] || [ -L "$config" ]; then
            printf 'kept existing config\n'
        else
            curl -fsSL "$base/config.example.toml" -o "$download_dir/config.toml" 2>/dev/null || fail "the config download failed"
            (set -C; cat "$download_dir/config.toml" >"$config") 2>/dev/null || fail "cannot write config"
        fi
        "$binary" config check "$config" 2>/dev/null || fail "slotr config check failed"
    fi
}

main "$@"
