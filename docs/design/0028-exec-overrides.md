# Design note 0028: `--user`/`--cwd`/`--env` overrides for `exec`

Status: implemented
Scope: `bin/ocirun/src/main.rs`'s `cmd_exec`, `bin/ociman/src/main.rs`'s
`cmd_exec`.

## The gap

0022 (`ocirun exec`) and 0023 (`ociman exec`) both explicitly flagged
the same "still not here" item in their own design notes: no
`--user`/`--cwd`/`--env` override support, only ever the target
container's own defaults read back from its bundle. Real `runc exec`
and `podman exec` both support all three.

## The underlying mechanism already existed — only the CLI surface didn't

`oci_runtime_core::exec::ExecRequest` already has `user`/`cwd`/`env`
fields, populated by each caller from the container's own bundle
defaults — nothing in `oci_runtime_core` itself needed to change at
all. The entire gap was that neither binary's own `cmd_exec` exposed
any way to override what it read back from the bundle before building
the request.

## Semantics verified against the real tools, not guessed

Read `runc exec`'s own flag-handling code
(`~/git/runc/exec.go`) to get the exact override semantics right rather
than inventing plausible-sounding ones:

* `--cwd <path>`: full override if given, otherwise the bundle's own
  default is unchanged.
* `--env KEY=value` (repeatable): **appended** to the base process
  environment, not a replacement — `runc`'s own code literally does
  `p.Env = append(p.Env, cmd.StringSlice("env")...)`.
* `--user <uid>[:<gid>]`: `uid` always overrides if `--user` is given
  at all; `gid` only changes if the `:gid` part was actually given,
  otherwise the container's own default gid is preserved — checked
  against `runc exec.go`'s own `strings.Cut`-based parsing, which only
  assigns `p.User.GID` inside the `if ok` branch.

`ocirun exec --user` is deliberately **numeric-only** (`<uid>[:<gid>]`,
parsed by a small `parse_numeric_user` helper), matching real `runc
exec`'s own low-level-runtime scope: name resolution needs
`/etc/passwd` inside the rootfs, which is a higher-level-tool concern
this project already puts in `ociman`, not `ocirun` (see 0024's own
reasoning for the same division of responsibility applied to an
image's `USER` config field).

`ociman exec --user`, by contrast, accepts a **name or a number** —
checked against real `podman exec --user`'s own flag doc ("Sets the
username or UID ... and optionally the groupname or GID") — reusing
0024's own `user_resolve::resolve` (and the same `resolve_user` wrapper
that already enforces this rootless runtime's single-mapped-uid
limitation with a clear error) against the container's own rootfs,
rather than duplicating any of that resolution logic. Unlike `ocirun
exec`'s partial-override semantics (uid always changes, gid only if
given), `ociman exec --user`'s resolution always sets *both* uid and
gid together (whatever `user_resolve::resolve` determines the target
user's own gid to be) — matching real `podman`'s own `GetExecUser`
semantics for a full user spec, not a partial one, and consistent with
how `ociman run`'s own image `USER` resolution (0024) already works.

## Real, automated, end-to-end tests

`tests/tests/ocirun_exec.rs` (4 new cases): `--cwd` actually changing
the exec'd process's working directory; `--env` appending a variable
while leaving the container's own base `PATH` intact; `--user 0:0`
succeeding; `--user 1000` being rejected (hits the same "only uid 0 is
mappable" wall the container's own init process already does, now
surfaced through `exec` too) without disturbing the still-running
container.

`tests/tests/ociman_exec.rs` (2 new cases, using the same
`seed_image`/`seed_image_with_files`/concurrent-`run` approach 0023/0025
established): `--cwd`/`--env` together against a real running
container; a named `--user root` (resolved via a seeded `/etc/passwd`)
succeeding, and a named `--user app` (resolving fine to a real non-root
uid via the same passwd file) still correctly rejected — proving the
resolution and the mapping-limitation check are wired together
correctly for `exec`, not just for `run` (0024 already covered `run`'s
own version of this).

## Performance

Doesn't touch `oci_runtime_core::exec`/`nsenter`/`launch` at all — pure
CLI-argument threading in each binary's own `cmd_exec`, evaluated once
per `exec` invocation, nowhere near the `run` fork-to-exec hot path
0012/0018/0023/0025/0026/0027 have been benchmarking. No re-benchmark
needed for this increment, consistent with prior increments that only
touched non-hot-path code.

## What's still not here

* `--additional-gids`/supplementary group IDs for `exec` (same gap
  0024 already flagged for image `USER` resolution: this runtime has
  nowhere to put them yet regardless of which command sets them).
* `-t`/`--tty` (pty allocation) for `exec` — no pty support anywhere
  in this project yet.
* `--privileged`/capability overrides for `exec` (only user/cwd/env,
  matching this increment's own scope).
