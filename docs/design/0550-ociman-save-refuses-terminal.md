# Design note 0550: `ociman save` refuses to write to a real terminal

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_save.rs`,
`tests/Cargo.toml`.

## What this closes

A real, previously-unnoticed bug: `ociman save` (and its `ociman image
save` alias, both sharing one `cmd_save`) previously wrote the raw,
binary tar archive straight onto stdout with no `--output` given,
*even when stdout was a real, interactive terminal*, and exited `0`.
Real `podman save` genuinely refuses to do this.

## Live-verified, not guessed

Using a real pty (`script -qec ...`) so both processes genuinely see a
terminal on stdout, side by side against a real installed `podman
4.9.3` and this project's own already-built `ociman`:

```
$ script -qec "podman save alpine; echo EXIT:$?" /dev/null
Error: refusing to save to terminal. Use -o flag or redirect
EXIT:125

$ script -qec "./target/release/ociman save docker.io/library/alpine:latest; echo EXIT:$?" /dev/null
<raw tar bytes dumped straight onto the terminal, corrupting its state>
EXIT:0
```

## Real, checked-directly confirmation

`~/git/podman/cmd/podman/images/save.go:110-115`:
```go
if len(saveOpts.Output) == 0 {
    saveOpts.Quiet = true
    fi := os.Stdout
    if term.IsTerminal(int(fi.Fd())) {
        return errors.New("refusing to save to terminal. Use -o flag or redirect")
    }
    ...
```
This runs unconditionally, right at the very top of `save`'s own
function, before any other work at all (image resolution included) —
genuinely reachable on every `save`/`image save` invocation with no
`-o`, not dead code.

## Real, functional gap (not a no-op)

Fixed by checking `std::io::stdout().is_terminal()` (already imported
in this file, and the identical pattern already used at
`cmd_stats`'s own terminal-clear-screen guard) right at the very top
of `cmd_save`, before ever resolving the image — matching real
podman's own identical "fail fast, before anything else" placement —
returning the exact same error wording real podman uses.

## Deliberately narrower scope for this increment

The identical class of bug exists on `ociman load`'s stdin side and
`ociman export`'s stdout side too (real podman has the matching
guards at `~/git/podman/cmd/podman/images/load.go:91-92` and
`~/git/podman/cmd/podman/containers/export.go:74-75`). Deliberately
**not** folded into this same increment — `save` alone is the
crisply-scoped fix here, matching this project's own established
one-gap-at-a-time convention; `load`/`export` are real, separate,
later increments.

## Tests

`save_refuses_to_write_to_a_real_terminal`
(`tests/tests/ociman_save.rs`): opens a genuine pty directly via
`rustix::pty` (`openpt`/`grantpt`/`unlockpt`/`ptsname`) — not just a
pipe, since `Command::output()`'s own captured stdout is never a
terminal, meaning this exact bug could otherwise hide behind every
other test in this file — spawns `ociman save` with its own stdout
connected to the pty's slave side, and asserts a real, immediate
failure with the exact real-podman-matching error text.
`tests/Cargo.toml`'s own `rustix` dev-dependency gained the `"pty"`
feature (additive only, no `Cargo.lock` change needed — the version
was already resolved).

Manually exercised beyond the automated tests, via `script(1)`: `ociman
save busybox:latest` with stdout genuinely a terminal (now refuses,
matching real podman's exact wording), stdout redirected to a file
(still works), and `-o` given explicitly (still works) — all three
also re-verified for the `ociman image save` alias.

## Verification

`cargo build --workspace --locked` (clean), `cargo fmt --all` (clean,
no changes needed for the new test), `cargo clippy --workspace
--all-targets --locked -- -D warnings` (clean), targeted
`ociman_save.rs` run (10/10, 9 pre-existing + 1 new), a full `cargo
test --workspace --locked` run (clean), `python3 ci/guards.py`
(clean), `cargo deny check` (clean, the `rustix` feature addition
introduces no new dependency, license, or advisory concern), `bash
ci/native-ci.sh` (clean), `bash ci/build-deb.sh` (clean, real `dpkg
-i`/`--version`/`dpkg -r` round trip). A small, targeted correctness
fix on an already-hot startup/exit path (`cmd_save`'s own very first
check) — no measurable perf change, no `ci/bench.sh` rerun needed.
