# Design note 0454: `ociman build --shm-size`

Status: implemented
Scope: `bin/ociman/src/build.rs`, `bin/ociman/src/main.rs`,
`tests/tests/ociman_build.rs`.

## What this closes

`ociman build` had no `--shm-size` flag at all — real `podman build
--shm-size`'s way of sizing every `RUN` step's own `/dev/shm` tmpfs,
distinct from (and reusing none of) `ociman run`/`ociman create
--shm-size`'s already-existing per-*container* flag, continuing the
same reuse-the-existing-primitive shape `0453` (`--ulimit`) just
established.

## Real, checked-directly confirmation

`~/git/podman/vendor/go.podman.io/buildah/pkg/cli/common.go:170,453`:
`ShmSize string`/`fs.StringVar(&flags.ShmSize, "shm-size", ...)` — one
build-wide value, no per-stage or per-instruction variant anywhere in
`CommonBuildOptions` (same shape as `Ulimit`). `~/git/podman/vendor/
go.podman.io/buildah/run_common.go:1332`: `setupSpecialMountSpecChanges
(spec, b.CommonBuildOpts.ShmSize)` is called from the same shared
per-`RUN`-invocation setup function `0453` already found calling
`addRlimits` — confirming this is genuinely applied fresh to every
`RUN` step in every stage, not just once.

## Implementation

- Reused `ociman run`/`create --shm-size`'s existing `parse_memory_
  limit` (`bin/ociman/src/main.rs`) verbatim via `crate::` path — no
  new parser, since both flags are already backed by the identical
  real `go-units.RAMInBytes` grammar (already established by `0376`'s
  own doc comment). Parsed once per build invocation in `cmd_build`,
  right after `rlimits`, before the main stage loop.
- `StageContext<'a>` gains a new `shm_size_bytes: Option<i64>` field,
  carried the same way `rlimits`/`dockerignore`/`forced_mtime` already
  are.
- `apply_instruction`'s `Instruction::Run` arm passes `stage_ctx.
  shm_size_bytes` through to `run_instruction`, which threads it to
  `run_step_spec`'s new trailing `shm_size_bytes: Option<i64>`
  parameter; its body rewrites `Spec::example()`'s own already-present
  `/dev/shm` tmpfs entry's own `size=` option in place — the exact
  same logic `main.rs`'s `synthesize_spec` already uses for `run`/
  `create`, reused verbatim here instead of a second implementation.
- `Command::Build` gains `shm_size: Option<String>` (`--shm-size`,
  `allow_hyphen_values` — same reason `run`/`create`'s own flag needs
  it: a negative value must reach this flag's own validation instead
  of being misread as an unrecognized flag), inserted after `ulimit`,
  before `quiet`.

No new parsing/validation code was written at all for this
increment either — same as `0453`, the entire change is plumbing an
already-fully-tested value through to a second call site.

## Tests

Two new tests in `tests/tests/ociman_build.rs`: `build_shm_size_
enforces_a_real_kernel_tmpfs_limit_for_run_steps` (a real,
kernel-enforced verification — a 4 MiB `dd` write into a real 1 MiB
`--shm-size`'d `/dev/shm` inside a `RUN` step genuinely fails the
build with `ENOSPC`, the same pattern already established by `ociman
run --shm-size`'s own `run_shm_size_actually_enforces_a_real_kernel_
tmpfs_limit`), and `build_without_shm_size_lets_a_run_step_write_
well_past_one_megabyte_into_dev_shm` (a regression guard: with no
`--shm-size` given at all, the same 4 MiB write still succeeds,
proving `run_step_spec`'s own default `/dev/shm` mount is never
accidentally rewritten to something smaller when nothing was asked
for). All 126 prior tests in the file pass unmodified (128/128
total).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (120 test-result
blocks, 0 failures, clean on the first full run), `python3 ci/
guards.py`, `cargo deny check`, `bash ci/native-ci.sh` (clean,
120/120, clean on the first run too), `bash ci/build-deb.sh` (real
`dpkg -i`/`--version`/`dpkg -r` round trip). `ci/bench.sh`'s own
`build`/`build --no-cache` sections never pass `--shm-size`, so every
`RUN` step there keeps the untouched default mount — same trivial
no-op-cost reasoning `0453` already established; no benchmark re-run
needed.

## Deliberately still out of scope

Real buildah's own `ShmSize` is genuinely build-wide with no per-stage
or per-instruction override at all (confirmed directly above), same
as `--ulimit` — so there is no narrower scope left to add here. A
future, separate increment could still add `ociman build --volume`
(BuildKit-/buildah-style `RUN --mount=type=bind`), a larger,
differently-shaped gap that doesn't reuse an existing `run`/`create`
primitive the way `--ulimit`/`--shm-size` both did.
