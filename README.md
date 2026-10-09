# slotr

slotr is a queue for heavy commands on one Linux machine, such as dev
servers and test stacks. `slotr run` waits for a free slot and enough free
memory, then runs your command in its own
[systemd user service](docs/install.md#requirements). When memory runs low,
slotr can stop the lowest-priority evictable command, newest first within a
priority level, and it can reclaim a run past its lease when another
campaign waits. This lowers the risk of running out
of memory but does not remove it.

## Quick install

```sh
curl -fsSL https://github.com/olafurns7/herdr-slotr/releases/latest/download/install.sh | sh
```

Downloads the release for Linux x86_64, verifies its checksum, and installs to `~/.local/bin`.

Prefer to check first?

```sh
curl -fsSL https://github.com/olafurns7/herdr-slotr/releases/latest/download/install.sh -o install.sh
cat install.sh
sh install.sh
```

## Quick agent setup

```sh
curl -fsSL https://github.com/olafurns7/herdr-slotr/releases/latest/download/install.sh | sh -s -- --config
```

Also creates the config from the example when none exists, then checks it.

## Documentation

- [Install](docs/install.md)
- [Agent setup](docs/agent-setup.md)
- [Usage](docs/usage.md)
- [Configuration](docs/configuration.md)
- [How it works](docs/how-it-works.md)
- [Troubleshooting](docs/troubleshooting.md)
- [Validate and release](docs/release.md)

Licence: [MIT](LICENSE). [Third-party notices](THIRD-PARTY-NOTICES) ship in the release archive.
