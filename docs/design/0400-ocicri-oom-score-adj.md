# Design note 0400: `ocicri CreateContainer` honors `resources.oom_score_adj`

Status: implemented
Scope: `bin/ocicri/src/bundle.rs`, `bin/ocicri/src/runtime_service.rs`,
`tests/tests/ocicri_container.rs`, `README.md`.

## What this closes

`ContainerConfig.linux.resources.oom_score_adj` (CRI field 5, `int64`,
proto doc comment "Default: 0 (not specified)") is a real field
kubelet sets, but `linux_container_resources_to_oci` never read it —
confirmed the only hit for `oom_score_adj` anywhere in `bin/ocicri/
src/*.rs` before this change was a doc comment admitting the gap
("`oom_score_adj`... have no home yet"). Meanwhile `oci_spec_types::
runtime::Process.oom_score_adj: Option<i32>` and `oci_runtime_core::
oom::apply` already exist and are already wired into `launch.rs`'s
`ChildSetup::run` (`0394`, for `ociman run/create --oom-score-adj`) —
no new runtime primitive needed at all, only CRI-level plumbing.

## Real, checked-directly confirmation

- `~/git/cri-o/internal/factory/container/container.go`'s own
  `SpecSetLinuxContainerResources` (called from `CreateContainer`'s
  own spec-generation path): `specgen.SetProcessOOMScoreAdj(int(
  resources.GetOomScoreAdj()))` — a direct, unconditional passthrough
  at container-creation time only.
- `~/git/cri-o/server/container_update_resources.go`'s own
  `toOCIResources` has a literal `// TODO(runcom): OOMScoreAdj is
  missing` comment — confirming real cri-o itself never applies this
  on a later `UpdateContainerResources` call either. Scoping this to
  `CreateContainer` only (matching `0394`'s own already-established
  "creation time only, never `exec`" scope) is therefore not a
  narrowing versus upstream — it's an exact match.

## Implementation

- `CriProcessConfig` (`bundle.rs`) gains `pub oom_score_adj:
  Option<i32>`.
- `runtime_service.rs`'s `create_container` resolves it up front,
  right next to the existing `resources` resolution: a literal proto
  `0` maps to `None` (leaves the process's own real, inherited value
  untouched) rather than an explicit, forced-to-zero write — matching
  this project's own already-established convention for every other
  `LinuxContainerResources` field on this same struct (`cpu_shares`/
  `cpu_period`/... all documented "Default: 0 (not specified)" and
  already treated that way), a deliberate divergence from real
  cri-o's own literal unconditional passthrough that changes nothing
  for the overwhelmingly common "not specified" case and is strictly
  more consistent for the rare explicit-`0` one.
- `build_spec` (`bundle.rs`) writes it straight onto `process.
  oom_score_adj` — the exact same field `ociman run/create
  --oom-score-adj` already writes and `oci_runtime_core::oom::apply`
  already reads back out at container-creation time.
- Fixed a stale doc comment on `update_container_resources` along the
  way: it still claimed `unified` "has no home yet" and was "honestly
  ignored," which stopped being true the moment `0398` wired
  `apply_unified` into this exact function — corrected to describe
  the real, current behavior (`unified` applied; `oom_score_adj`
  deliberately *not* re-applied here, for the reason above;
  `hugepage_limits` still has no home).

## A real, previously-unconsidered exec-vs-create-time distinction, found while verifying this end to end

The first version of this change's own integration test read back
`/proc/self/oom_score_adj` from *inside* an `ExecSync`'d process — the
same technique `ociman run --oom-score-adj`'s own equivalent test
uses — and failed, reading back `0` instead of the requested value.
Root cause: `ociman run`'s own test's timed command genuinely *is*
the container's own init process, `execve`d in place (which inherits
whatever `oom_score_adj` was already written for that same pid before
`exec`); `ocicri`'s `ExecSync`, by contrast, is a *separate*, freshly
forked process only joining the target container's namespaces (`0240`
's own `__exec` re-exec helper) — never the one process `oom::apply`
actually adjusted at creation time, so its own `/proc/self` reads back
the exec helper's own, unrelated, default `0` regardless of whether
this feature works at all. Fixed by reading `/proc/1/oom_score_adj`
instead — the container's own real init process, addressable by its
pid-namespace-relative pid `1` from inside any process that has joined
the same pid namespace, which `ExecSync` does. Not a bug in this
change's own implementation, just a sharper test than the naive
"reuse the `ociman` pattern verbatim" first draft — worth documenting
here since the same pitfall would silently produce a false negative
(or, worse, a false positive with the wrong assertion value) for any
future `ocicri` test trying to verify a create-time `process.*` field
via `ExecSync`.

## Tests

Two new unit tests in `bundle.rs` (`build_spec_writes_an_explicit_
oom_score_adj_into_the_spec`, `build_spec_without_an_explicit_
oom_score_adj_leaves_it_unset`) plus the 8 pre-existing `CriProcessConfig`
test literals updated with the new field. One new end-to-end
integration test, `create_container_oom_score_adj_sets_a_real_value`
— a real started container's own `/proc/1/oom_score_adj`, read via a
real `ExecSync`, for the reason above. All existing tests continue to
pass unmodified (36/36 in `ocicri_container.rs`).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures), `python3
ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh`, `bash
ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg -r` round trip).
This change touches `launch.rs` not at all (the existing `oom::apply`
call site and its cost are completely unchanged; this is purely new
CRI-level plumbing feeding an already-existing spec field) — no
benchmark re-run needed.

## Deliberately still out of scope

`hugepage_limits` (no hugetlb support anywhere in this project) and
`unified` re-verification (already covered by `0398`) remain the only
other unmapped `LinuxContainerResources` fields, matching this
project's own already-narrower-than-the-full-spec `oci_runtime_core::
cgroups` scope. `selinux_options`/`apparmor`/`add_ambient_capabilities`
remain unread, each a real, separate, unrelated gap, matching
`0388`-`0399`'s own carried-forward "still out of scope" list.
