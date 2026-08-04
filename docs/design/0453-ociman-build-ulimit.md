# Design note 0453: `ociman build --ulimit`

Status: implemented
Scope: `bin/ociman/src/build.rs`, `bin/ociman/src/main.rs`,
`tests/tests/ociman_build.rs`.

## What this closes

`ociman build` had no `--ulimit` flag at all — real `podman build
--ulimit`'s way of applying `RLIMIT_*` resource limits (max open
files, max processes, core dump size, ...) to every `RUN` step's own
process, distinct from (and reusing none of) `ociman run`/`ociman
create --ulimit`'s (`0376`) already-existing per-*container* flag.

## Real, checked-directly confirmation

`~/git/podman/vendor/go.podman.io/buildah/pkg/cli/common.go:454`:
`fs.StringSliceVar(&flags.Ulimit, "ulimit", ...,
"ulimit options")` — one build-wide `[]string`, no per-stage or
per-instruction variant anywhere in `CommonBuildOptions`
(`~/git/podman/vendor/go.podman.io/buildah/define/build.go:88-105`).
`~/git/podman/vendor/go.podman.io/buildah/run_linux.go:682`:
`addRlimits(commonOpts.Ulimit, g, ...)` is called from the shared
per-`RUN`-invocation setup function (the same function that also
wires cgroup/memory/cpu resources for that one step), confirming this
is genuinely applied fresh to *every* `RUN` step in *every* stage of
the build, not just once — matching `--dns`/`--dns-search`
(`0299`)'s own existing "applies to every RUN step" shape in this
project, not a per-container, set-once-at-launch flag like `run`/
`create --ulimit`.

## Implementation

- Reused `0376`'s existing `parse_ulimit`/`clamp_ulimit_to_host`
  (`bin/ociman/src/main.rs`) verbatim via `crate::` paths — no new
  parser, no new name table, no new clamp logic. Parsed once per
  build invocation (not once per `RUN` step) via `let rlimits: Vec<
  oci_spec_types::runtime::PosixRlimit> = ulimit.iter().map(|u| crate
  ::parse_ulimit(u).map(crate::clamp_ulimit_to_host)).collect()`,
  right after `dockerignore` is computed in `cmd_build`, before the
  main stage loop.
- `StageContext<'a>` (the struct already carrying `dockerignore`/
  `forced_mtime`/etc. so every `RUN`-step-threading function can
  reach them) gains a new `rlimits: &'a [PosixRlimit]` field,
  constructed once at `cmd_build`'s single `StageContext { ... }`
  call site.
- `apply_instruction`'s `Instruction::Run` arm passes `stage_ctx.
  rlimits` through to `run_instruction`, which threads it to `run_
  step_spec`'s new trailing `rlimits: &[PosixRlimit]` parameter; its
  body sets `process.rlimits = rlimits.to_vec();` right after `process
  .user.gid = gid;` — the exact same field-assignment shape `main.rs`'s
  `synthesize_spec` (`ociman run`/`create`'s own spec-construction
  function) already uses for the same field.
- `Command::Build` gains `ulimit: Vec<String>` (`#[arg(long =
  "ulimit", value_name = "NAME=SOFT[:HARD]")]`), inserted after
  `unsetlabel`, before `quiet`; its dispatch arm passes `&ulimit`
  through to `build::cmd_build`.

No new parsing/validation code was written at all for this
increment — the entire change is plumbing an already-fully-tested
value (`0376`'s `parse_ulimit`/`clamp_ulimit_to_host`) through to a
second call site (`run_step_spec` instead of `synthesize_spec`).

## Tests

Two new tests in `tests/tests/ociman_build.rs`: `build_ulimit_sets_a_
real_kernel_enforced_rlimit_for_run_steps` (a real, kernel-enforced
verification via busybox `ash`'s own `ulimit -n`/`ulimit -Hn`
builtins inside a `RUN` step, captured to a file and read back via a
follow-up `run` — the same pattern already established by `build_dns_
flags_synthesize_a_real_resolv_conf_for_run_steps` (`0299`) and `run_
step_has_a_real_resolv_conf_copied_from_the_host`), and `build_ulimit_
with_an_unrecognized_name_is_a_clear_error` (reusing `0376`'s existing
`parse_ulimit` validation, now reachable from `build` too). All 124
prior tests in the file pass unmodified (126/126 total).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (120 test-result
blocks, 0 failures, clean on the first full run), `python3 ci/
guards.py`, `cargo deny check`, `bash ci/native-ci.sh` (clean,
120/120, clean on the first run too), `bash ci/build-deb.sh` (real
`dpkg -i`/`--version`/`dpkg -r` round trip). `ci/bench.sh`'s own
`build`/`build --no-cache` sections never pass `--ulimit`, so every
`RUN` step there gets an empty `rlimits` slice — `process.rlimits =
rlimits.to_vec()` on an empty slice is the same trivial no-op cost
`0376` already established for `run`/`create` in the overwhelmingly
common no-`--ulimit` case; no benchmark re-run needed.

## Deliberately still out of scope

Real buildah's own `Ulimit` is genuinely build-wide with no per-stage
or per-instruction override at all (confirmed directly above) — so
there is no narrower scope left to add here; a future, separate
increment could still add `ociman build --volume`/`--shm-size`, which
reuse existing `run`/`create` primitives the same way this one did.
