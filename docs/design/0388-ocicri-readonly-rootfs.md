# Design note 0388: `ocicri CreateContainer` honors `readonly_rootfs`

Status: implemented
Scope: `bin/ocicri/src/bundle.rs`, `bin/ocicri/src/runtime_service.rs`,
`tests/tests/ocicri_container.rs`, `README.md`.

## What this closes

A real, previously-undetected correctness gap in the same shape `0365`
already fixed for `run_as_user`: `ContainerConfig.linux.security_
context.readonly_rootfs` was never read anywhere in `ocicri` at all —
worse, `build_spec` *actively forced* `root.readonly = false`
unconditionally, regardless of what the request actually asked for. A
pod that explicitly requests a read-only root filesystem (a common,
often policy-enforced request — e.g. Kubernetes's own Pod Security
Standards "Restricted" profile) silently got a fully writable one
instead, a real divergence between what a pod spec asked for and what
this runtime actually did.

## Real, checked-directly confirmation

- `crates/oci-cri-types/proto/api.proto` line 1059:
  `LinuxContainerSecurityContext.readonly_rootfs` (field 7) — the
  container-level field (distinct from `LinuxSandboxSecurityContext`'s
  own field 4 of the same name, which this project has no equivalent
  handling for either, out of scope here since sandboxes have no live
  process of their own, `docs/design/0233`).
- `~/git/cri-o/internal/factory/container/container.go`: `ReadOnly()`
  is a one-line `if sc.GetReadonlyRootfs() { return true }` (else the
  server-wide default); `~/git/cri-o/server/container_create_linux.go`
  line ~774: `specgen.SetRootReadonly(ctr.ReadOnly(...))` — a direct,
  unconditional passthrough, confirming this is a real, simple field
  with no other interacting behavior (unlike `privileged`, which the
  same proto file's own doc comment says implies several *other*
  fields are overridden too — deliberately not attempted here).

## Implementation

- `oci_spec_types::runtime::Root::readonly` already exists and is
  exactly the right knob — no new spec-type work needed, unlike
  `apparmor`/`selinux_options` (which have no representation in this
  project's spec types at all).
- `bundle::CriProcessConfig` gains `pub readonly_rootfs: bool`,
  following the exact same "resolve every CRI-level input up front,
  thread it through this one struct" shape `hostname`/`working_dir`/
  `mounts` already established.
- `build_spec`'s hardcoded `readonly = false` becomes `readonly =
  cri.readonly_rootfs` — the smallest possible change that both keeps
  today's writable-by-default behavior for the common, unconfigured
  case (`readonly_rootfs` defaults to `false` in the proto) and
  genuinely honors an explicit `true` request.
- `runtime_service.rs`'s `create_container` reads `security_context.
  readonly_rootfs` (defaulting to `false` when no security context is
  given at all, via `Option::is_some_and`) right next to the existing
  `validate_run_as_user` call, before `bundle::prepare` ever extracts
  a single layer — the same "resolve every CRI-level input up front"
  ordering every other field already follows.

## Tests

Two new unit tests in `bin/ocicri/src/bundle.rs`:
`build_spec_honors_an_explicit_readonly_rootfs_request` (the
previously-broken `true` case) — the existing `build_spec_applies_
cri_precedence_for_env_and_cwd`/`build_spec_falls_back_to_image_cwd_
then_root_and_default_path` tests already cover the `false` default,
now updated with the new struct field.

One new integration test in `tests/tests/ocicri_container.rs`:
`create_container_readonly_rootfs_sets_root_readonly_in_the_real_
spec` — checked the same host-independent way `ociman_run.rs`'s own
`run_read_only_sets_root_readonly_in_the_real_spec` checks its
identical `--read-only` flag (reading the actual generated
`config.json` back out), not by asserting a real in-container write
attempt fails: that test's own doc comment already documents, from a
real prior investigation, that remounting `/` read-only can silently
no-op under this project's own rootless "fake root in a userns" model
on some hosts (the same real `CAP_SYS_ADMIN`-in-the-owning-namespace
limitation `oci_runtime_core::launch`'s own `RemountReadonly` handler
already tolerates for `/sys`), so a real write-attempt assertion here
would be exactly as host-dependent, not a stronger check — confirmed
directly this turn: an initial version of this test asserting a real
write failure passed the spec-generation half but failed the actual
write-rejection half on this exact dev host, matching that
established precedent precisely. Also asserts the contrast case (no
security context at all keeps the existing writable-by-default
behavior unchanged) as a regression guard for the exact bug this
closes. All existing tests across `ocicri_container.rs` (27
pre-existing) and `bundle.rs`'s own module tests continue to pass
unmodified.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures), `python3
ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh`, `bash
ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg -r` round trip).
This change touches only `bin/ocicri`, which `ci/bench.sh` doesn't
measure at all (`ocicri` appears only in a comment there, not in any
timed command) — no benchmark re-run needed.

## Deliberately still out of scope

Every other `LinuxContainerSecurityContext` field surveyed alongside
this one stays a real, honest gap (most already a loud
`Status::unimplemented` via `validate_run_as_user`, or silently
absent): `capabilities.add_capabilities`/`drop_capabilities`,
`privileged` (currently silently ignored with no error at all — a
candidate for its own future increment, mirroring `validate_run_as_
user`'s existing loud-rejection pattern), `selinux_options`/
`apparmor` (no spec-type representation at all), `supplemental_
groups` (would need the same rootless-mapping rejection `run_as_
group` already has), `no_new_privs` (this project's own existing
hardcoded-`true` default is already the stricter posture), and
`masked_paths`/`readonly_paths` (a request's own explicit list is
never merged onto the existing defaults). None of these interact with
`readonly_rootfs`.
