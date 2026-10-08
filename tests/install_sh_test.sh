#!/bin/sh
set -eu
repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
shell=${SH:-sh}
work=$(mktemp -d)
trap 'rm -rf "$work"' 0
trap 'exit 1' HUP INT TERM
asset=slotr-0.1.2-x86_64-unknown-linux-musl.tar.gz
release=$work/release
mkdir "$release" "$work/members"
cat >"$work/members/slotr" <<'STUB'
#!/bin/sh
case "$*" in
    --version) echo 'slotr 0.1.2' ;;
    "config check "*) [ -f "$3" ] && echo 'slotr: config OK' ;;
    *) exit 1 ;;
esac
STUB
printf 'licence\n' >"$work/members/LICENSE"
printf 'notices\n' >"$work/members/THIRD-PARTY-NOTICES"
printf '[pools.default]\nslots = 1\n' >"$release/config.example.toml"
pack() {
    tar -czf "$release/$asset" -C "$work/members" "$@"
    (cd "$release" && sha256sum "$asset" >"$asset.sha256")
}
pack slotr LICENSE THIRD-PARTY-NOTICES
cp "$release/$asset" "$work/good.tar.gz"
cp "$release/$asset.sha256" "$work/good.sha256"
run() {
    HOME="$work/home" SLOTR_INSTALL_BASE_URL="file://$release" "$shell" "$repo/install.sh" "$@" >"$work/out" 2>"$work/err"
}
fail() { echo "install test: $*" >&2; exit 1; }
refused() {
    expected=$1
    expected_code=${2:-1}
    shift
    [ "$#" -eq 0 ] || shift
    code=0
    run "$@" || code=$?
    [ "$code" = "$expected_code" ] || fail "expected exit $expected_code, got $code"
    [ "$(cat "$work/err")" = "$expected" ] || fail "unexpected error: $(cat "$work/err")"
    if [ -f "$work/before" ]; then
        cmp "$work/before" "$work/home/.local/bin/slotr" || fail 'changed installed binary on refusal'
    else
        [ ! -e "$work/home/.local/bin/slotr" ] || fail 'installed binary on refusal'
    fi
    [ ! -d "$work/home/.local/bin" ] ||
        [ -z "$(find "$work/home/.local/bin" -name '.slotr.*' -print)" ] || fail 'left install temporary file'
}
run
grep -q '^slotr 0.1.2$' "$work/out" || fail 'missing version'
[ -x "$work/home/.local/bin/slotr" ] || fail 'binary is not executable'
printf 'old binary\n' >"$work/home/.local/bin/slotr"
run
cmp "$work/members/slotr" "$work/home/.local/bin/slotr" || fail 'did not replace binary'
run --config
cmp "$release/config.example.toml" "$work/home/.config/slotr/config.toml" || fail 'did not create config'
grep -q '^slotr: config OK$' "$work/out" || fail 'did not check config'
printf 'edited config\n' >"$work/home/.config/slotr/config.toml"
run --config
[ "$(cat "$work/home/.config/slotr/config.toml")" = 'edited config' ] || fail 'overwrote config'
grep -q '^kept existing config$' "$work/out" || fail 'missing kept message'
cp "$work/home/.local/bin/slotr" "$work/before"
printf '%064d  %s\n' 0 "$asset" >"$release/$asset.sha256"
refused 'slotr install: the checksum did not verify'
# The remaining refusals also prove that a fresh HOME stays empty.
mkdir "$work/fresh"
mv "$work/home" "$work/saved-home"
mv "$work/fresh" "$work/home"
mv "$work/before" "$work/saved-before"
refused 'slotr install: the checksum did not verify'
sed 's/slotr-0.1.2/other-0.1.2/' "$work/good.sha256" >"$release/$asset.sha256"
refused 'slotr install: the checksum did not verify'
cp "$work/good.sha256" "$release/$asset.sha256"
cat "$work/good.sha256" >>"$release/$asset.sha256"
refused 'slotr install: the checksum did not verify'
printf 'extra\n' >"$work/members/extra"
pack slotr LICENSE THIRD-PARTY-NOTICES extra
refused 'slotr install: the archive layout is invalid'
pack slotr LICENSE LICENSE
refused 'slotr install: the archive layout is invalid'
tar -cf "$work/dup.tar" -C "$work/members" slotr LICENSE
tar -rf "$work/dup.tar" -C "$work/members" LICENSE
gzip -c "$work/dup.tar" >"$release/$asset"
(cd "$release" && sha256sum "$asset" >"$asset.sha256")
refused 'slotr install: the archive layout is invalid'
pack LICENSE THIRD-PARTY-NOTICES
refused 'slotr install: the archive layout is invalid'
mv "$work/members/slotr" "$work/stub"
ln -s LICENSE "$work/members/slotr"
pack slotr LICENSE THIRD-PARTY-NOTICES
refused 'slotr install: the archive layout is invalid'
mv "$release/$asset" "$work/missing.tar.gz"
refused 'slotr install: the release download failed'
refused 'usage: sh install.sh [--config]' 2 --unknown
# Cutting off the last call leaves definitions only.
sed '$d' "$repo/install.sh" >"$work/truncated.sh"
HOME="$work/home" "$shell" "$work/truncated.sh"
[ ! -e "$work/home/.local/bin/slotr" ] || fail 'truncated script installed binary'
printf 'install.sh tests passed (%s)\n' "$shell"
