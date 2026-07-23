# Design note 0207: `ocibox enter`

Status: implemented
Scope: `bin/ocibox/src/main.rs` (`Command::Enter`, `cmd_enter`,
`enter_spec`, `default_shell_args`, `BoxRecord::env`/
`BoxRecord::working_dir`); `bin/ocibox/Cargo.toml` (new
`oci-runtime-core` dependency); `tests/tests/ocibox_enter.rs`.

## Continuing milestone 7

`ocibox create`/`list`/`rm` (0205, 0206) manage a box's own persisted
state and rootfs, but neither actually launches anything — both design
notes named `ocibox enter` as the next, still-ahead step. This
increment is that step: a real, live command running inside an
already-created box's own rootfs, using the exact same shared
`oci_runtime_core::launch`/`Bundle`/`validate` two-phase lifecycle
`ociman run`/`ocirun run` already use, with **zero new namespace/mount/
launch code of its own** to write or maintain.

## Deliberately not real `distrobox enter`'s own persistence model

Real `distrobox enter` creates one long-lived container once, then
every subsequent `enter` call attaches to the *same*, already-running
container (a real init process kept alive across sessions). Matching
that would need `ocibox create` itself to also launch a genuinely
long-lived keeper process the box stays subordinate to — a materially
bigger feature than this increment, and deliberately deferred.

This first slice instead does a **single foreground fork+exec+wait**
per `enter` call (matching `ocirun run`'s own simplest
create-start-wait-in-one model, via `oci_runtime_core::launch::run`):
each invocation is its own independent container process. The box's
own **rootfs does persist** between calls (any file written stays
there, verified directly — see below), but no container *process*
itself survives between separate `enter` invocations. A real, honestly
documented limitation, not silently papered over.

## What `enter_spec` builds

Closely mirrors `ociman build`'s own `run_step_spec` (a real, writable
rootless rootfs, the same `podman`-default capability set and seccomp
profile every other real container this project runs gets), simplified
for `ocibox`'s own narrower needs:

* `process.args`: the given `COMMAND`, or (if empty) a detected default
  shell — `/bin/bash` if the box's own rootfs has one, else `/bin/sh`,
  else a clear error naming neither (`default_shell_args`).
* `process.env`: the box's own recorded `env` (captured once at
  `create` time from the source image's `ContainerConfig`, see below),
  falling back to a bare `PATH` if the image declared none — the same
  real `podman`-matching fallback `ociman`'s own
  `DEFAULT_ENV_WHEN_IMAGE_DECLARES_NONE` already established, kept as
  its own small duplicate here.
* `process.cwd`: the real host `$HOME` if it's bind-mounted (see
  below), else the box's own recorded `working_dir`, else `/`.
* `process.user`: left at `User::default()`'s own `0`/`0` — root
  *inside* the rootless-mapped user namespace, matching every other
  command in this project with no `--user` equivalent yet. A real
  host-user-account setup inside the rootfs, unlike real `distrobox
  enter`'s own init script, is out of scope for this first slice.
* `process.terminal = false`: no PTY allocation, a real,
  already-documented, project-wide gap (`oci_runtime_core`'s own
  module doc comment) — `ociman run` itself has no `-t` support
  either, so this is consistent with existing scope, not a new
  limitation.
* A `$HOME` bind mount (`Mount { kind: "bind", options: ["rbind"] }`)
  is added **only if** `$HOME` resolves to a real, existing host
  directory — deliberately conditional (unlike real `distrobox enter`'s
  own unconditional host-home bind mount, which also creates a
  matching host user account inside the rootfs first, which this
  project doesn't do), so `enter` still works with no usable `$HOME`
  at all.

## `BoxRecord` gained `env`/`working_dir`

Captured once at `create` time from the resolved image's own
`ContainerConfig`, rather than re-read from the image's config at
`enter` time — the source image could have since been removed from the
store entirely (`ociman rmi` + `prune`) without that affecting an
already-created box at all. Both fields are `#[serde(default)]`, so
`box.json` files from before this increment still deserialize fine
(empty env, no working dir).

## Exit-code forwarding

`ocibox`'s own `main()` normally goes through
`oci_cli_common::run_main(|| anyhow::Result<()>)`, which maps
`Ok(())` to exit code 0 unconditionally — wrong for `enter`, where exit
code 0 must mean "the command *inside the box* exited 0", not merely
"`ocibox` itself didn't error". `cmd_enter` bypasses this the exact
same way `ocirun run`'s own `cmd_run` already does (`bin/ocirun/src/
main.rs`, confirmed by direct source read, not assumed): call
`std::process::exit(exit_code)` directly with the real code
`oci_runtime_core::launch::run` returns, from inside the `run_main`
closure, before it ever returns.

## Verified by hand

Against a real created box (`busybox`, pulled via `ociman pull`):

* An explicit command (`/bin/echo hello`) runs and its real output
  appears.
* Exit codes forward correctly for both success and a nonzero exit
  (`/bin/sh -c 'exit 7'` → `ocibox enter` itself exits `7`).
* No `COMMAND` given falls back to `/bin/sh` (this box's own busybox
  rootfs has no `/bin/bash`) and runs interactively.
* A file written in one `enter` invocation (`echo ... > /tmp/marker.txt`)
  is still there and readable in a wholly separate, later `enter`
  invocation — confirming rootfs persistence despite no process
  persistence.
* With `$HOME` set to a real directory, `ls $HOME`/`cat
  $HOME/canary.txt` inside the box see the real host directory's
  contents.
* `ocibox enter <unknown-name>` is a clear `no such box` error, exit
  code 1.
* `ocibox rm` afterward cleans up normally; no stray mounts or loop
  devices left behind (`mount | grep /tmp/.tmp`, `losetup -a | grep
  verity` both empty).

## Tests

Five new integration tests in `tests/tests/ocibox_enter.rs`: exit-code
forwarding (success and nonzero), default-shell detection, rootfs
persistence across two separate `enter` calls, real `$HOME` bind-mount
visibility, and a clear error for an unknown box name. All prior
`ocibox create`/`list`/`rm` tests continue to pass unchanged.

Full `cargo build --workspace --locked`/`cargo test --workspace
--locked` (2 clean runs, 88/88 result blocks — one more than
before, the new test binary)/`cargo fmt --all --check`/`cargo clippy
--workspace --all-targets --locked -- -D warnings`/`python3
ci/guards.py`/`cargo deny check`/`bash ci/native-ci.sh` all clean. No
performance regression (`ociman run --rm`, ~69ms, consistent with
prior measurements — this change adds a new `oci-runtime-core`
dependency to `ocibox` only, `ociman`'s/`ocirun`'s own code is
untouched).

## What this doesn't do yet

No persistent background container reusable across separate `enter`
calls (see above); no real host-user-account matching inside the
rootfs; no X11/Wayland/audio/nvidia passthrough; no PTY allocation (a
pre-existing, project-wide gap); no `/etc/hosts`/`resolv.conf`
synthesis (unlike `ociman run`, `ocibox enter` is scoped closer to
`ocirun run`'s own simplicity); `ocibox stop`, `export`, `upgrade`, and
real cross-session persistence are all still ahead.
