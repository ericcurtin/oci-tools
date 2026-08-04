# Design note 0426: `ociman exec --privileged`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_exec.rs`,
`README.md`.

## What this closes

`ociman exec` had no `--privileged` flag at all — the exec'd
process always inherited exactly the container's own already-running
init process's own capability set, with no way to run a single exec
with a broader one (a real, common operator escape hatch: debugging
inside an otherwise-unprivileged container without restarting it
with `--privileged` from the start).

## Real, checked-directly confirmation

`~/git/podman/libpod/oci_conmon_exec_linux.go`'s own
`setProcessCapabilitiesExec`:

```go
allCaps, err := capabilities.BoundingSet()
if options.Privileged {
    pspec.Capabilities.Bounding = allCaps
} else {
    pspec.Capabilities.Bounding = ctrSpec.Process.Capabilities.Bounding
}
pspec.Capabilities.Inheritable = []string{}
if execUser.Uid == 0 {
    pspec.Capabilities.Effective = pspec.Capabilities.Bounding
    pspec.Capabilities.Permitted = pspec.Capabilities.Bounding
} else if user == c.config.User {
    // (further, narrower elevation for exec'ing as exactly the
    // container's own configured non-root user — see below)
}
```

Three things confirmed directly, not assumed:
1. `--privileged` grants the real, full host bounding set
   (`BoundingSet()`, this project's own `ALL_CAPABILITY_NAMES`) —
   independent of whatever the container itself was created with
   (the two happen to coincide for a container that was itself
   already `run --privileged`, but this flag's real effect doesn't
   depend on that).
2. The inheritable set is **unconditionally** cleared, regardless of
   `--privileged` or exec uid — real podman does this on every exec,
   not just a privileged one (inheritable capabilities only ever
   matter for a real `uid != 0` exec combined with file
   capabilities, a mechanism this project has no equivalent of at
   all).
3. Effective/permitted are only elevated to match the bounding set
   for a real `uid == 0` exec (the default, absent `--user`) — a
   non-root exec's effective/permitted sets are left completely
   untouched by either branch.

The further, narrower elevation for exec'ing as exactly the
container's own configured non-root user (`user == c.config.User`)
is deliberately **not** implemented — a real, honest narrower-first-
slice scope, the same established precedent every other "first
slice" design note in this project already sets.

## Implementation

- `Command::Exec` gains `privileged: bool` (`#[arg(long)]`).
- New `resolve_exec_capabilities(base, privileged, exec_uid) ->
  Option<LinuxCapabilities>` — a direct, checked-against port of the
  Go logic above (minus the deferred third branch), reusing this
  project's own existing `ALL_CAPABILITY_NAMES` (already shared by
  `run --privileged`).
- `cmd_exec` calls it once, after `effective_user` (including any
  `--user` override) is fully resolved, so the `uid == 0` check sees
  the real, final exec uid, not the container's own original one.

## Tests

Seven new unit tests for `resolve_exec_capabilities` (bounding-set
swap, root-uid elevation, non-root-uid non-elevation, unconditional
inheritable-clearing regardless of `--privileged`, ambient left
untouched, and a bundle with no capabilities section at all treated
as empty). One new end-to-end integration test in `tests/tests/
ociman_exec.rs`, `exec_privileged_genuinely_grants_the_full_bounding_
capability_set`: reads the exec'd process's own real `CapBnd` back
from `/proc/self/status` inside the container (the kernel's own
ground truth, not a guess) both with and without the flag, asserting
the privileged one is strictly larger and is a real superset of the
unprivileged one (`--privileged` only ever adds, never removes). All
11 prior tests in `ociman_exec.rs` and all 194 prior unit tests
continue to pass unmodified (12/12 integration, 201/201 unit total).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean, no diff), `cargo clippy --workspace --all-targets --locked --
-D warnings` (needed one `#[allow(clippy::too_many_arguments)]` on
`cmd_exec`, the same established pattern several other functions in
this file already use), `cargo test --workspace --locked` (119
test-result blocks, 0 failures, clean on the second full run — one
earlier attempt hit an unrelated, known, pre-existing `ociman_
logs.rs` follow-streaming host-contention flake, confirmed
environmental via an immediate isolated rerun), `python3 ci/
guards.py`, `cargo deny check`, `bash ci/native-ci.sh` (clean,
119/119), `bash ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg
-r` round trip). Touches only `ociman exec`'s own capability
computation, not any startup/destroy-time hot path — no benchmark
re-run needed.

## Deliberately still out of scope

Real podman's own further, narrower elevation for exec'ing as
exactly the container's own configured non-root user (see above) —
a real, separate, smaller gap left for a future increment if it ever
turns out to matter in practice.
