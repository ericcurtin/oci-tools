# Design note 0475: `ocicri` `security_context.no_new_privs`

Status: implemented
Scope: `bin/ocicri/src/bundle.rs`, `bin/ocicri/src/runtime_service.rs`,
`tests/tests/ocicri_container.rs`.

## What this closes

Every CRI container `ocicri` launched got `process.no_new_privileges`
hardcoded to `true` regardless of what the request actually asked
for — `build_spec` never read `security_context.no_new_privs` at
all, silently leaving `Spec::example()`'s own hardcoded `true`
default in place unconditionally.

## A previously-wrong assessment, corrected

`docs/design/0388-ocicri-readonly-rootfs.md`'s own "still out of
scope" section had already noticed this gap and *knowingly deferred*
it, reasoning that "this project's own existing hardcoded-`true`
default is already the stricter posture." Re-examined this
increment and found that reasoning doesn't hold up: `no_new_privs:
false` is the common real-world default (Kubernetes' own
`AllowPrivilegeEscalation` defaults to `true` unless a Pod Security
Standard explicitly forces it), and ordinary container images
routinely rely on setuid binaries (`sudo`, `ping`, `mount`, ...) that
break under `no_new_privileges=true`. Forcing every CRI container to
the strict posture regardless of the request is a real behavioral
divergence from upstream `cri-o`, not a safe simplification — a
genuine correctness/compatibility bug, the same shape as `0365`/
`0388`/`0389`, not an accepted narrower scope. Documented here
transparently rather than silently corrected, matching this
project's own established convention for a previously-wrong claim
(see `0471`'s identical correction to `0470`).

## Real, checked-directly confirmation

- `crates/oci-cri-types/proto/api.proto:1068-1070`:
  `LinuxContainerSecurityContext.no_new_privs` (field 11), doc
  comment: *"no_new_privs defines if the flag for no_new_privs should
  be set on the container."* — a plain proto3 `bool`, default
  `false`.
- `~/git/cri-o/internal/factory/container/container.go:842-844`
  (`SpecSetPrivileges`): real cri-o's own direct, unconditional
  `specgen.SetProcessNoNewPrivileges(securityContext.
  GetNoNewPrivs())` — a plain passthrough. The surrounding lines
  (831-841) only ever *log a warning* when this would have no real
  effect (a `CAP_SYS_ADMIN` bounding capability, or a privileged
  container) — never a silent behavior override, confirming this is
  a genuine passthrough field, not one real cri-o itself second-
  guesses.

## Implementation

Exactly the same shape as the `readonly_rootfs` (`0388`)/
`oom_score_adj` (`0400`) precedents — pure plumbing through an
already-fully-working shared primitive, no new architecture:

- `CriProcessConfig` (`bundle.rs`) gains `pub no_new_privs: bool`.
- `build_spec`: `process.no_new_privileges = cri.no_new_privs;`
  right where `process.oom_score_adj = cri.oom_score_adj;` already
  sits.
- `runtime_service.rs`'s `create_container`: resolves `no_new_privs`
  the identical way `readonly_rootfs` is already resolved
  (`config.linux.security_context.no_new_privs`, defaulting to
  `false` when absent — the proto's own documented default), threaded
  into the `CriProcessConfig` literal.

No runtime-launch-side change needed at all: `oci_runtime_core::
launch`/`identity` already fully implements applying `process.
no_new_privileges` correctly (in the right order relative to
seccomp/capabilities, `docs/design/0013`/`0191`) for every other
binary that writes this same spec field.

## Tests

Two new unit tests in `bundle.rs` (`build_spec_honors_an_explicit_
no_new_privs_false_request`/`..._true_request`) plus one new end-to-
end integration test in `tests/tests/ocicri_container.rs`
(`create_container_no_new_privs_false_clears_the_default_true_in_
the_real_spec`, checked against the real generated `config.json`,
matching `create_container_readonly_rootfs_sets_root_readonly_in_
the_real_spec`'s own identical convention) — caught and fixed a real
mistake in this test's own first draft before landing: `Process::
no_new_privileges`'s own `#[serde(skip_serializing_if = "std::ops::
Not::not")]` omits the JSON field entirely when `false` (the OCI
spec's own "absence means false" convention), so asserting a literal
`false` value fails; the correct regression guard is `assert_ne!`
against `true`, the same "contrast against the wrong value" shape
`readonly_rootfs`'s own sibling test already uses. All 40 unit tests
in `bundle.rs`'s own test module pass (38 prior + 2 new); all 39
integration tests in `ocicri_container.rs` pass (38 prior + 1 new).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (121 test-result
blocks, 0 failures on the first attempt), `python3 ci/guards.py`
(clean), `cargo deny check` (clean), `bash ci/native-ci.sh` (clean,
121/121 on the first attempt), `bash ci/build-deb.sh` (clean, real
`dpkg -i`/`--version`/`dpkg -r` round trip on the first attempt). No
benchmark re-run needed: `ci/bench.sh` never exercises `ocicri` at
all, and this change touches no hot startup path — a per-container
spec-construction addition, not the launch mechanism itself.
