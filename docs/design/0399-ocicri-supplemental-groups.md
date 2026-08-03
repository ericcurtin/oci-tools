# Design note 0399: `ocicri CreateContainer` rejects a non-zero `security_context.supplemental_groups`

Status: implemented
Scope: `bin/ocicri/src/runtime_service.rs`, `tests/tests/ocicri_container.rs`, `README.md`.

## What this closes

`ContainerConfig.linux.security_context.supplemental_groups` (a real
CRI field, `Vec<i64>`) was flagged as a real, still-open gap in every
one of `validate_run_as_user`/`validate_privileged`'s own sibling doc
comments since `0388` ("the same rootless-mapping rejection
`run_as_group` already has"), but never actually given its own
validator — read *nowhere at all*, so a pod's own explicit
`securityContext.supplementalGroups: [1000]` was silently dropped, and
the container quietly ran with no supplemental groups whatsoever. The
exact same shape of bug `0365` already fixed for `run_as_user`/
`run_as_group`.

## Real, checked-directly confirmation

- Generated proto binding: `LinuxContainerSecurityContext.
  supplemental_groups: Vec<i64>` (field 8) — confirmed present,
  confirmed never referenced anywhere in `bin/ocicri/src/*.rs` before
  this change.
- `~/git/cri-o/server/container_create.go`'s own `setupContainerUser`:
  real cri-o calls `specgen.AddProcessAdditionalGid(group)`
  unconditionally for every value in `sc.GetSupplementalGroups()`, no
  validation of its own at all (real cri-o only ever runs rootful, so
  it has no rootless-mapping concern to check in the first place).

## Why this project can't just apply it the way real cri-o does

This project's own containers are rootless-only, and only ever map
container gid 0 to this process's own real egid (`into_rootless`
writes exactly one `LinuxIdMapping{container_id: 0, size: 1}` for both
uid and gid) — the identical constraint `validate_run_as_user`'s own
`run_as_group` check already enforces for the primary gid. A
non-zero supplemental group is exactly as unmappable, for the exact
same reason, so it gets the identical clear, honest
`Status::unimplemented` rather than a silent no-op (worse: silently
running with fewer groups than the pod explicitly asked for, an
availability/permissions surprise no real user would want).

## Implementation

`validate_supplemental_groups`, a new sibling of `validate_run_as_
user`/`validate_privileged` right below the latter: no security
context at all, an empty list, or a list containing only `0` (already
this project's own existing default) all succeed; any other entry —
alone or mixed in with a `0` — returns `Status::unimplemented` naming
the offending value, reusing `run_as_group`'s own message wording for
consistency. Wired into `create_container` right next to the two
existing calls, the same "resolve every CRI-level input up front"
ordering already established there.

## Tests

Two new unit tests (`None`/empty/`[0]` all succeed; `[1000]` and a
mixed `[0, 1000]` both reject naming `1000`). One new end-to-end
integration test in `tests/tests/ocicri_container.rs`, mirroring
`create_container_rejects_privileged_clearly_but_allows_the_default`'s
own two-request shape (a rejected non-zero request, then an allowed
all-zero one against the same running server). All existing tests
continue to pass unmodified (35/35 in `ocicri_container.rs`).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures), `python3
ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh`, `bash
ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg -r` round trip).
This change touches only `ocicri`'s own request-validation path, not
`launch.rs`'s hot create/start path at all — no benchmark re-run
needed, matching `0392`/`0396`'s own identical reasoning for the same
class of change.

## Deliberately still out of scope

`selinux_options`/`apparmor` (no spec-type representation anywhere in
this project at all — a materially bigger addition than a single
existing-field validator) and `add_ambient_capabilities` (real
ambient-capability semantics are a separate, more involved concern)
remain unread, each a real, separate, unrelated gap, matching
`0388`-`0392`'s own carried-forward "still out of scope" list.
