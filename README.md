# slotr

slotr is a queue for heavy commands on one Linux machine, such as dev
servers and test stacks. `slotr run` waits for a free slot and enough free
memory, then runs your command in its own
[systemd user service](docs/install.md#requirements). When memory runs low,
slotr can stop the newest evictable command, and it can reclaim a run past
its lease when another campaign waits. This lowers the risk of running out
of memory but does not remove it.

## Quick install

You need Linux x86_64 and `gh` signed in with access to
olafurns7/herdr-slotr. Paste this whole block:

```sh
d=$(mktemp -d) && a=slotr-0.1.0-x86_64-unknown-linux-musl.tar.gz &&
gh release download v0.1.0 --repo olafurns7/herdr-slotr --pattern "$a" --pattern "$a.sha256" --dir "$d" &&
[ "$(sha256sum <"$d/$a" | cut -d' ' -f1)" = "$(cut -d' ' -f1 "$d/$a.sha256")" ] &&
tar -xzf "$d/$a" -C "$d" slotr &&
mkdir -p ~/.local/bin && install -m 0755 "$d/slotr" ~/.local/bin/slotr &&
~/.local/bin/slotr --version
```

## Quick agent setup

For a Herdr fleet host, an agent can paste this block. It installs the
same pinned release with the same checks as herdr-setup's installer, then
creates `~/.config/slotr/config.toml` from `config.example.toml` only if no config exists, and checks it.

```sh
(
set -eu
a=slotr-0.1.0-x86_64-unknown-linux-musl.tar.gz d=$(mktemp -d)
fail() { echo "slotr setup: $*" >&2; exit 1; }
[ "$(uname -s) $(uname -m)" = "Linux x86_64" ] || fail "Linux x86_64 only"
gh auth status >/dev/null 2>&1 || fail "gh is missing or not signed in"
gh release download v0.1.0 --repo olafurns7/herdr-slotr --pattern "$a" --pattern "$a.sha256" --dir "$d" >/dev/null || fail "the release download failed"
h=$(sha256sum <"$d/$a") h=${h%% *} s=$(cat "$d/$a.sha256")
{ [ "${#h}" -eq 64 ] && [ "$(grep -c '' <"$d/$a.sha256")" = 1 ] && { [ "$s" = "$h  $a" ] || [ "$s" = "$h *$a" ]; }; } || fail "the checksum did not verify"
m=$(tar -tvzf "$d/$a")
case $m in -*' slotr') ;; *) fail "the archive holds no slotr binary" ;; esac
{ [ "$(printf '%s\n' "$m" | grep -c '')" = 1 ] && mkdir "$d/bin" && tar -xzf "$d/$a" -C "$d/bin" slotr && [ -f "$d/bin/slotr" ] && [ ! -L "$d/bin/slotr" ]; } || fail "the archive holds no slotr binary"
mkdir -p "$HOME/.local/bin" && t=$(mktemp "$HOME/.local/bin/.slotr.XXXXXX")
{ cat "$d/bin/slotr" >"$t" && chmod 0755 "$t" && mv -f "$t" "$HOME/.local/bin/slotr"; } || fail "cannot write ~/.local/bin/slotr"
"$HOME/.local/bin/slotr" --version
mkdir -p "$HOME/.config/slotr"
[ -e "$HOME/.config/slotr/config.toml" ] || { gh api -H 'Accept: application/vnd.github.raw' 'repos/olafurns7/herdr-slotr/contents/config.example.toml?ref=v0.1.0' >"$d/config.toml" && mv -n "$d/config.toml" "$HOME/.config/slotr/config.toml"; }
"$HOME/.local/bin/slotr" config check
)
```

## Documentation

- [Install](docs/install.md)
- [Agent setup](docs/agent-setup.md)
- [Usage](docs/usage.md)
- [Configuration](docs/configuration.md)
- [How it works](docs/how-it-works.md)
- [Troubleshooting](docs/troubleshooting.md)
- [Validate and release](docs/release.md)
