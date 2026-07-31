# Design note 0390: `ocicri CreateContainer` wires `linux.resources`

Status: implemented
Scope: `bin/ocicri/src/bundle.rs`, `bin/ocicri/src/runtime_service.rs`,
`bin/ocicri/src/launcher.rs`, `tests/tests/ocicri_container.rs`,
`README.md`.

## What this closes

A real, significant correctness gap, arguably bigger in practical
impact than any single missing `security_context` field (`0388`,
`0389`): `ContainerConfig.linux.resources` was never wired into a
container's own generated spec at `CreateContainer` time at all — the
only place `linux_container_resources_to_oci` (the CRI→OCI
`LinuxResources` mapping function) was ever called was
`update_container_resources`, which requires an already-`Running`
container and writes cgroup files directly. A CRI container created
with an explicit `LinuxContainerResources` (CPU shares/quota/period/
cpuset, memory limit/swap) ran **completely unconstrained** until/
unless kubelet later issued a separate `UpdateContainerResources`
call — which, for ordinary pods without in-place vertical scaling,
kubelet normally never does. Resources are expected to take effect at
creation, defeating basic resource isolation/QoS guarantees the whole
CRI resource model is built on.

## Real, checked-directly confirmation

- `crates/oci-cri-types/proto/api.proto`: `LinuxContainerConfig.
  resources` (field 1) is a real, always-present-shape input alongside
  `security_context` (field 2) — every `CreateContainer` call kubelet
  makes for a pod with resource requests/limits sets this.
- This project's own already-existing precedent made the fix
  unusually cheap: `ociman`'s own launch path
  (`bin/ociman/src/main.rs:7410-7419`) already reads `bundle.spec.
  linux.resources` straight out of the already-generated bundle spec
  and threads it into `CgroupSetup::Systemd { resources: ... }`
  (`.map(Box::new)`). `ocicri`'s own `launcher.rs` (the structurally
  identical call site launching a *CRI* container) simply hardcoded
  `resources: None` instead of the equivalent read.

## Implementation

- `bundle::CriProcessConfig` gains `pub resources: Option<oci_spec_
  types::runtime::LinuxResources>`, already translated by the caller
  via the pre-existing `linux_container_resources_to_oci`.
- `build_spec` writes `linux.resources = cri.resources.clone()` right
  after setting `linux.seccomp` — `None` (the common, unconfigured
  case) leaves the spec's own resources absent, exactly matching
  today's existing behavior; `Some(resources)` writes it in for real.
- `runtime_service.rs`'s `create_container` resolves `config.linux.
  resources.as_ref().map(linux_container_resources_to_oci)` up front,
  next to the existing `readonly_rootfs` resolution, before `bundle::
  prepare` ever extracts a layer.
- `launcher.rs`'s hardcoded `resources: None` becomes `bundle.spec.
  linux.as_ref().and_then(|l| l.resources.clone()).map(Box::new)` —
  the identical expression `ociman`'s own call site already uses
  verbatim, reading back the same field `build_spec` just wrote.

## Tests

Two new unit tests in `bin/ocicri/src/bundle.rs`:
`build_spec_writes_an_explicit_resources_request_into_the_spec` (a
real memory limit + CPU shares round-tripped into `spec.linux.
resources`) and `build_spec_leaves_resources_absent_when_none_are_
requested` (a regression guard for the exact bug this closes). The
three pre-existing `build_spec` tests updated with the new struct
field (`resources: None`).

One new integration test in `tests/tests/ocicri_container.rs`:
`create_container_resources_take_effect_at_creation_without_a_later_
update_call` — a real `CreateContainer` with an explicit
`memory_limit_in_bytes`/`cpu_quota`/`cpu_period`, started, with **no**
`UpdateContainerResources` call anywhere in the test, checked the same
way `update_container_resources_changes_the_real_live_cgroup` checks
its own identical fields: real `memory.max`/`cpu.max` cgroup files
read back directly (via `ContainerStatus`'s own verbose pid +
`oci_runtime_core::cgroups::cgroup_dir_for_running_pid`), not just
trusting an RPC's own content-free success response. All existing
tests across `ocicri_container.rs` (30 pre-existing) and `bundle.rs`'s
own module tests continue to pass unmodified.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures), `python3
ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh`, `bash
ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg -r` round trip).
This change touches only `bin/ocicri`, which `ci/bench.sh` doesn't
measure at all — no benchmark re-run needed.

## Deliberately still out of scope

`UpdateContainerResources` itself is unchanged and still works
exactly as before — this note only ensures a container's *initial*
resources are no longer silently dropped at creation, matching what a
kubelet-driven workflow already assumes happens. Every
`LinuxContainerSecurityContext` field surveyed alongside `readonly_
rootfs`/`privileged` (`0388`'s/`0389`'s own "deliberately still out of
scope" sections) remains a real, separate, unrelated gap.
