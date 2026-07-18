# Design note 0003: runtime-spec types and `ocirun spec` (milestone 3, part 1)

Status: implemented (first increment of milestone 3)
Scope: `oci_spec_types::runtime`; `ocirun spec`.

Milestone 3's full scope (`oci-runtime-core` + the rest of `ocirun`'s
command set: `create`/`start`/`state`/`kill`/`delete`/`exec`/`run`/
`features`, plus `ociman run/exec/ps/logs` rootless) is tracked separately
and remains "—" in the README milestone table; this note covers only the
first, self-contained increment: the `config.json` types every later piece
of milestone 3 will consume, plus the one `ocirun` subcommand that only
needs those types (no namespaces, cgroups, or process execution yet).

## Why start here

`ocirun create`/`run` need to *parse* `config.json`; `ocirun spec` needs to
*produce* one. Shipping the runtime-spec types and the read-only/
side-effect-free half of that pair first — fully tested against real
`runc`/`crun` output — gives the actual container-creation work (next
increment) a data model that is already proven correct, instead of
discovering schema mistakes while also debugging namespace/cgroup code.

## `oci_spec_types::runtime`

Deliberately partial: exactly the fields `Spec::example()` (the `runc
spec` default bundle) and `Spec::into_rootless()` (`specconv.ToRootless`)
touch — `Process`/`User`/`LinuxCapabilities`/`PosixRlimit`, `Root`,
`Mount`, `Linux`/`LinuxNamespace`/`LinuxIdMapping`, and a `LinuxResources`
with only `devices` (the one resource the default spec sets). Full cgroup
resource limits (memory/cpu/pids/block-IO/huge-pages/network), seccomp,
hooks, `IntelRdt`, personality, and scheduler/IO-priority are not modeled
yet — they land with actual container creation, when there is real code
to exercise them against.

**Verified against the real thing, not just re-derived from the Go
source**: `crates/oci-spec-types/tests/fixtures/{runc,crun}-spec.json` are
captured verbatim from `runc spec` (runc 1.3.4, spec 1.2.1) and
`crun spec` (crun 1.14.1) on this project's reference distro. A test
parses each fixture and asserts it round-trips (and, for runc, that it is
field-for-field identical to `Spec::example()` modulo the intentionally
different hostname). This is the same "verify against the real ecosystem
tool" approach milestone 2 used for manifest parsing.

One simplification worth flagging: `Spec::example()` always includes the
cgroup namespace. Real `runc spec` only adds it when the host is cgroup-v2
unified (`cgroups.IsCgroup2UnifiedMode()`), because runc still supports
cgroup v1 hosts. oci-tools' supported distros (CentOS Stream 10, Ubuntu
26.04) are cgroup-v2-unified by default, and the project doesn't aim to
support legacy cgroup v1 hosts at all, so the conditional collapses to
"always" — documented in the function's doc comment rather than silently
diverging from upstream behavior.

## `ocirun spec`

Matches `runc spec`'s behavior: writes `config.json` in the bundle
directory (`--bundle`/`-b`, defaulting to the current directory), refuses
to overwrite an existing one, `--rootless` applies the same
namespace/mount/resource transformation runc's `--rootless` flag does
(computing the real effective uid/gid via the new
`oci_cli_common::identity` helper — `/proc/self/status`, no new
dependency), and serializes with tab indentation and `0o666` permissions
to match `runc`'s `MarshalIndent(spec, "", "\t")` /
`os.WriteFile(..., 0o666)` as closely as JSON-key-ordering allows (object
key order itself differs in a couple of places — Rust struct field
declaration order drives serde's output order, and it doesn't exactly
match the Go struct's — which is semantically irrelevant for JSON but
worth naming so nobody "fixes" it against a byte-diff later).

Tests: `oci_spec_types::runtime`'s unit tests cover the data model in
isolation; `tests/tests/ocirun_spec.rs` (new) exercises the actual built
binary — default spec contents, the rootless transformation, the
bundle-directory flag, and the overwrite guard — following the same
built-binary-integration-test pattern as `tests/tests/smoke.rs` (whose
`bin_path` helper moved to `oci_tools_tests::bin_path` so both files share
one implementation instead of two copies).

## Decisions and risks

* **Target `ociVersion` 1.2.1, not the upstream-vendored 1.3.0.** The
  installed `runc` binary (1.3.4, the version this project's CI images and
  this host actually run) emits `1.2.1`; matching what real deployments
  produce today matters more than tracking the latest spec source tree.
  Revisit when the distros' packaged runc moves to a newer spec version.
* **No hooks, no seccomp, no full resource limits yet** — see above; this
  is intentional, not an oversight, and every omitted field has a doc
  comment saying so.
