# Design note 0153: `ociman logs --tail`

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Logs`'s new `tail` field,
`cmd_logs`'s new tail-trimming of its initial catch-up read, new
`tail_lines` helper); `tests/tests/ociman_logs.rs` (2 new integration
tests); `cmd_logs`'s own unit test module (5 new `tail_lines` tests).

## Closing 0152's own deferred gap

0152's own "what this doesn't do yet" named this directly: real
`docker logs`/`podman logs --tail N` (show only the last `N` already-
captured lines) wasn't implemented yet.

## Checked directly against real podman: no short alias, and `--tail 0` is a real, distinct value

Checked directly (`~/git/podman/cmd/podman/containers/logs.go`) rather
than assumed: real podman's own `--tail` has **no** short alias at
all — `-n`/`-t` are already taken by its own `--names`/`--timestamps`
flags, a real, easy mistake to make by analogy with `tail -n`. Real
podman's own default is a literal `-1` sentinel meaning "all lines";
this project's own `tail: Option<usize>` expresses the exact same
three-way distinction more idiomatically for Rust — `None` (the flag
simply not given) standing in for that `-1` default, `Some(0)` a real,
meaningful "show nothing at all" (distinct from `None`, checked
directly against real podman's own `getTailLog`/`GetLogFile`, which
only special-cases `Tail > 0` for actually reading lines — `Tail == 0`
still seeks to the very end and shows nothing before it), and
`Some(n)` for `n` real lines.

## A deliberately simpler algorithm than real podman's own, and why that's a legitimate narrowing here

Real podman's own `getTailLog` uses a genuine reverse-reader
(`reversereader.NewReverseReader`) to read a potentially huge log file
backwards without ever loading the whole thing into memory — a real,
warranted optimization for podman's own log format (individual
timestamped `LogLine` entries, no natural upper bound on total log
size across a long-lived container's whole life). This project's own
`container.log` is a much simpler plain concatenated byte stream (see
`docs/design/0025`'s own "combined, not kept separate" design), and
`--tail` is an opt-in flag that changes nothing about the default,
already-established (and already performance-verified, see
`docs/design/0150`) read-the-whole-file path when not given at all —
so a plain, whole-file, in-memory `tail_lines` (splitting on `\n` via
`split_inclusive`, keeping the last `N` chunks) was a legitimate,
narrower first choice, not a performance regression relative to
anything that existed before this flag did. Reverting to a real
reverse-reader for very large logs is a real, separately-scoped future
optimization if it's ever actually needed, not a correctness gap.

## Only the initial catch-up read is trimmed, matching real `podman logs --tail N -f` exactly

Checked directly (`GetLogFile`'s own `whence`/`getTailLog` split):
`--tail` only ever affects where the very first read starts from;
`--follow`'s own ongoing polling for new content afterward is
completely unaffected. `cmd_logs` matches this precisely: `tail_lines`
is applied once, to the buffer read before the follow loop begins;
`print_new_log_bytes` (the follow loop's own incremental reader) is
untouched, exactly as before 0153.

## Real, automated tests

Five new unit tests for `tail_lines` itself (returns everything when
`n` is at least the real line count; returns only the last `n` lines;
`0` is a real, distinct "nothing" result, not "all"; a final line with
no trailing `\n` is still counted and preserved correctly; empty input
is empty regardless of `n`). Two new integration tests in
`tests/tests/ociman_logs.rs`: `--tail 2`/`--tail 0`/`--tail 100`
(larger than the real line count)/no `--tail` at all, all checked
against one real, finished container's own four-line output; and
`--tail 1 -f` against a real, still-running container, confirming the
catch-up read is trimmed to just the last already-captured line while
new output produced afterward, still following, is never trimmed.
Both new integration tests verified stable across repeated runs.

Full `cargo build --workspace --locked`/`cargo test --workspace
--locked` (2 clean runs)/`cargo fmt --all --check`/`cargo clippy
--workspace --all-targets --locked -- -D warnings`/`python3
ci/guards.py`/`cargo deny check`/`bash ci/native-ci.sh` all clean.

## What this doesn't do yet

* **`--since`/`--until`** — real `docker logs`/`podman logs` also
  support showing only lines within a given timestamp range; not
  implemented here (this project's own log file has no per-line
  timestamp metadata at all yet to filter by — a bigger, separately-
  scoped change to the log format itself, not just the reading side).
* A real reverse-reader for very large log files — see above; a
  legitimate, deliberately deferred optimization, not a correctness
  gap for this increment's own actual scope.
