# Design note 0405: `ocibox rm --rm-home`

Status: implemented
Scope: `bin/ocibox/src/main.rs`, `tests/tests/ocibox_list_rm.rs`,
`README.md`.

## What this closes

Real `distrobox rm --rm-home` had no `ocibox rm` equivalent — a real
CLI flag `ocibox` would reject as unrecognized, unlike `--force`
(already accepted, `0321`) if a script or muscle-memory habit passed
it.

## Real, checked-directly confirmation — and a genuine surprise found along the way

A surface reading of real distrobox's own `--rm-home` help text
("Remove container's home directory") suggested a straightforward
unconditional-removal port, guarded only by the same "never the real
host `$HOME`" safety check `ocibox create --home` already documents.
Reading the actual implementation directly instead
(`~/git/distrobox/pkg/commands/rm.go`'s own `removeContainer`)
revealed something narrower and worth documenting precisely:

```go
removeHome := false
if removeHomeRequested && !noTTY && inspectOutput.ContainerHome != userHome {
    question := fmt.Sprintf("Do you really want to remove custom home of container %s (%s)?", ...)
    removeHome = c.prompter.Prompt(question, false)
}
```

`removeHome` only ever becomes `true` when **all three** hold: `--rm-
home` was given, `noTTY` (real distrobox's own `-y`/`--yes` flag) is
`false`, and the box's own home differs from the real user's own real
`$HOME` — and even then, only after a real interactive confirmation
prompt (defaulting to "no" if never answered) this project has no
equivalent of at all. Since `ocibox` has no interactive terminal
session concept whatsoever — every invocation is the real, checked-
directly equivalent of real distrobox's own always-`--yes`/`noTTY`
case (the same reasoning `create --pull`'s own doc comment already
gives for why `--yes` needs no flag of its own here) — real
distrobox's own `--rm-home` **never actually removes anything either**
under the one real mode this project can ever run in. A genuinely
faithful port is therefore this exact same real no-op, not the
unconditional removal a surface reading alone would have produced —
caught by reading the actual Go source before writing a line of Rust,
not assumed from the flag's own name or help text.

## Implementation

`Command::Rm` gains `rm_home: bool` (`--rm-home`), accepted and
immediately discarded at the one call site (matching `--force`'s own
existing `force: _` pattern) — real CLI compatibility, genuinely zero
behavioral effect, exactly like real distrobox's own actual behavior
under non-interactive operation.

## Tests

One new end-to-end integration test in `tests/tests/ocibox_list_rm.rs`,
`rm_rm_home_flag_is_a_real_no_op_and_never_removes_the_custom_home` —
a real box created with a custom `--home` directory, removed with
`--rm-home` given, proving the box's own storage directory is still
genuinely removed while the custom home directory (and a real canary
file inside it) survives completely untouched. All existing tests
continue to pass unmodified (13/13 in `ocibox_list_rm.rs`).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures), `python3
ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh`, `bash
ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg -r` round trip).
This touches only `ocibox rm`'s own CLI parsing, not any hot path at
all — no benchmark re-run needed.

## Deliberately still out of scope

Every other real `distrobox rm` flag beyond `--force`/`--rm-home`
(there are none — `--all`/`-y` were already ported at `0321`/covered
by this project's own always-non-interactive default) is now closed;
this command has no further known gap against real distrobox's own
actual `rm` surface.
