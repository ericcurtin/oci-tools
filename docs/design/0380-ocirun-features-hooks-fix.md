# Design note 0380: `ocirun features`'s stale `hooks` list

Status: implemented
Scope: `bin/ocirun/src/features.rs`, `README.md`.

## What this closes

`ocirun features`'s own module doc comment (design note 0077) states
its whole purpose is that every list it reports "is built from this
project's own existing, already-tested source of truth ... rather
than a separate, hand-typed copy — so this command can never silently
drift out of sync with what the rest of the codebase actually does."
The `hooks` field was the one exception: a plain, hand-typed
`vec!["prestart", "createRuntime", "poststart", "poststop"]`, with a
comment claiming `createContainer`/`startContainer` were "deliberately
excluded" because they "genuinely aren't executed yet" — true when
0077 was written, but false today.

## Real, checked-directly confirmation

- Design note **0088** ("`createContainer`/`startContainer` hooks",
  landed after 0077) implemented real execution of both hooks:
  `crates/oci-runtime-core/src/launch.rs`'s `ContainerHooks`/
  `run_container_hooks`, called from `mount_pivot_and_exec` at the
  spec's own documented timing (`createContainer` pre-`pivot_root`,
  `startContainer` post-`pivot_root`/pre-`exec`) — confirmed by
  reading the actual call sites, not just the design-doc history.
- `tests/tests/ocirun_hooks.rs` has real, passing, already-existing
  tests exercising both: `create_container_hook_receives_a_creating_
  state_with_host_paths_still_visible`, `start_container_hook_
  receives_a_created_state_and_runs_inside_the_containers_own_rootfs`,
  `a_failing_create_container_hook_aborts_the_container_and_start_
  container_never_runs`, `create_container_runs_before_start_
  container` — all already passing before this change, unaffected by
  it (this fix touches only `features.rs`, nowhere near the actual
  hook-execution code).
- The README's own milestone-3 summary already correctly says "all six
  real lifecycle hooks including `createContainer`/`startContainer`" —
  `features.rs`'s own hand-typed list was the one place in the entire
  codebase still claiming otherwise.
- Real `runc features`'s own installed output (`runc 1.3.4`, checked
  directly): `["prestart", "createRuntime", "createContainer",
  "startContainer", "poststart", "poststop"]` — this project's
  corrected list now matches byte-for-byte, in the same order.

## Implementation

`Features::hooks` (built in `features()`) now lists all six real hook
names, in real runc's own exact order. The adjacent code comment
(previously the source of the stale claim) is rewritten to explain
what actually happened (0077 predated 0088; the hand-typed list was
simply never revisited once real execution landed) rather than
re-asserting the now-false claim. `features_serializes_with_the_real_
spec_field_names`'s own test previously *enforced* the bug (`assert!
(!hooks.contains("createContainer"))`) — inverted to assert both
`createContainer` and `startContainer` are present, matching reality.

No change to any hook-execution code at all — this was purely a
stale, hand-typed introspection list drifting out of sync with an
already-correct, already-tested implementation, not a functional gap.

## Tests

Updated `features_serializes_with_the_real_spec_field_names` (now
asserts `createContainer`/`startContainer` are present, rather than
asserting `createContainer`'s absence). Manually verified `ocirun
features`'s own real JSON output matches an installed `runc
features`'s `hooks` array exactly.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures), `python3
ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh`, `bash
ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg -r` round trip).
This change is pure, read-only introspection (`ocirun features`), not
on any container-launch code path at all — no `ci/bench.sh`
re-verification needed.
