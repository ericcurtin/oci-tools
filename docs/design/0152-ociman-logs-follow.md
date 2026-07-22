# Design note 0152: `ociman logs -f`/`--follow`

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Logs`'s new `follow` field,
`cmd_logs`'s new polling logic, new `print_new_log_bytes` helper);
`tests/tests/ociman_logs.rs` (2 new tests).

## Closing the gap 0025 explicitly deferred

`cmd_logs`'s own doc comment had said so directly since the very first
`ociman logs` increment (0025): "Doesn't yet support `-f`/`--follow`
(tailing a still-running container's output live) — only ever prints
what's been captured so far and exits." A real, commonly-used
`docker logs`/`podman logs` flag, picked up here.

## The log file is already unbuffered and append-only — no new plumbing needed to tail it

`oci_runtime_core::launch::spawn_log_tee_thread` (the background
thread that copies a container's own combined stdout/stderr into
`container.log`) already writes through a plain `std::fs::File` in
append mode, with no internal buffering of its own (`write_all`
directly, no `BufWriter`) — so a *separate* process re-reading that
same file sees new bytes the moment they're written to the kernel's
page cache, no artificial delay of this project's own making. This
means `--follow` needed no changes to the log-writing side at all:
just a polling reader on the *reading* side, checking the container's
own persisted status periodically to know when to stop.

## Checked directly: real `docker`/`podman logs -f` stop on their own once the container exits

Confirmed directly (a real `podman logs -f` against a container that
then exits on its own returns control to the shell immediately, rather
than hanging until interrupted) before implementing this — a real,
specific behavior worth checking rather than assuming "follow" means
"never returns except via Ctrl-C". `cmd_logs`'s own polling loop reads
new content, checks whether the container's own `effective_status()`
is still `Running`, and — once it isn't — does one final read (to
catch anything written between the container's last status transition
and this check) before returning, matching that exactly.

## A real bug caught by the tests, not guessed at in advance

The first version of this feature treated "the log file doesn't exist
yet" the same for both `-f` and plain `logs`: print nothing, return
immediately. This is correct for plain `logs` (nothing was ever
captured), but wrong for `-f` against a container that had *just*
started (`ociman run -d` immediately followed by `ociman logs -f`,
exactly what the first version of the new integration test did): the
log-tee thread only creates `container.log` once the container's own
process is actually about to start, so a `-f` invocation racing that
window would see `NotFound`, print nothing, and exit immediately —
silently losing the entire follow, not because there was truly nothing
to show, but because it was simply too early. Caught directly by
running the new test (it failed with an empty `stdout` on the very
first run, not a hypothetical worried about in advance) and fixed by
polling for the file to *appear* too, for as long as the container's
own status is anything short of `Stopped`, before concluding there is
genuinely nothing to follow.

## Real, automated tests

Two new integration tests in `tests/tests/ociman_logs.rs`: a real
detached, running container's own output streamed live via `-f`,
confirmed to have taken a real, non-trivial duration (proving it
actually followed, not just read once) while still returning well
before any reasonable timeout once the container stopped on its own;
`-f` against an already-stopped container returning immediately
(matching plain `logs`'s own existing behavior for that case, no
pointless polling). Both new tests, plus all 3 pre-existing ones,
verified stable across repeated runs.

Full `cargo build --workspace --locked`/`cargo test --workspace
--locked` (2 clean runs)/`cargo fmt --all --check`/`cargo clippy
--workspace --all-targets --locked -- -D warnings`/`python3
ci/guards.py`/`cargo deny check`/`bash ci/native-ci.sh` all clean.

## What this doesn't do yet

* **`--tail N`**/**`--since`** — real `docker logs`/`podman logs` also
  support showing only the last `N` lines or lines since a given
  timestamp; not implemented here, `-f` always starts from the very
  beginning of the captured log (matching this project's own existing
  plain `logs` behavior, which already always shows the whole thing).
* **Separate stdout/stderr streams** — same pre-existing gap 0025
  already recorded (combined, not kept separate); unrelated to `-f`
  specifically.
