# Design note 0552: `ociman export` refuses to write to a real terminal

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_export.rs`.

## What this closes

The `export`-side sibling of `0550`/`0551` (`ociman save`/`load`'s own
identical fixes), deliberately deferred as its own separate increment
in both: a real, previously-unnoticed bug — `ociman export` previously
wrote the raw, binary tar archive straight onto stdout with no
`--output` given, even when stdout was a real interactive terminal.
Real `podman export` genuinely refuses to do this.

## Live-verified, not guessed

Real installed `podman 4.9.3`, via a real pty:

```
$ podman container create --name t busybox:latest true
$ script -qec "podman export t; echo EXIT:$?" /dev/null
Error: refusing to export to terminal. Use -o flag or redirect
EXIT:125
```

This project's own binary, verified via a real, held-open pty (the
same technique `save`'s own fix, `0550`, established — a real,
genuinely open terminal, not just a pipe): with the bug still present,
`ociman export` began writing raw tar bytes (`bin\0\0\0...`, real file
content) directly onto the pty's own master side before the check was
added.

## Real, checked-directly confirmation

`~/git/podman/cmd/podman/containers/export.go:74-75`:
```go
file := os.Stdout
if term.IsTerminal(int(file.Fd())) {
    return errors.New("refusing to export to terminal. Use -o flag or redirect")
}
```
Checked directly that this runs *before* the container is ever
resolved: `export.go`'s own `registry.ContainerEngine().
ContainerExport(...)` call — the point resolution actually happens —
is the very last line of the whole function, well after this check.

## Real, functional gap (not a no-op)

Fixed by checking `std::io::stdout().is_terminal()` (the identical
pattern already established for `cmd_save`'s/`cmd_load`'s own fixes,
`0550`/`0551`) right at the very top of `cmd_export`, before ever
resolving the container — matching real podman's own identical "fail
fast, before anything else" placement, returning the exact same error
wording real podman uses.

## Tests

`export_refuses_to_write_to_a_real_terminal`
(`tests/tests/ociman_export.rs`): opens a genuine pty directly via
`rustix::pty` (the identical technique `0550`/`0551` already
established), connects the child's stdout to the pty's slave side, and
asserts a real, immediate failure with the exact real-podman-matching
error text — using a deliberately unresolvable container id to also
prove the check's own ordering (the error is genuinely about the
terminal, not "container ... does not exist", confirmed by asserting
that latter phrase is absent from stderr). No new dependency/feature
needed — `rustix`'s `"pty"` feature was already added in `0550`.

Manually exercised beyond the automated tests: `ociman export -o
<file>` and `ociman export > file` (both still work, stdout isn't a
terminal either way) against a real, stopped container.

## Verification

`cargo build --workspace --locked` (clean), `cargo fmt --all` (clean,
no changes needed for the new test), `cargo clippy --workspace
--all-targets --locked -- -D warnings` (clean), targeted
`ociman_export.rs` run (5/5, 4 pre-existing + 1 new), `python3
ci/guards.py` (clean), `cargo deny check` (clean). A small, targeted
correctness fix on an already-hot startup/exit path (`cmd_export`'s
own very first check) — no measurable perf change, no `ci/bench.sh`
rerun needed.
