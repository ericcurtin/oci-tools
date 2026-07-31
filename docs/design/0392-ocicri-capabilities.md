# Design note 0392: `ocicri CreateContainer` honors `capabilities.add/drop`

Status: implemented
Scope: `crates/oci-runtime-core/src/identity.rs`, `bin/ociman/src/main.rs`,
`bin/ocicri/src/bundle.rs`, `bin/ocicri/src/runtime_service.rs`,
`tests/tests/ocicri_container.rs`, `README.md`.

## What this closes

`ContainerConfig.linux.security_context.capabilities.add_capabilities`/
`.drop_capabilities` were never read anywhere in `ocicri` — every CRI
container got exactly the same hardcoded real `podman`-default
11-capability set on all three (bounding/effective/permitted) sets,
no matter what a pod's own `capabilities` actually requested.

## Real, checked-directly confirmation

- `crates/oci-cri-types/proto/api.proto`'s own `Capability` message
  (fields 1/2, `add_capabilities`/`drop_capabilities`) — `ambient`
  (field 3) is deliberately out of scope here, see below.
- `~/git/cri-o/internal/factory/container/container.go`'s own
  `SpecSetupCapabilities` (lines ~640-710): confirms `add=["ALL"]`
  combined with an individual `drop` (e.g. `drop=["CHOWN"]`) still
  applies that drop on top of the full set (`kubernetes/
  kubernetes#51980`) — a case this project's own already-existing
  `ociman run --cap-add`/`--cap-drop` merge algorithm (ported from
  real `docker`/`podman`'s own `MergeCapabilities`) already handles
  correctly, confirmed by tracing its own logic rather than assumed.
- The same source also revealed a real, checked-directly cri-o quirk
  deliberately *not* replicated: its own `toCAPPrefixed` helper
  returns an already-`cap_`-prefixed name *verbatim*, un-uppercased
  (`"cap_chown"` stays `"cap_chown"`, never becoming the canonical
  `"CAP_CHOWN"` a real runtime's own capability-name matching expects)
  — this project's own `normalize_capability` always uppercases
  first, so this can never happen here, the same "diverge from a real
  tool's own bug, keep the more correct behavior" precedent already
  established elsewhere (e.g. `0376`).

## Implementation

- **Shared prerequisite**: `normalize_capability`/`normalize_
  capabilities`/`merge_capabilities`/`CAP_ALL` moved out of `ociman`-
  private code into `oci_runtime_core::identity` (next to the
  already-shared `ALL_CAPABILITY_NAMES` these functions already
  depend on) the moment this second, unrelated caller needed the
  identical primitive — the same "shared prerequisite" reasoning
  `oci_runtime_core::etc_hosts`/`resolv_conf` already established for
  their own second callers (`0296`/`0297`). Converted from `anyhow::
  Result` to this crate's own established `io::Result` convention
  (matching `0296`'s own identical conversion), since `oci_runtime_
  core` has no `anyhow` dependency at all. `ociman`'s own call site
  (`bin/ociman/src/main.rs`) now calls `oci_runtime_core::identity::
  merge_capabilities` directly; `anyhow::Error: From<io::Error>`
  means its own `?`-based error propagation needed no changes at all.
  All twelve pre-existing unit tests moved verbatim into `identity.rs`'s
  own test module (byte-for-byte unchanged assertions, matching
  `0296`'s own "verified the moved unit tests pass identically in
  their new home" precedent), plus one new test proving the `add=
  ["ALL"]` + individual-`drop` interaction cri-o's own source
  revealed.
- `bundle::CriProcessConfig` gains `pub capabilities: Vec<String>` —
  already the final, merged list; `build_spec` just writes it onto
  `bounding`/`effective`/`permitted` directly, no merge logic left in
  `bundle.rs` at all.
- `runtime_service.rs`'s `create_container` calls `merge_capabilities`
  up front (base: this project's own real `podman_default_
  capabilities()`, since `privileged: true` never reaches here at all
  — already rejected earlier by `validate_privileged`, `0389`), mapping
  a merge failure (an unknown capability name, or a contradictory
  add/drop request) to a real `Status::invalid_argument` — a client-
  input problem, not the generic `internal` `PrepareError::Other`
  would otherwise map to.

## Tests

Twelve pre-existing capability tests moved into `oci_runtime_core::
identity`'s own test module unchanged, plus one new test:
`merge_capabilities_add_all_with_an_individual_drop_removes_just_
that_one`. Two new unit tests in `bin/ocicri/src/bundle.rs`:
`build_spec_writes_the_given_capabilities_onto_all_three_sets`. One
new integration test in `tests/tests/ocicri_container.rs`:
`create_container_capabilities_add_and_drop_change_the_real_process_
capability_sets` — the same real `/proc/self/status` bitmask-diffing
technique `ocirun_exec.rs`'s own `exec_cap_adds_a_capability_on_top_
of_the_containers_own_default_set` test already established, ported
for `ocicri`'s own real `podman`-default base (computed
programmatically from `podman_default_capabilities()`'s own
documented bit positions, not hand-derived, to avoid a transcription
error): a real started container's own default set, one with `add_
capabilities: [NET_ADMIN]`, and one with `drop_capabilities: [CHOWN]`,
each read back via a real `ExecSync`. `ociman`'s own full `ociman_
run.rs` suite (97 tests) and `oci-runtime-core`'s own full unit test
suite pass completely unmodified, confirming the move changed no
observable behavior.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures; the new
integration test hit this suite's own already-documented pre-exec
race under full parallel load once — confirmed unrelated to this
change by passing in isolation three times and on two full clean
re-runs of the whole file), `python3 ci/guards.py`, `cargo deny
check`, `bash ci/native-ci.sh`, `bash ci/build-deb.sh` (real `dpkg
-i`/`--version`/`dpkg -r` round trip). This change touches the shared
`oci_runtime_core::identity` module (used by `ociman run`'s own
capability computation before a container ever launches, not on the
actual fork/exec hot path `ci/bench.sh` times) — a targeted
`hyperfine` spot-check (`ociman run --rm` vs. `ociman run --rm
--cap-add net_admin --cap-drop chown`) showed no regression, both
~35-37ms, within noise of each other.

## Deliberately still out of scope

`add_ambient_capabilities` (the CRI proto's own third `Capability`
field) is not handled at all — real cri-o's own ambient-capability
handling is a separate, more involved concern (`inheritable`-set
interaction, `addInheritableCapabilities` config toggle) this
increment's scope deliberately excludes, matching `ociman run`'s own
CLI, which has no equivalent flag either. Every other
`LinuxContainerSecurityContext` field surveyed alongside `readonly_
rootfs`/`privileged`/`resources`/`masked_paths`/`readonly_paths`
(`0388`-`0391`'s own "deliberately still out of scope" sections)
remains a real, separate, unrelated gap.
