# Design note 0189: `ociman wait` — multiple containers, `--condition`,
`--ignore`, plus two stale doc comments fixed

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Wait`, `cmd_wait`,
`parse_wait_condition`); `crates/oci-runtime-core/src/overlay.rs`
(stale doc comment); `tests/tests/ociman_wait.rs`.

## Starting point: a survey, not a hypothesis

Before writing any code, surveyed every remaining "still-deferred"/
"what this doesn't do yet" note across `bin/ociman/src/main.rs` and
`docs/design/*.md` to find the next small, well-scoped, genuinely
open gap (rather than guessing) — most turned out to already be
closed by a later increment; `docs/design/0092-ociman-wait.md`'s own
"what's still not here" (no `--condition`/`--ignore`, one container at
a time) was confirmed still open in the current code.

## Two stale doc comments found and fixed along the way (zero risk,
zero behavior change)

The same survey turned up two doc comments that no longer match
reality, both purely descriptive (no code they describe needed to
change):

* `Command::Cp`'s own doc comment still claimed container-to-container
  copying was "deliberately out of scope for now" — `cmd_cp`'s own doc
  comment, right next to it, already correctly describes it as
  implemented (0151). Fixed the stale one to match.
* `oci_runtime_core::overlay`'s own module doc comment still claimed
  "nothing in any existing container's own creation path calls it
  yet" and "this module doesn't wire itself into a CLI command yet" —
  but `bin/ociman/src/rootfs_setup.rs`'s own `rootless_overlay_
  supported_cached` has called it on every `ociman run` since 0110.
  Fixed to describe the real, current wiring and point at the
  existing indirect test coverage (`tests/tests/ociman_run.rs`'s own
  `.rootless-overlay-supported` marker-file mechanism).

## Real semantics, checked directly

Checked real `podman wait --help` and its own source
(`~/git/podman/cmd/podman/containers/wait.go`,
`~/git/podman/pkg/domain/infra/abi/containers.go`,
`~/git/podman/libpod/container_api.go`) before designing anything:

* Multiple `CONTAINER` args are accepted positionally (not comma-
  separated); each one's own real exit code prints on its own line, in
  the exact order given (confirmed directly).
* Every name is resolved **up front**, before any waiting begins at
  all — confirmed directly: `podman wait valid-one does-not-exist`
  aborts the whole command immediately, printing *nothing* for
  `valid-one` even though it does exist and would otherwise have
  resolved and printed fine.
* `--ignore` turns an unresolvable name into a `-1` placeholder instead
  of a hard error (confirmed directly).
* `--condition` is repeatable; multiple values are OR'd together (any
  *one* satisfies the wait) — confirmed by reading `WaitForCondition
  WithInterval`'s own `wantedStates` map, never an AND.
* A reached condition other than `stopped`/`exited` always prints
  `-1`, never a real exit code (confirmed directly: `podman wait
  --condition running` on an already-running container prints `-1`).
* `stopped` and `exited` are pure synonyms in real podman itself
  (`WaitForConditionWithInterval`'s own `case ContainerStateExited,
  ContainerStateStopped: waitForExit = true`).

## The fix

* `Command::Wait`'s own `id: String` becomes `ids: Vec<String>`
  (`required = true`, so existing single-id usage is unaffected).
* New `--condition` (repeatable `Vec<String>`) and `--ignore` flags.
* `parse_wait_condition` maps `created`/`running`/`stopped`/`exited`/
  `paused` onto this project's own `Status` enum — real podman's own
  additional `configured`/`removing`/`stopping`/`unknown` states and
  `healthy`/`unhealthy` healthcheck conditions have no equivalent in
  this project's own simpler lifecycle (no periodic healthcheck
  scheduler at all, `ociman healthcheck run` is a manual one-shot
  command a wait condition couldn't meaningfully block on) — any of
  those is a clear, immediate error rather than a silently wrong
  match, matching this project's own established "honestly narrower,
  never silently wrong" pattern.
* `cmd_wait` resolves every id first (fail-fast, matching real podman
  exactly — `Err(e) if ignore => resolved.push(None)`, else propagate
  immediately with nothing printed for any container), then loops over
  each resolved container printing its own real exit code (or `-1`)
  once it reaches any wanted condition.
* Reused the existing `display_status` helper (already shared by `ps`/
  `inspect` to compute a real, cgroup-freezer-backed `Paused` on top of
  `effective_status()`, from 0144) rather than duplicating that logic
  — `--condition paused` gets real freezer-state matching for free.

## Tests

Five new integration tests in `tests/tests/ociman_wait.rs`: multiple
containers printing each exit code in order; `--ignore` on a
nonexistent container; the fail-fast behavior (nothing printed for a
valid container once a different name fails to resolve); `--condition
running` returning immediately with `-1`; an unsupported condition
value erroring clearly. All 3 pre-existing tests continue to pass
unchanged (the single-id case). Verified `--condition paused` by hand
against a real `ociman pause`d container (matches the real cgroup
freezer state via `display_status`, confirmed directly). Full `cargo
build --workspace --locked`/`cargo test --workspace --locked` (2 clean
runs, 83/83 result blocks)/`cargo fmt --all --check`/`cargo clippy
--workspace --all-targets --locked -- -D warnings`/`python3
ci/guards.py`/`cargo deny check`/`bash ci/native-ci.sh` all clean.

## A real, unrelated disk-space finding along the way (operational
cleanup, not a code fix)

While auditing the environment for anything relevant to "ensure we
don't run out of disk space," found 30 real, currently-*mounted*
loopback ext4 images (~124MB total) left over from many earlier,
unrelated sessions' `oci-erofs` fs-verity test runs
(`crates/oci-erofs/src/verity.rs`'s own `VerityFs` test fixture).
Root-caused directly: the fixture's own `Drop` impl correctly
`umount`s (confirmed by hand — the exact same commands, run manually,
clean up perfectly every time), so this isn't a code bug at all; the
only way to leave a mount behind is for the test process itself to be
killed (e.g. `SIGKILL`, which no `Drop` can ever run for) before ever
reaching that `Drop` — which is exactly what happened across several
earlier sessions' own forced process terminations (timeouts, a hung
fork/thread-hazard test needing a manual `kill -9`). Since this
fixture only exists inside `#[cfg(test)]` and is never reachable from
real `ociman`/`ocirun` production code at all, there is nothing to fix
in the codebase — cleaned up the 30 stale mounts/loop devices/tempdirs
directly (`sudo umount` each, then `rm -rf`) as one-time operational
hygiene instead.

## What this doesn't do yet

`--exit-first-match`/`--latest` (real podman/docker flags this project
has no equivalent scope for yet — `--latest` in particular needs a
notion of "the most recently created container," which nothing else in
this project's own CLI currently tracks); `healthy`/`unhealthy`
healthcheck conditions (blocked on this project having no periodic
healthcheck scheduler at all, only a manual one-shot `healthcheck
run`).
