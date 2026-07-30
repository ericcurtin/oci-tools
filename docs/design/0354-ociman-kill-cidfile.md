# Design note 0354: `ociman kill --cidfile`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_kill.rs`.

## What this closes

`ociman stop`/`rm`/`restart`/`pause`/`unpause` all gained `--cidfile`
(`0310`, `0316`, `0318`, `0320`) — but `kill`, a genuinely close
sibling of every one of those, never did, even though real `podman
kill --cidfile` has existed the whole time. A real, previously-
unnoticed gap surfaced during last turn's own scoping survey.

## Real, checked-directly semantics

Read `~/git/podman/cmd/podman/containers/kill.go` directly:
`--cidfile` is a repeatable `StringArrayVar`; each file's own first
line only (`strings.Cut(string(content), "\n")`) is merged into the
same target list an explicit `CONTAINER` argument already builds —
identical to every one of `stop`/`rm`/`restart`/`pause`/`unpause`'s
own already-implemented `--cidfile` handling. No `--ignore` exists for
real `podman kill` at all (unlike `rm`/`stop`), so an unreadable
cidfile is a hard error — the same convention `pause`/`unpause
--cidfile` already established (rather than `rm --ignore`'s own
tolerant one).

## Implementation

A near-literal copy of `cmd_pause_or_unpause`'s own existing cidfile-
merge block, adapted for `cmd_kill`: `ids: &[String]` becomes an owned
`Vec<String>` extended with each cidfile's own first line, then
re-bound to `&[String]` for the rest of the function's own existing
`--all`/multi-target logic (all previously already correct, untouched)
to keep working against. `--all`/`--cidfile` mutual exclusion checked
first, matching every sibling command's own identical ordering.

## Verified

New tests in `ociman_kill.rs`: `kill_all_and_cidfile_together_is_a_
clear_error`, `kill_cidfile_reads_the_container_id_from_a_file_and_
ignores_trailing_content` (mirrors `ociman_pause.rs`'s own established
`--cidfile` test technique exactly), `kill_multiple_cidfiles_are_all_
merged_into_the_same_target_list` (two real, separately-running
containers, killed via two separate `--cidfile` flags in one call).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (113 test-result blocks,
0 failures), `python3 ci/guards.py`, `cargo deny check`.
