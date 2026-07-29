# Design note 0284: `ociman stats` default continuous-streaming mode

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_stats.rs`.

## Closing the one deliberately narrow gap `0145` left open

`0145` implemented `ociman stats --no-stream` (a real, one-shot
cgroup-v2-accounting sample) and left real `podman stats`'s own
*default* behavior — continuous, redrawing streaming — as a clear,
loud "not implemented yet, pass --no-stream" error rather than a
silent behavioral difference. This note closes that gap: every piece
`--no-stream` already needed (the real cgroup readers, the CPU%/MEM%
computation, the table renderer) is reused completely unchanged, this
is a loop wrapped around already-working, already-tested code.

## Real semantics, checked directly against a real installed `podman`

`podman stats --help`: `-i`/`--interval` (default `5` seconds),
`--no-reset` ("Disable resetting the screen between intervals").
`~/git/podman/cmd/podman/common/term.go`'s own `ClearScreen`: writes
the real ANSI clear-screen-and-home-cursor escape sequence
(`\x1b[2J\x1b[1;1H`), but **only** when stdout is a real terminal
(`term.IsTerminal`) — never when redirected to a file/pipe, which
would otherwise corrupt captured output with control characters.
Streams until interrupted (`Ctrl+C`) or the target container stops.

## Implementation

`cmd_stats`'s previous single-shot body is now `sample_container_stats`
— unchanged logic, just returning `Ok(None)` instead of a hard error
when the container isn't running, since that's now a meaningful,
non-error signal for the streaming loop to act on rather than always a
failure. `--no-stream` mode calls it once and still turns `None` into
the exact same "container ... is not running" error as before (a
pure, verified-unchanged refactor — all 5 pre-existing `--no-stream`
tests pass unmodified). The default streaming loop calls it every
`--interval` seconds, clears the screen first (via `std::io::
IsTerminal`, the same guard real podman's own `term.IsTerminal` check
implements) unless `--no-reset`, and ends — a real, honest success,
not an error — the moment the target container is no longer running.
No special `SIGINT` handling needed: an ordinary foreground loop
already behaves exactly like real `podman stats` under an unhandled
`Ctrl+C` (the process is simply killed), so `Ctrl+C` support came for
free.

## Verified

Integration (`tests/tests/ociman_stats.rs`): the previous "always
errors without `--no-stream`" test was replaced with one exercising
the real new default behavior end to end — a real, short-lived
(`sleep 2`) container, `ociman stats --interval 1 --no-reset <id>`
streams at least one real sample (the table header/row unchanged from
`--no-stream`), then ends cleanly with a real, honest "is no longer
running" message once the container's own process exits, the whole
call completing (not hanging) in a few real seconds. Run repeatedly to
confirm it isn't flaky.

Full workspace: `cargo build`/`test --workspace`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`python3 ci/guards.py`, `cargo deny check`, `ci/native-ci.sh`,
`ci/build-deb.sh`.

## Still ahead

Real `podman stats`'s own `--all`/`-a` (streaming every container, not
just one named one) and `--format` (custom Go-template output) remain
real, separately-scoped candidates.
</content>
</invoke>
