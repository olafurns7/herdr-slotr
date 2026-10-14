# Cooperative flock guard

The optional guard helps callers move selected legacy locks into slotr's queue.
It refuses native file and inherited-FD calls targeting a pool's
`legacy_lock.queue_only = true` file, with exit 2, the pool/path, and a recipe:

```sh
slotr run --pool heavy --campaign my-feature --purpose "build check" -- cargo test --locked
```

It neither runs the refused payload nor acquires a lock, creates a target,
or queues a replacement ticket. The caller must submit the actual command.
Slotr's own shared/exclusive compatibility locking stays unchanged.

## Fixed policy and opt-in launcher

`queue_only` defaults to false. Configure each selected lock explicitly:

```toml
[pools.heavy.legacy_lock]
path = "/absolute/locks/heavy.lock"
mode = "shared"
queue_only = true

[pools.runtime.legacy_lock]
path = "/absolute/locks/runtime.lock"
mode = "exclusive"
queue_only = false
```

Every protected path must be absolute and resolve to an existing regular
file. At least one protected entry is required; a missing, malformed, empty,
or unprotected policy fails closed even for unrelated targets or help.
Missing, renamed, nonregular, or unresolvable protected files also refuse.
Keep detached runtime locks unprotected until their supervised lifetime is
proved: a lock cannot account for work that outlives its slotr unit.

The Linux-only checker has an explicit, decision-only interface:

```sh
/absolute/bin/slotr flock-check --config /absolute/slotr/config.toml -- -n /absolute/locks/watch.lock command
```

Success is silent exit 0. It never executes native flock or the payload,
creates the target, or queues work; a native launcher uses its result.
Checker dispatch precedes normal config loading: it reads exactly the
required absolute file using the normal schema, ignoring HOME/XDG discovery,
`SLOTR_CONFIG`, and all scalar `SLOTR_*` overrides for policy. Ordinary
slotr commands retain their usual environment/config behavior.

Compile the matching reviewed [native source](../scripts/flock-shim.c) with
an existing C compiler to a staging path, using literal absolute defines:

```sh
cc -std=c11 -Wall -Wextra -Werror -O2 \
  '-DSLOTR_BINARY="/absolute/bin/slotr"' \
  '-DSLOTR_CONFIG="/absolute/slotr/config.toml"' \
  scripts/flock-shim.c -o /absolute/staging/flock
```

Replace both literal paths with the intended deployment paths. There are no
runtime executable/policy environment overrides, interpreter, shell wrapper,
or argv0/symlink dispatch. The launcher requires both defines and refuses
relative paths. Staging compilation does not install it. The installer and
release package continue to contain only slotr; the hub owns any later
launcher installation and PATH/config changes under the idle migration gate.

## Native compatibility

The finite parser follows util-linux **2.39.3** flock's leading option scan:
short clusters, attached/separate `w`/`E` values, exact and unambiguous long
options, and `--`. Options stop at the first operand. One operand is an FD;
multiple operands select a file/directory plus command. Native `FILE -c STRING`
and `FILE --command STRING` are recognized only in that position, with exactly
one command string. Early help/version delegates when reached. Unknown,
ambiguous, or malformed syntax refuses; new native syntax requires a parser
review before use through the shim.

Existing targets are compared by device/inode, covering relative, symlink,
hardlink, and `/proc/self/fd/N` aliases. FD spellings accept leading ASCII
whitespace, an optional sign, decimal digits/zero padding and nonnegative
signed-i32 values, including `-- -0` for FD0. Inspection never closes,
consumes, or locks inherited descriptors. Identity uncertainty refuses.

Once all protected files are validated, unrelated target metadata ENOENT
delegates unchanged: native decides whether to create a new leaf/dangling
symlink referent or report a missing-parent error. The guard does not open
or create the target. Existing unrelated directories and runtime locks
delegate too. The native C parent forks a checker child with the original
argument pointers and inherited environment/FDs, then waits for that exact
child. Only normal exit 0 permits exec of fixed `/usr/bin/flock` with the
original native args. Nonzero, exec/fork/wait errors, signal death, or a
stopped checker refuse; a stopped checker is killed and reaped by its parent.
The parent never opens or redirects standard FDs, changes cwd/umask,
normalizes environment names, or resets the caller's signal mask. Rust
startup changes stay inside the checker child. Ignored SIGPIPE and closed
standard descriptors reach native flock as they did in the original caller.

For inherited ignored SIGCHLD or SA_NOCLDWAIT, the parent snapshots the
complete action, temporarily defaults it for a waitable child, and restores
it before native exec; any restoration failure refuses. EINTR waits retry.
The extra checker can add a pending SIGCHLD while that signal is blocked.
Installed util-linux 2.39.3 clears SIGCHLD pending/action before its payload,
so actual native payload outputs match in the covered tests. This does not
claim extra-child timing or signal-generation transparency. The launcher
never drains unrelated pending signals or changes the mask to hide them.
No shell or PATH lookup wraps native flock; only native's own `-c` form
selects a shell. Raw non-UTF8 argv/environment and inherited open-file
descriptions pass through the native parent unchanged.

## Cooperative boundary

This is accidental-bypass guidance, not kernel or security enforcement.
Keep policy, launcher, and lock namespace stable during guarded calls.
Pathname inspection followed by native reopening cannot prevent deliberate
inode/namespace substitution. Direct `/usr/bin/flock`, direct syscalls,
mutable launcher/config, intentional namespace changes, and participants
opened before deployment are outside the guarantee. Do not replace a live
lock inode; historical aliases after replacement are not tracked.

## Separately approved migration and rollback

1. Freeze every new heavy submission, both slotr and native. Include runtime
   participants if a shared binary/config swap affects them.
2. Inventory and drain native holders **and blocked waiters** using process,
   FD, and lock evidence; slotr status alone is insufficient. Preserve the
   existing lock inode and recheck idle before deploying.
3. Deploy the approved binary, fixed config and native launcher; protected
   files must already exist. Refresh shell command hashes and check actual
   resolution in interactive/noninteractive shells and scratch HOME.
4. Keep the barrier until scratch smoke checks prove protected file/FD
   refusal, unrelated watch FD acquisition/persistence/unlock, unaffected
   runtime file/FD calls, unchanged payload environment, and slotr
   priority/FIFO behavior.
5. Rollback uses the same freeze/drain barrier and restores launcher/PATH,
   config and binary without replacing a live lock inode. Record that native
   bypass has been restored before releasing the barrier.

Source and scratch integration checks do not prove live watcher/runtime
behavior, deployment, or rollback. This document does not authorize host
changes or migration.
