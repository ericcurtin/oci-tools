# Design note 0551: `ociman load` refuses to read from a real terminal

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_load.rs`.

## What this closes

The `load`-side sibling of `0550` (`ociman save` refusing to write to
a terminal), deliberately deferred there as its own separate
increment: a real, previously-unnoticed bug — `ociman load` (and its
`ociman image load` alias, both sharing one `cmd_load`) previously
blocked forever reading from stdin with no `--input` given, even when
stdin was a real interactive terminal with nothing typed. Real
`podman load` genuinely refuses to do this outright instead.

## Live-verified, not guessed

Real installed `podman 4.9.3`, via a real pty:

```
$ script -qec "podman load; echo EXIT:$?" /dev/null
Error: cannot read from terminal, use command-line redirection or the --input flag
EXIT:125
```

This project's own binary, verified via a real, *held-open* pty (a
plain `script`/pipe wasn't enough here — `script`'s own stdin wiring
in this environment delivered an immediate EOF rather than a
genuinely open, blocking terminal read, unlike `save`'s own bug which
only needed stdout to be a pty; `load`'s bug needed a real Python
`pty.openpty()` with the master left open and nothing written to
reproduce the actual hang):

```
$ python3 -c "... master, slave = pty.openpty(); Popen([ociman, 'load'], stdin=slave, ...) ..."
poll result after 2s: None   # still running -- genuinely hung
```

## Real, checked-directly confirmation

`~/git/podman/cmd/podman/images/load.go:91-92`:
```go
if term.IsTerminal(int(os.Stdin.Fd())) {
    return errors.New("cannot read from terminal, use command-line redirection or the --input flag")
}
```
This runs at the very top of `load`'s own function, in the `else`
branch taken whenever `--input` is empty (i.e. "read from stdin"),
before any other work at all — genuinely reachable, not dead code.

## Real, functional gap — arguably worse than `save`'s own bug

Fixed by checking `std::io::stdin().is_terminal()` (the same pattern
already established for `cmd_save`'s own identical fix, `0550`) right
at the very top of `cmd_load`, before ever opening the store —
matching real podman's own identical "fail fast, before anything
else" placement, returning the exact same error wording real podman
uses. Arguably a worse pre-existing bug than `save`'s own: instead of
save's silent-corruption-but-still-exits, this was a silent,
*indefinite* hang with no feedback to the user at all.

## Tests

`load_refuses_to_read_from_a_real_terminal`
(`tests/tests/ociman_load.rs`): opens a genuine pty directly via
`rustix::pty` (the identical technique `ociman_save.rs`'s own
`save_refuses_to_write_to_a_real_terminal` established for `0550`),
connects the child's stdin to the pty's slave side, and asserts a
real, *immediate* failure (the test itself completes in ~0.01s,
confirming the fix returns instantly rather than blocking) with the
exact real-podman-matching error text. No new dependency/feature flag
needed — `tests/Cargo.toml`'s `rustix` dev-dependency already gained
`"pty"` in `0550`.

Manually exercised beyond the automated tests: `ociman load -i
<file>` and `ociman load < file` (both still work, stdin isn't a
terminal either way), and `ociman image load` with a real, held-open
pty on stdin (also refuses correctly, confirming the shared `cmd_load`
implementation fixes the alias too).

## Verification

`cargo build --workspace --locked` (clean), `cargo fmt --all` (clean,
no changes needed for the new test), `cargo clippy --workspace
--all-targets --locked -- -D warnings` (clean), targeted
`ociman_load.rs` run (7/7, 6 pre-existing + 1 new), `python3
ci/guards.py` (clean), `cargo deny check` (clean). A small, targeted
correctness fix on an already-hot startup/exit path (`cmd_load`'s own
very first check) — no measurable perf change, no `ci/bench.sh` rerun
needed.
