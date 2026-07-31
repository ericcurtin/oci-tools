# Design note 0396: `ocicri CreateContainer` applies `PodSandboxConfig.linux.sysctls`

Status: implemented
Scope: `bin/ocicri/src/bundle.rs`, `bin/ocicri/src/runtime_service.rs`,
`tests/tests/ocicri_container.rs`, `README.md`.

## What this closes

`PodSandboxConfig.linux.sysctls` — a real, sandbox-(pod-)level CRI
concept, distinct from a container-level one (checked directly:
`LinuxContainerConfig` in the CRI proto has no `sysctls` field of its
own at all) — was never read anywhere in `ocicri`. A pod's own
`securityContext.sysctls` had no effect on any container this project
launched, regardless of what it requested.

## Real, checked-directly confirmation

- `crates/oci-cri-types/proto/api.proto:530-531`:
  `LinuxPodSandboxConfig.sysctls` (a `map<string, string>`), confirmed
  distinct from any container-level field.
- `~/git/cri-o/server/sandbox_run_linux.go:692-693,859-896`
  (`configureGeneratorForSysctls`): real cri-o applies sandbox-level
  sysctls exactly *once*, to its own real infra ("pause") container's
  spec generator — every other container in the same pod shares that
  process's already-configured namespaces, so they inherit the same
  effective kernel-parameter values for free, without any per-
  container work of their own.

## Why this project applies it per-container instead

This project has no real infra process or shared per-pod namespaces
at all yet (`docs/design/0233`, still explicitly deferred) — each
`ocicri`-managed container gets its own fully independent namespaces.
There is no single "sandbox process" to bind a one-time sysctl write
to, and no shared namespace for the effect to propagate across sibling
containers in the same pod the way real cri-o's model provides for
free. This is the exact same shape of gap `0292` (hostname), `0296`
(`/etc/hosts`), and `0297` (`/etc/resolv.conf`) already closed: each of
those is also a genuinely sandbox-level CRI concept this project
resolves independently *per container* instead, since there's no
separate sandbox process to apply it to once. `0396` follows that
same, now well-established precedent.

## Implementation

- `bundle::CriProcessConfig` gains `pub sysctl: BTreeMap<String,
  String>`; `build_spec` writes it straight onto `linux.sysctl`
  (`0395`'s own field), right after the `masked_paths`/`readonly_
  paths` assignment.
- `runtime_service.rs`'s `create_container` resolves `sandbox_config.
  linux.as_ref().map(|l| l.sysctls.clone()...).unwrap_or_default()`
  (converting the proto's own `HashMap` to this project's
  deterministic `BTreeMap`) — read from the *sandbox* config the
  request carries, never from the per-container `config` at all,
  matching the real CRI proto's own field placement.
- No new validation logic needed at all: the exact same shared `oci_
  runtime_core::sysctl::apply` every other `oci_runtime_core::launch`
  caller already goes through (`0395`) validates each key against this
  container's own actually-declared namespaces at real container-start
  time — a `net.*` sysctl requested by a pod is rejected exactly as
  safely here as an explicit `ociman run --sysctl net.*=...` already
  is, with zero extra code.

## Tests

One new unit test in `bin/ocicri/src/bundle.rs`:
`build_spec_writes_the_sandboxs_own_sysctls_into_the_spec`. One new
integration test in `tests/tests/ocicri_container.rs`:
`create_container_applies_the_sandboxs_own_sysctls_to_a_real_running_
container` — a real sandbox-level `kernel.shmmax` sysctl, genuinely
read back via a real `ExecSync` inside a real started container. All
existing tests across `ocicri_container.rs` (33 pre-existing) and
`bundle.rs`'s own module tests continue to pass unmodified.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures), `python3
ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh`, `bash
ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg -r` round trip).
This change touches only `bin/ocicri`, which `ci/bench.sh` doesn't
measure at all — no benchmark re-run needed.

## Deliberately still out of scope

`LinuxPodSandboxConfig`'s own other fields (`cgroup_parent`,
`security_context`, `overhead`, `resources`) remain unread, each a
real, separate, unrelated gap. Every `LinuxContainerSecurityContext`/
`Process`/`Linux` field surveyed in prior notes (`0388`-`0395`'s own
"deliberately still out of scope" sections) remains unchanged too.
