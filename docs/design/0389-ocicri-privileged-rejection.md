# Design note 0389: `ocicri CreateContainer` rejects `privileged: true`

Status: implemented
Scope: `bin/ocicri/src/runtime_service.rs`, `tests/tests/ocicri_container.rs`,
`README.md`.

## What this closes

A real, previously-undetected gap, worse in shape than every other
unsupported `LinuxContainerSecurityContext` field this codebase
already checks: `security_context.privileged` was never read
*anywhere at all* in `ocicri` — not honored, not rejected. A workload
explicitly asking for privileged access (a real, legitimate request
some pods make — e.g. CNI plugin pods, some CSI drivers, host-level
debugging tools) silently got an ordinary, confined container instead,
with no error telling it so. Every other unsupported request this
codebase handles (`run_as_username`, a non-zero `run_as_user`/
`run_as_group`, non-default mount propagation, SELinux relabeling,
recursive read-only, UID/GID mappings, `readonly_rootfs`'s own now-
fixed 0388) gets a clear, honest `Status::unimplemented`/
`invalid_argument` rather than a silent no-op; `privileged` was the
one glaring exception.

## Real, checked-directly confirmation

- `crates/oci-cri-types/proto/api.proto`'s own
  `LinuxContainerSecurityContext.privileged` doc comment (field 2)
  spells out exactly how large real privileged-mode support actually
  is: it implies "all capabilities are added," "sensitive paths...
  are not masked," "sysfs and procfs mounts are mounted RW," "AppArmor
  confinement is not applied," "Seccomp restrictions are not
  applied," "device cgroup does not restrict access to any devices,"
  "all devices from the host's /dev are available," and "SELinux
  restrictions are not applied" — eight distinct behavioral changes
  at once, not one field mapping onto one existing OCI spec knob the
  way `readonly_rootfs` (0388) does.
- `~/git/cri-o/server/container_create_linux.go`'s own
  `getSpecGen`/`specSetDevices`/`addSysfsMounts` (lines ~774-900) and
  `~/git/cri-o/internal/factory/container/container.go`'s own
  `SetPrivileged` confirm real cri-o genuinely implements every one of
  those eight behaviors — a materially larger increment than this
  project's own established "one field, one existing primitive" shape
  for a single design note, and not attempted here.
- No cri-o config path returns a distinct "privileged not allowed"
  error by default either (checked directly: no match for any such
  message in `server/`/`internal/factory/container/`), so there's no
  existing real error string to port verbatim the way `validate_run_
  as_user`'s own messages already do — this note's own message is
  this project's own, matching its established phrasing
  (`"... is not yet supported"`, the same wording `run_as_username`'s
  own rejection already uses).

## Implementation

- New `fn validate_privileged(security_context: Option<&cri::
  LinuxContainerSecurityContext>) -> Result<(), Status>` right next to
  `validate_run_as_user` in `runtime_service.rs`: `None` or
  `Some(sc) if !sc.privileged` succeeds (the common, unconfigured
  default, and the `privileged: false` case some clients set
  explicitly); `Some(sc) if sc.privileged` is a clear
  `Status::unimplemented("privileged containers are not yet
  supported")`.
- Called from `create_container` immediately after `validate_run_as_
  user`, before `bundle::prepare` ever extracts a single layer — the
  same "resolve every CRI-level input up front, before any real work
  happens" ordering every other validation already follows.

## Tests

One new unit test in `runtime_service.rs`'s own `mod tests`:
`validate_privileged_rejects_an_explicit_true_but_allows_everything_
else` (a pure function, cheap to test directly, no server/socket
needed). One new integration test in `tests/tests/ocicri_container.rs`:
`create_container_rejects_privileged_clearly_but_allows_the_default`
— a real `CreateContainer` RPC over a real Unix socket with
`privileged: true` genuinely rejected with the exact expected
`Status::unimplemented` message, and an otherwise-identical
`privileged: false` request still succeeding. All existing tests
across `ocicri_container.rs` (29 pre-existing) and `runtime_service.
rs`'s own module tests continue to pass unmodified (one unrelated,
pre-existing flake in `exec_sync_runs_commands_in_a_running_
container` under full parallel load, confirmed unrelated to this
change and passing in isolation and on a full clean re-run).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures), `python3
ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh`, `bash
ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg -r` round trip).
This change touches only `bin/ocicri`, which `ci/bench.sh` doesn't
measure at all — no benchmark re-run needed.

## Deliberately still out of scope

Real, full privileged-mode support itself (the eight behaviors listed
above) remains unimplemented — this note only replaces a silent,
previously-undetected no-op with an honest error, the same
"correctness over completeness" priority `0365`/`0388` already
established. Every other `LinuxContainerSecurityContext` field
surveyed alongside `readonly_rootfs`/`privileged` (`0388`'s own
"deliberately still out of scope" section) remains a real, separate
gap unrelated to this change.
