# Design note 0351: `ociman run`/`exec --preserve-fds`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/{ociman_run,ociman_exec}.rs`.

## A real, previously-uncaught factual error, found and corrected

Design note `0294` (`ocirun exec --preserve-fds`) asserted, at its own
`ociman exec` call site, that this flag was intentionally left off
because real podman lacks an equivalent — citing `0276` as the
"checked directly" source. Re-checking both claims directly this
turn:

- `0276` is actually about `ocirun exec -g`/`--additional-gids` and
  never once mentions `--preserve-fds` at all — a stale/mixed-up
  citation, not a real re-verification.
- Running `podman exec --help` (and `podman run --help`) directly on
  this machine (podman 4.9.3) shows `--preserve-fds` genuinely
  present and enabled in local (non-`--remote`) mode on **both**.
  Confirmed further in real podman's own source
  (`~/git/podman/cmd/podman/containers/run.go:68-69`,
  `~/git/podman/cmd/podman/containers/exec.go:90-91`): the flag is
  registered unconditionally on both commands (`MarkHidden` only ever
  applies for `--remote`, a mode this project has no equivalent of).

This closes both gaps at once, and — checked directly this time,
not assumed — confirms one real, genuine asymmetry along the way:
`podman create --help` has **no** `--preserve-fds` flag at all
(`~/git/podman/cmd/podman/containers/create.go` has no equivalent flag
registration, confirmed by grep), unlike `run`. This makes sense once
traced: `create` never launches anything — there is no live process at
create time for an extra inherited fd to ever reach, and a later
`ociman start` reruns from a completely fresh `ociman` process with no
way to inherit fds from the original `create` invocation's own process
either way. `ociman create` correctly keeps no equivalent flag.

## Implementation

The underlying primitive — `oci_runtime_core::exec::ExecRequest.
preserve_fds: u32` and `launch::run_reporting_pid`'s own identical
parameter, both already fully implemented and tested for `ocirun`
(`0291`/`0294`) — needed zero changes at all; this was pure CLI
plumbing, replacing two hardcoded `0`s with the real, given value:

- `Command::Run` gained `preserve_fds: u32` (`--preserve-fds`,
  default `0`) as its own field, **not** folded into the shared
  `RunArgs` struct `Run`/`Create` both flatten — the one real
  deliberate asymmetry above means it must only ever appear on `Run`.
  Threaded through both of `cmd_run`'s own launch paths
  (`launch_detached_and_confirm` for `-d`, `run_and_finalize` for the
  foreground default) — both already had a parameter list, so this is
  one more argument each, no new plumbing shape.
- `Command::Exec` gained the identical field; `cmd_exec`'s own
  `ExecRequest` construction now passes it through instead of a
  hardcoded `0`.
- New `verify_preserve_fds(n: u32) -> anyhow::Result<()>` in `ociman`'s
  own `main.rs` — a real, if small, deliberate duplication of
  `ocirun`'s own already-established identically-named function
  (`0291`): checks `/proc/self/fd/<fd>` existence for every claimed
  fd, matching real podman's own `IsFdInherited` check
  (`~/git/podman/cmd/podman/containers/run.go`) — but with real
  podman's own exact error wording ("file descriptor N is not
  available - the preserve-fds option requires that file descriptors
  must be passed"), not `ocirun`'s own copy of real runc's
  differently-worded one. Called eagerly, before any other work, in
  both `cmd_run` and `cmd_exec` — matching real podman's own upfront
  validation-before-anything-else convention.
- `cmd_start`'s own (unrelated) `launch_detached_and_confirm` call site
  (used by both `ociman start` and `ociman restart`, which reruns via
  `cmd_start`) explicitly passes a hardcoded `0` — matching real
  podman's own identical lack of `--preserve-fds` on `start` too
  (checked directly, `podman start --help`).

## Verified

New tests, mirroring `ocirun_run.rs`'s/`ocirun_exec.rs`'s own already-
established `--preserve-fds` tests exactly (same real `pre_exec`/
`dup2`-onto-fd-3-plus-`FD_CLOEXEC`-clearing technique, needed to make
the check deterministic regardless of whatever fds the test harness
itself already has open):

- `ociman_run.rs`:
  `run_preserve_fds_closes_extra_fds_by_default_but_keeps_them_with_the_flag`,
  `run_preserve_fds_rejects_a_claim_with_no_matching_open_fd` (also
  confirms nothing was created at all — the check runs before any
  real launch work), `create_has_no_preserve_fds_flag` (a real,
  positive confirmation of the deliberate asymmetry, not just an
  absence nobody thought to test).
- `ociman_exec.rs`:
  `exec_preserve_fds_closes_extra_fds_by_default_but_keeps_them_with_the_flag`,
  `exec_preserve_fds_rejects_a_claim_with_no_matching_open_fd`.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (113 test-result blocks,
0 failures — including all 81 pre-existing `ociman_run` and 9
pre-existing `ociman_exec` tests, unmodified), `python3 ci/guards.py`,
`cargo deny check`. No `ci/bench.sh` re-run needed: the default
(unused) case is provably unchanged — the exact same code path,
parameterized rather than hardcoded, `verify_preserve_fds(0)` a
zero-iteration no-op loop.

## Still ahead

Real podman's own separate `--preserve-fd <FD>` (repeatable, arbitrary
specific fd numbers, distinct from the count-based `--preserve-fds N`
this note implements) would need a new `Vec<u32>` fd-list mechanism
this project's shared `preserve_fds: u32` primitive doesn't support at
all — a genuinely separate, bigger feature, not picked up here.
`ociman inspect --size`/`-s` (candidate A from this turn's own
scoping) remains a separate, similarly-small, not-yet-scoped
candidate.
