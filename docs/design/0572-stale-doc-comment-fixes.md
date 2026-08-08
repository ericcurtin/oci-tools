# Design note 0572: two stale doc-comment fixes

Status: implemented
Scope: `bin/ociman/src/main.rs`, `bin/ocirun/src/main.rs`.

## What this closes

Two hand-typed doc comments drifted out of sync with an already-
correct, already-tested implementation elsewhere in the same codebase
— the exact same bug shape `docs/design/0380` (`ocirun features`'s
stale `hooks` list) already found and fixed once before. Neither is a
functional change; both are pure documentation corrections, verified
by direct comparison against the real, already-shipped, already-
tested behavior rather than assumed.

## Fix 1: `ociman healthcheck run --help` claimed the timeout isn't enforced

`Command::HealthcheckCommand::Run`'s own doc comment (rendered
verbatim into `ociman healthcheck run --help`) said: *"the configured
`Timeout` isn't enforced yet, so a genuinely hung check currently
blocks this command itself rather than being killed and reported
`unhealthy`."*

This was true when `docs/design/0172` first shipped `healthcheck run`,
but `docs/design/0308` closed exactly this gap: `cmd_healthcheck_run`'s
own doc comment, two thousand-some lines away in the same file,
already correctly says *"The configured `Timeout`… is enforced now
(0308): a genuinely hung test is killed (`SIGKILL`) once it elapses
and reported `unhealthy`."* 0308 corrected the function's own comment
but never touched the sibling CLI enum-variant's comment, and it sat
wrong ever since — a real, live, user-visible bug: running `ociman
healthcheck run --help` today prints the false claim, directly
contradicting the accurate comment sitting right next to the code that
actually enforces it.

Live-verified directly: `tests/tests/ociman_healthcheck.rs`'s own
`healthcheck_run_kills_a_hung_test_once_its_own_timeout_elapses` test
already exists, already passes, and already proves the timeout is
real — this fix only corrects the doc comment `--help` renders to
stop contradicting that same, already-tested reality.

## Fix 2: `ocirun`'s module doc comment claimed hooks "still remain" for the two-phase lifecycle

`bin/ocirun/src/main.rs`'s own top-of-file module doc comment said:
*"`prestart`/`createRuntime`/`poststart`/`poststop` for the `create`/
`start`/`kill`/`delete` lifecycle specifically still remain."*

Tracing the actual code shows this is false and has been for some
time:
- `cmd_create` calls the exact same `launch::create` real runc/crun
  both run `prestart`/`createRuntime` from (`crates/oci-runtime-core/
  src/launch.rs:712`'s own comment: *"Real runc's own `create`/`run`
  both run `prestart`/`createRuntime` hooks"*).
- `cmd_start` calls `oci_runtime_core::launch::run_poststart_hooks`
  (`bin/ocirun/src/main.rs:1584`).
- `cmd_delete` calls `oci_runtime_core::launch::run_poststop_hooks`
  (`bin/ocirun/src/main.rs:1728`).
- `kill` has no hook point of its own anywhere in the real runtime
  spec at all (the six real hook points are `prestart`/
  `createRuntime`/`createContainer`/`startContainer`/`poststart`/
  `poststop` — none map to a signal-delivery operation), so there was
  never a real gap there to begin with; the original comment's own
  inclusion of `kill` in this list was itself a mistake, not a
  narrower-but-real gap.

Real, comprehensive test coverage already exists and already passes,
proving this directly: `tests/tests/ocirun_lifecycle.rs`'s own
`create_runs_prestart_then_create_runtime_before_returning`,
`a_failing_prestart_hook_aborts_create_entirely`,
`start_runs_poststart_hook_with_a_running_state`, and
`delete_runs_poststop_hook_with_a_stopped_state` — all specifically
exercise the two-phase `create`/`start`/`delete` lifecycle's own hook
execution, not just the combined `run` command's.

Unlike Fix 1, this comment is a `//!` module-level doc comment, never
rendered through `ocirun --help` (clap only renders the `Cli` struct's
own doc comment) — verifiable only by reading source, not by running
the binary. Included here alongside Fix 1 since it's the exact same
bug class, equally trivial, and equally zero-risk.

## Why this is narrow and safe

Both fixes touch only rustdoc comments — zero lines of executable code
changed, zero cgroup/namespace/capability/systemd/mount contact. No
new tests were needed: the underlying behavior each comment now
accurately describes was already correct and already covered by
existing, passing tests (cited above in each fix) before this note;
this only corrects what the comments themselves claimed.

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (129
test-result blocks, all passing on the first attempt with
`RUST_TEST_THREADS=2`), `python3 ci/guards.py` (clean), `cargo deny
check` (clean), `bash ci/native-ci.sh` (clean on the first attempt,
`RUST_TEST_THREADS=2` set from the start since this host's own
concurrent-session CPU contention has repeatedly hit the same well-
documented `ocicri_container.rs`-class transient flakes this same
day, and `ci/native-ci.sh` doesn't set that itself), `bash
ci/build-deb.sh` (clean on the first attempt, real `dpkg -i`/
`--version`/`dpkg -r` round trip). No `ci/bench.sh` rerun needed: pure
doc-comment changes, no code path touched at all.

## Deliberately still out of scope

`ociman image diff` (a real, live-verified functional gap identified
during this same research pass, reusing existing `oci_layer`/rootfs-
extraction machinery) is a good candidate for a follow-up increment,
left for a separate note since it's a genuine new feature rather than
a same-shaped documentation fix.
