# Design note 0196: `ociman build -q`/`--quiet`

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Build`'s new `--quiet`/`-q`
flag); `bin/ociman/src/build.rs` (threading `quiet` through
`cmd_build`/`build_stage`/`apply_instruction`/`run_instruction`,
suppressing the unused-build-arg warning and the "tagged: ..." line);
`crates/oci-runtime-core/src/launch.rs` (new `discard_output` parameter
on `run`/`run_reporting_pid`, real fix for a read-only-`/dev/null`
bug found while testing this end to end); `bin/ocirun/src/main.rs`/
`bin/ociman/src/main.rs` (the two other `run_reporting_pid` call sites,
both pass `false`); `tests/tests/ociman_build.rs`.

## Continuing milestone 4

0195's own "what this doesn't do yet" named `-q`/`--quiet` as one of
the remaining small survey items. Checked real `docker build --help`/
`podman build --help` directly first: `-q, --quiet — refrain from
announcing build instructions and image read/write progress`.

## Real, checked-directly semantics

Verified directly against a real installed `podman build -q`, three
separate scenarios:

1. An ordinary tagged build with one `RUN echo hi` step and no `-q`:
   prints `STEP x/y` announcements, the `RUN` step's own live stdout
   (`hi`), `COMMIT ...`/`--> ...` lines, and finally `Successfully
   tagged ...` plus the digest.
2. The exact same build with `-q`: prints **only** the final digest —
   nothing else at all, not even the `RUN` step's own live output
   (`hi` never appears) or the `Successfully tagged ...` line.
3. `-q` combined with an unused `--build-arg`: the "`[Warning] one or
   more build args were not consumed: [...]`" message, which a real
   non-quiet build always prints, is *also* suppressed entirely by
   `-q`.

So the real rule: `-q` suppresses every single line of build output
except the one final image digest — not just the `STEP`-by-`STEP`
progress announcements this project's own `ociman build` never had in
the first place (its own pre-existing non-quiet default was already
sparser than real podman's own non-quiet default in that one
specific respect), but also a `RUN` step's own live command output,
the tag confirmation line, and the unused-build-arg warning. Has no
effect on `--json` output, which was already exactly this minimal.

## The fix

* `Command::Build` gains `-q`/`--quiet` (bare bool).
* `quiet` threaded through `cmd_build` → `build_stage` →
  `apply_instruction` → `run_instruction`, ending at the one real
  `oci_runtime_core::launch::run` call site a `RUN` step makes.
* `launch::run`/`launch::run_reporting_pid` both gain a new
  `discard_output: bool` parameter (alongside the existing
  `close_stdin`): when true (and no `log_path` was given — the two
  concepts never actually collide in practice, since `discard_output`
  is only ever true from `run`'s one call site, which never has a log
  path of its own), the container's own stdout/stderr are both dup'd
  onto a freshly-opened `/dev/null` instead of whatever fds 1/2 this
  process already has, by setting the very same `stdout_log_fd`/
  `stderr_log_fd` fields `setup_log_tee_pipe` otherwise uses for a
  *different* caller (`ociman run --log`). The two other existing
  `run_reporting_pid` call sites (`ocirun run`, `ociman run`/`ociman
  create`) both pass `false` — this is exclusively a `ociman build -q`
  concept, neither `ocirun` nor an ordinary `ociman run` has an
  analogous quiet mode.
* In `cmd_build` itself: the unused-`--build-arg` warning call and the
  final `tagged: ...` line are both skipped when `quiet` — the digest
  line itself is always printed regardless, matching real podman's own
  one remaining line of output.

## A real bug found while testing this end to end

The first implementation attempt opened `/dev/null` with a plain
`std::fs::File::open` (read-only) and dup'd that same read-only fd
onto *both* stdout and stderr. Manually testing the new flag
end-to-end (not just the parser) caught this immediately: a Containerfile
with a completely ordinary `RUN echo hi` step started failing outright
under `-q`, with `RUN /bin/sh -c echo hi failed with exit code 1` —
despite `-q` supposedly only changing what gets *printed*, never what
actually gets built. `strace -f` on the forked child showed exactly
why: `openat(AT_FDCWD, "/dev/null", O_RDONLY|O_CLOEXEC)`, then
`dup3(fd, 1, 0)` — a read-only `/dev/null` dup'd onto stdout makes
every subsequent `write(2)` to fd 1 fail with `EBADF`, which is
precisely what turned busybox `sh`'s own builtin `echo` into a nonzero
exit. Fixed by opening with `OpenOptions::new().write(true)` instead —
confirmed directly afterward that the same Containerfile now builds
successfully under `-q`, with the RUN step's own output nowhere in the
captured stdout and the final digest still present and correct.

## Tests

Two new integration tests in `tests/tests/ociman_build.rs`:
* `build_quiet_suppresses_run_step_output_and_prints_only_the_digest`
  — builds the same Containerfile with and without `-q`, asserting the
  loud build shows both the `RUN` step's own live output and the
  `tagged: ...` line, while the quiet build's entire stdout is exactly
  one `sha256:...` line, and the image is still genuinely built and
  tagged (`ociman inspect` on it still succeeds).
* `build_quiet_suppresses_unused_build_arg_warning` — an unused
  `--build-arg` warns on stderr without `-q`, and doesn't with it.

All 103 pre-existing `ociman build` tests continue to pass unchanged
(105 total now). Full `cargo build --workspace --locked`/`cargo test
--workspace --locked` (2 clean runs, 83/83 result blocks)/`cargo fmt
--all --check`/`cargo clippy --workspace --all-targets --locked -- -D
warnings`/`python3 ci/guards.py`/`cargo deny check`/`bash
ci/native-ci.sh` all clean. No performance regression (`ociman build
--no-cache`, one `RUN` step, both with and without `-q`, ~16-26ms,
consistent with prior measurements for the same scenario — quiet mode
is, if anything, marginally cheaper, since there is less to write to
a terminal).

## What this doesn't do yet

Real `until=`/`dangling=`-style prune filters, `--timestamp`
(reproducible builds), and the larger `RUN --mount=`/heredoc/multi-
platform gaps named by the earlier milestone-4 survey remain open,
each its own well-scoped future increment.
