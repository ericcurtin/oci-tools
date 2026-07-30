# Design note 0346: `ociman commit --iidfile`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_commit.rs`.

## What this closes

`ociman build` already has `--iidfile` (writing the built image's own
digest to a file after a successful build), but `ociman commit` — which
computes and records the exact same kind of digest at the exact same
kind of point in its own function — had no equivalent at all. A real,
previously-unnoticed asymmetry between two commands that both produce
a new image.

## Real, checked-directly semantics

Read `~/git/podman/cmd/podman/containers/commit.go` directly:
`--iidfile` is a plain string flag; after a successful commit,
`os.WriteFile(iidFile, []byte(response.Id), 0o644)` — the bare image ID
string, no trailing newline, no surrounding whitespace — the exact
same shape real `podman build --iidfile` already writes (and the exact
same shape this project's own `ociman build --iidfile` already
established and tests for, `docs/design/0134`).

## Implementation

A near-literal copy of `ociman build --iidfile`'s own existing
three-line write block (`build.rs`), placed in `commit_inner` right
after `store.put_image(...)` — the same point in the function
`manifest_ingested.digest` (this command's own equivalent of `build`'s
digest) is already fully computed and about to be printed to stdout.
No new logic, no new concept: `Command::Commit` gained one
`iidfile: Option<PathBuf>` field (`--iidfile`), threaded through
`cmd_commit`/`commit_inner` (both already `#[allow(clippy::too_many_
arguments)]`) as one more parameter.

## Verified

New test: `commit_iidfile_flag_writes_the_committed_images_own_digest_
with_no_trailing_newline` — mirrors `ociman_build.rs`'s own existing
`iidfile_flag_writes_the_built_images_own_digest_with_no_trailing_
newline` test exactly, plus an extra assertion that the file's own
content matches what was printed to stdout (both must be the exact
same digest string).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (113 test-result blocks,
0 failures), `python3 ci/guards.py`, `cargo deny check`.

## Still ahead

`ociman volume rename` and `ociman volume ls -q`/`--quiet` remain
separate, similarly-small, not-yet-scoped candidates surveyed
alongside this one.
