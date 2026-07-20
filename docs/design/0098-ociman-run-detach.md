# Design note 0098: `ociman run -d`/`--detach` (milestone 3)

Status: implemented
Scope: `bin/ociman/Cargo.toml` (`rustix`'s `process`/`stdio` features),
`bin/ociman/src/main.rs` (`Command::Run`'s new `detach` flag,
`cmd_run`'s split into a setup phase plus the new `run_and_finalize`/
`wait_for_detached_container_to_start`), `tests/tests/
ociman_detach.rs`.

`ociman run` gained real `docker run -d`/`podman run -d`: the CLI
invocation itself returns immediately, printing the container's own
id, while the container keeps running in the background. Ranked
"medium, high value" by the ongoing small-gaps survey — arguably the
single most commonly-missing real flag before this increment — with
real groundwork already in place: `cmd_run` already tees the
container's own output to a real, persistent `container.log` file
independently of live terminal streaming (0025), so most of "capture
output for later" was already solved; what remained was real
daemonization.

## The setup phase stays synchronous; only the actual run gets forked away

`cmd_run` already had a natural seam: image resolution, layer
extraction, spec synthesis, and bundle validation all happen before
the container's own process ever starts, and a failure at any of
those points is reported immediately, synchronously, to the CLI's own
caller (matching real `docker run -d`, which still reports "no such
image" immediately rather than silently detaching first). This
increment splits `cmd_run` at exactly that seam: everything through
`oci_runtime_core::validate::validate` stays exactly where it always
was, unchanged, still synchronous either way; only the final "run to
completion and finalize state" step (extracted into a new,
shared function, `run_and_finalize`) is what gets forked into the
background for `--detach`.

## `run_and_finalize`: one function, two call sites, byte-identical logic either way

`run_and_finalize` is the *exact* logic that already lived inline in
`cmd_run` (the `record_running` callback, the systemd `CgroupSetup`,
the `run_reporting_pid` call, the failed-scope cleanup, and the
final `--rm`/exit-code-annotation state write) — moved verbatim into
its own function, not rewritten. The foreground path calls it
directly and then `std::process::exit`s with its own real return
value, exactly as `cmd_run` always did. The detached path calls the
*same* function from inside a forked child instead. Confirmed via the
full existing `ociman_run.rs`/`ociman_stop.rs`/`ociman_kill.rs`/etc.
test suites, all still passing unchanged: the foreground behavior is
provably identical to before this refactor.

## A real, ordinary `fork(2)` + `setsid(2)`, reusing this crate's own existing primitive

`oci_runtime_core::process::fork` (the exact same function
`ChildSetup`'s own relay-fork and `launch::create`/`run_reporting_pid`
already use) forks the detached "keeper" process. It then:

* Calls `setsid(2)` (`rustix::process::setsid`) to leave the
  original process group/session entirely — the real mechanism that
  makes a detached container survive the original shell's own exit
  (or a `SIGHUP` if it does), not merely "runs in the background of
  the same session" the way a shell's own `&` would.
* Redirects its own `stdin`/`stdout`/`stderr` to `/dev/null`
  (`rustix::stdio::dup2_std{in,out,err}`) — real Docker/podman's own
  documented convention: a detached container shows **no** live
  output on the original terminal at all, only `ociman logs`/`docker
  logs` retrieves it after the fact. The log-tee thread
  `run_reporting_pid` spawns internally still faithfully writes the
  real container output to `container.log` regardless (its *own*
  echo-to-this-process's-stdout half simply becomes a silent write to
  `/dev/null` instead, exactly the outcome wanted).
* Calls `run_and_finalize` exactly as the foreground path does, with
  its own freshly re-opened `StateStore` handle (not a shared, cloned
  one — `StateStore` is just a thin, cheap-to-recreate handle around a
  root path, so reopening it in the child is simpler, and just as
  correct, as making it `Clone`).

No new low-level primitive was needed in `oci-runtime-core` at all —
this is a genuine, ordinary Unix "detach into the background"
operation, architecturally unrelated to the namespace/cgroup/pivot_root
machinery `ChildSetup`'s own forks handle.

## Synchronization: poll the same persisted state file every other subcommand already reads

The original CLI invocation needs to know the keeper has gotten far
enough to report a real, running pid before it can safely print the
container id and return — rather than inventing a new pipe or signal
for this (which this project's own `launch.rs` uses extensively
*inside* a single container's own setup, but always within one still-
running process, never across a CLI invocation that's about to exit),
`wait_for_detached_container_to_start` simply polls the same
`state.json` `record_running`'s own callback already writes — the
exact same "a concurrent invocation sees something real" mechanism
`docs/design/0023` already established for `ociman exec`/`ps`/`rm`
against a still-foreground `run`, just applied to the detaching
invocation itself. A container whose own command exits almost
immediately (reaching `Stopped` before the poll loop ever observes
`Running`) is still correctly treated as success, matching real
`docker run -d`'s own behavior for exactly that case. A genuine setup
failure inside the forked child (state record removed before ever
reporting a pid) or the keeper process disappearing outright are both
real, reported errors, not silent hangs — bounded by a real timeout
either way.

## Real, manual verification against a real, freshly-pulled busybox

Built the release binary and exercised every real scenario before
writing any automated test: `run -d` returning in ~70ms while `ps`
immediately shows the container genuinely running; the container
correctly reaching `stopped` on its own once its command finishes,
with `ociman logs` correctly showing its real captured output; no
leftover or orphaned processes once it's done; `-d --rm` correctly
auto-removing the container's own record after it exits; a bad image
reference and a bad `--memory` value both failing synchronously
(sub-10ms) without ever forking at all.

## Real, automated tests

Three integration tests in `tests/tests/ociman_detach.rs`: `run -d`
returning almost immediately while the container is genuinely still
running, then reaching `stopped` on its own with real logs still
readable; `-d --rm` removing the container's own record after it
exits; and a setup failure (an unresolvable image) failing
synchronously with no container record left behind at all.

## Performance — `cmd_run` is an explicitly benchmarked function, A/B re-verified

`cmd_run`'s own internal call path changed directly (the extraction
of `run_and_finalize`), so this project's own "always re-verify" rule
applies even though the actual operations performed by the foreground
path are unchanged. A `git stash`/`git stash pop` A/B `hyperfine`
comparison against `ociman run --rm` (the ordinary, non-detached
path) was run twice: the *direction* flipped between the two runs
(1.04× "after" faster, then 1.04× "before" faster on a second pass) —
consistent with this command's own already-documented wide
contention-noise band (33-80ms), not a real, reproducible regression.

## What's still not here

* `ocirun update`/`pause`/`resume`, the build cache,
  `ONBUILD`/`HEALTHCHECK`, a symbolic `--chmod` mode — all still
  exactly as earlier increments left them, unrelated to this
  increment's own scope.
* Real docker/podman's own `-a`/`--attach` (re-attaching to an
  already-detached, already-running container's live output) — not
  attempted here; `ociman logs` already covers "read what's been
  captured so far," just not "stream it live."
