# Design note 0092: `ociman wait` (milestone 3)

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Wait`, `cmd_wait`),
`tests/tests/ociman_wait.rs`.

`ociman` gained real `docker wait`/`podman wait`: block until a
container stops, then print its exit code. Found via the same
small-gaps survey that produced 0090/0091, ranked as "almost no new
code" — the exit code is already captured and persisted by `cmd_run`'s
own foreground wait (`ANNOTATION_EXIT_CODE`, existing since much
earlier), so `wait` itself needed nothing more than a poll loop over
already-persisted state.

## Verified against real podman source first, not assumed

Read `~/git/podman/cmd/podman/containers/wait.go` directly: block,
then print a bare exit-code integer per container, nothing else — no
extra formatting, no container id echoed back (unlike `kill`/`stop`,
which print the id). Default poll interval `250ms`
(`--interval`/`-i`). `cmd_wait` matches this shape: `--interval`
(milliseconds, default `250`), a plain `println!("{exit_code}")` once
`effective_status()` reports `Stopped`.

## Zero new state — poll `effective_status()`, read the existing annotation

`effective_status()` already correctly downgrades a dead-pid-but-not-
yet-persisted-`Stopped` state to `Stopped` (existing since the state
module's own earliest tests) — exactly the polling primitive `wait`
needs, no changes to it at all. The exit code itself comes straight
from `ANNOTATION_EXIT_CODE`, the same annotation `cmd_run`'s own
foreground wait already writes once the container's process exits;
`wait` only ever *reads* it. The one edge case handled explicitly:
if the container is genuinely stopped but the annotation is somehow
still missing (should not happen in practice — only reachable if
`cmd_run`'s own foreground invocation never got to persist it), print
`-1` rather than failing outright, since the container really has
stopped by then and `wait` succeeding is still the more useful answer
than an error.

## Real, manual verification against a real, freshly-pulled busybox

Built the release binary and ran a real, backgrounded
`docker.io/library/busybox` container (`sleep 1; exit 7`): confirmed
`ociman wait` genuinely blocked (~1.75s, not an instant return) and
printed the real exit code `7`; confirmed `wait` on an
already-*stopped* container (`exit 42`) returned essentially
instantly with `42`; confirmed `wait` on an unknown container id
fails cleanly.

## Real, automated tests

Three integration tests in `tests/tests/ociman_wait.rs`, mirroring
`ociman_stop.rs`/`ociman_kill.rs`'s own established helpers exactly
(seeded offline images, `spawn()`+detached-stdio+poll): a genuine
block-then-print-the-real-exit-code case (asserting real elapsed wall
time, not just the printed value, to prove `wait` actually blocked
rather than racing to a stale answer), the already-stopped-returns-
immediately case, and the unknown-container error case. One real bug
caught in the test itself while writing it, not the implementation: an
early draft asserted `ociman run ... exit 42`'s own process exit
*succeeded*, forgetting this project's own well-established "the
container's own exit code becomes `ociman run`'s own exit code"
behavior — fixed to assert the real exit code (`42`) instead of
success.

## Not a hot-path change — no A/B perf re-verification needed

Purely additive: one new `Command` enum variant, one new, wholly
independent function (`cmd_wait`), one new match arm. Confirmed
directly via `git diff --stat`: `synthesize_spec`/`resolve_seccomp`/
`command_for` and every cgroup driver are completely untouched.

## What's still not here

* `ociman rename`/`inspect` (for containers)/`top`, `ociman run -d`/
  `--detach`, `ocirun update`/`pause`/`resume`, automated failed-
  systemd-scope cleanup, the build cache, `ONBUILD`/`HEALTHCHECK` —
  all still exactly as earlier increments left them, unrelated to
  this increment's own scope.
* Real podman's own richer `wait` (`--condition`, `--ignore`,
  multi-container args) — this increment is deliberately the narrow,
  single-container, single-condition ("stopped") first slice, matching
  this project's own established "narrow first increment" pattern.
