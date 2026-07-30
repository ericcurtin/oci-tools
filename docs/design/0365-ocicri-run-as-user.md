# Design note 0365: `ocicri CreateContainer` validates `run_as_user`/`run_as_group`

Status: implemented
Scope: `bin/ocicri/src/runtime_service.rs`, `tests/tests/ocicri_container.rs`,
`README.md`.

## What this closes

`bundle.rs`'s own module doc comment (`0237`) named "per-container
`run_as_user`/security-context mapping" as deliberately out of scope
since the very first `CreateContainer` slice. Confirmed by direct
grep: zero occurrences of `run_as_user`/`readonly_rootfs`/
`no_new_privs`/`masked_paths`/`privileged` anywhere in
`bin/ocicri/src/*.rs` before this note — `create_container` never read
`ContainerConfig.linux.security_context` at all.

## The real, checked-directly gap this closes — and why it doesn't need a new spec field

Every container this project's own `ocicri` creates is rootless with
exactly one real uid/gid mapped: container `0`, to this process's own
euid/egid (`Spec::into_rootless`'s own single-entry `uid_mappings`/
`gid_mappings`) — the exact same constraint `ociman run --user`'s own
`resolve_user` already gives a clear, immediate error for instead of
letting a request for anything else silently fail much later, deep
inside `identity::apply`'s own `setresuid(2)`/`setresgid(2)` as a bare
`EINVAL` (found the hard way while adding `--user` support in `0286`).
`ocicri` had no equivalent check at all: a pod's own explicit
`securityContext.runAsUser: 1000` was silently *ignored* entirely, and
the container quietly ran as uid `0` regardless — a real, previously-
undetected divergence from the pod spec's own explicit intent, not
merely an unimplemented feature erroring loudly.

`run_as_user: 0`/`run_as_group: 0` — a real, legitimate request many
pods make explicitly (not just the absence of any request at all) —
already matches this project's own existing default `process.user`
(`Spec::example()`'s own `User::default()`), so accepting it needs no
new field threaded through `CriProcessConfig`/`build_spec` at all:
the only thing that was ever actually missing was the *validation*
that turns a request for anything else into a clear, loud error
instead of a silent no-op.

## Real, checked-directly rules ported

Read `~/git/cri-o/server/container_create.go`'s own `setupContainerUser`
directly:

- `run_as_group` given without `run_as_user`/`run_as_username` is a
  real, immediate error — real cri-o's own exact message ("user group
  is specified without user or username") reused verbatim.
- `run_as_username` takes priority over `run_as_user` when both are
  given (`generateUserString`), resolved against the image's own real
  `/etc/passwd` (`GetUserInfo`) — deliberately **not** implemented at
  all here yet, the same "numeric only, name resolution is a higher-
  level-tool concern" scope `ocirun exec --user`'s own doc comment
  already established: a clear `Status::unimplemented` rather than
  silently ignored.
- Any non-zero `run_as_user`/`run_as_group` is a clear
  `Status::unimplemented`, wording mirroring `ociman`'s own
  `resolve_user` exactly (this rootless runtime cannot map it; a
  subordinate uid/gid range via `/etc/subuid`/`/etc/subgid` would be
  needed for anything else).

Still deliberately out of scope, unrelated to the CRI-requested
`run_as_user`/`run_as_group` this note is actually about: an image's
own declared `USER` (real cri-o's own `imageUser` fallback in
`generateUserString`) is never read or applied for CRI containers
either — a separate, pre-existing gap.

## Verified

New tests in `tests/tests/ocicri_container.rs`:
`create_container_run_as_user_and_group_zero_succeeds`;
`create_container_rejects_unsupported_run_as_user_fields_clearly`
(non-zero uid, non-zero gid, `run_as_username` given at all, and
`run_as_group` without `run_as_user` — each asserted against its own
real gRPC status code and message). All 25 pre-existing
`ocicri_container.rs` tests re-run unmodified and still pass.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures, full clean
run, no flakes), `python3 ci/guards.py`, `cargo deny check`, `bash
ci/native-ci.sh`, `bash ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).
