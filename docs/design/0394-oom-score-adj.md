# Design note 0394: `process.oomScoreAdj` + `ociman run/create --oom-score-adj`

Status: implemented
Scope: `crates/oci-spec-types/src/runtime.rs`, `crates/oci-runtime-core/src/oom.rs` (new),
`crates/oci-runtime-core/src/launch.rs`, `crates/oci-runtime-core/src/lib.rs`,
`bin/ociman/src/main.rs`, `tests/tests/ocirun_run.rs`, `tests/tests/ociman_run.rs`,
`README.md`.

## What this closes

A real, previously-silent gap of the same shape `0393` (`umask`) just
closed: `process.oomScoreAdj` — a real, optional runtime-spec field
tuning a container's own init process's `/proc/self/oom_score_adj` —
had no representation anywhere in this project's own spec types at
all, and no code anywhere ever wrote to that file. A pod/bundle asking
for a specific OOM-killer preference (a real, legitimate tuning knob —
e.g. protecting a critical process, or deprioritizing a disposable
one) had no way to express that at all, and `ociman run` had no flag
for it either, unlike real `podman run --oom-score-adj`.

## Real, checked-directly confirmation

- `~/git/container-libs/vendor/github.com/opencontainers/runtime-spec/
  specs-go/config.go:91-92`: `Process.OOMScoreAdj *int` — a real,
  optional runtime-spec field this project's own `Process` type had
  no equivalent of.
- `~/git/crun/src/libcrun/linux.c:4447-4467` (`libcrun_set_oom`): a
  real, direct `/proc/self/oom_score_adj` write, guarded by `def->
  process->oom_score_adj_present` — genuinely a no-op when absent, no
  fabricated default the way `umask` has one.
- **Scope, checked directly rather than assumed**: `libcrun_set_oom`
  is called from exactly two places, both in `linux.c`'s own
  container-creation-time setup path — never from `container.c`'s own
  exec handling (confirmed via `grep`, zero hits). Real `runc`'s own
  equivalent (`libcontainer/container_linux.go:1176`) is likewise only
  ever wired into its own container-creation path. This project's own
  counterpart matches that scope exactly: only `oci_runtime_core::
  launch`, never `oci_runtime_core::exec`.
- `~/git/podman/cmd/podman/common/create.go`'s own `oomScoreAdjFlagName`
  (`podman run --oom-score-adj`, a plain `int`, "-1000 to 1000" in its
  own help text) — confirmed neither podman nor crun does any
  client-side range pre-validation of their own (checked directly,
  no such check in either project's source): an out-of-range value is
  a real, surfaced kernel `EINVAL` at the write itself, not a clear
  CLI-level rejection. This project's own implementation follows the
  identical "let the kernel's own rejection speak for itself"
  precedent, rather than inventing a stricter check neither real tool
  actually has.

## Implementation

- `oci_spec_types::runtime::Process` gains `pub oom_score_adj:
  Option<i32>` (camelCase `oomScoreAdj` on the wire, `None` meaning
  "not given at all, leave the inherited value untouched" — matching
  real crun's identical `oom_score_adj_present` guard, not a
  fabricated default the way `umask`'s own `0o022` fallback is).
- New `oci_runtime_core::oom` module (mirroring `rlimits.rs`'s own
  small, single-concern shape): `apply(proc_root: &Path, value:
  Option<i32>) -> io::Result<()>` — `None` is a real no-op that
  touches nothing at all; `Some(value)` writes the plain decimal
  string to `<proc_root>/self/oom_score_adj`. No name-table lookup
  needed (unlike rlimits): the spec field is already a plain integer.
- `launch.rs`'s `ChildSetup` gains an `oom_score_adj: Option<i32>`
  field (threaded from `process_spec.oom_score_adj` at construction
  time, the same shape `rlimits`/`no_new_privileges` already use);
  `ChildSetup::run()` calls `oom::apply` right after `rlimits::apply`
  — both are plain process attributes with the same "no ordering
  dependency on namespaces/identity" reasoning, applied early,
  matching crun's own placement. `exec.rs` is deliberately untouched
  at all, matching the confirmed real-crun/runc scope above.
- `ociman run`/`create` gains `--oom-score-adj`, written straight into
  `synthesize_spec`'s generated `process.oom_score_adj` right after
  the existing `--umask` wiring; no client-side range validation,
  matching the real-tool precedent above. `ocirun` needs no new flag
  at all: it already reads `config.json` directly, so an explicit
  `process.oomScoreAdj` there is now genuinely honored instead of
  having nowhere to parse into. `ocibox`/`ocicri` are unaffected
  (neither distrobox nor the CRI proto has an equivalent concept, so
  both simply leave a container's inherited value untouched, matching
  what real `distrobox`/`cri-o` + `runc`/`crun` would also produce).

## Tests

Three new unit tests in the new `oci_runtime_core::oom` module
(`None` is a real no-op that touches nothing even when `self/` itself
doesn't exist; `Some` writes the real decimal value; a genuinely
missing `proc_root/self` surfaces a real `io::Error`, not a panic).
Two new real, end-to-end integration tests: `tests/tests/
ocirun_run.rs`'s `run_honors_an_explicit_oom_score_adj_declared_in_
the_bundle` (a hand-edited `config.json`'s `process.oomScoreAdj`
genuinely applied, read back via a real `/proc/self/oom_score_adj`
inside a running container) and `tests/tests/ociman_run.rs`'s
`run_oom_score_adj_flag_sets_a_real_value`. Both use a real, positive
(increasing) value deliberately: an unprivileged process may always
*raise* its own `oom_score_adj` without `CAP_SYS_RESOURCE` (this
project's own default capability set doesn't grant it), so this is
the one value shape guaranteed to succeed regardless of the host's
own starting value — a *lowering* value would need that capability
and isn't exercised here. All existing tests across `oci-runtime-core`
(233 pre-existing), `ocirun_run.rs` (24), and `ociman_run.rs` (101)
continue to pass unmodified.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures), `python3
ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh`, `bash
ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg -r` round trip).
This change touches `launch.rs`'s `ChildSetup::run`, the same shared
hot-path primitive every container launch across every binary already
goes through (the exact bar `0393`'s own full `ci/bench.sh` re-run
was justified by) — a **full `ci/bench.sh` re-run** was done again
rather than skipped: every figure held at or improved on its own
recorded baseline (`ocirun run` 3.4ms vs `crun run` 6.8ms/`runc run`
22.1ms; `ociman run --rm` 5.65×/8.29× faster than `podman`/`docker`;
`ociman exec` 15.77×/48.15× faster; `ociman rm` 38.97× faster;
`ociman commit` 28.95× faster; `ociman build` 17.30-27.81× faster
both cached and uncached) — the added `oom::apply` call is a single,
`None`-short-circuited no-op on every one of these measured paths
(none of them pass `--oom-score-adj`), so no measurable overhead was
ever expected, and none was observed.
