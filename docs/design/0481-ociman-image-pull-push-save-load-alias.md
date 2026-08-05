# Design note 0481: `ociman image pull`/`push`/`save`/`load` aliases

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_pull.rs` (new),
`tests/tests/ociman_push.rs`, `tests/tests/ociman_save.rs`.

## What this closes

Continuing the `ociman image` alias family `0478`/`0480` started:
`pull`, `push`, `save`, and `load` were still missing.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/images/pull.go:38-56`: `imagesPullCmd`
  (`Parent: imageCmd`) and `pullCmd` (top-level) share `Args`/`Use`/
  `Short`/`Long`/`RunE`/`ValidArgsFunction` verbatim.
- `~/git/podman/cmd/podman/images/push.go:36-58`: `imagePushCmd`
  (`Parent: imageCmd`) and `pushCmd` (top-level) share `Use`/`Short`/
  `Long`/`RunE`/`Args`/`ValidArgsFunction` verbatim.
- `~/git/podman/cmd/podman/images/save.go:26-60`: `imageSaveCommand`
  (`Parent: imageCmd`) and `saveCommand` (top-level) share `Args`/
  `Use`/`Short`/`Long`/`RunE`/`ValidArgsFunction` verbatim, plus
  `saveFlags` registered on both.
- `~/git/podman/cmd/podman/images/load.go:23-52`: `imageLoadCommand`
  (`Parent: imageCmd`) and `loadCommand` (top-level) share `Args`/
  `Use`/`Short`/`Long`/`RunE` verbatim, plus `loadFlags` registered on
  both.

## Implementation

The exact same pure CLI-surface dispatch-reuse shape `0478`/`0480`
already established — four new `ImageCommand` variants (`Pull`,
`Push`, `Save`, `Load`), each field-for-field identical to the
already-existing `Command::Pull`/`Push`/`Save`/`Load`, each
dispatching straight into the exact same free functions
(`cmd_pull`/`cmd_push`/`cmd_save`/`cmd_load`) the top-level commands
already call. `Command::Image`'s own top-level doc comment updated
again to list the newly-covered verbs.

## Tests

Four new/updated integration tests: `image_pull_is_a_byte_identical_
alias_for_pull` (new `tests/tests/ociman_pull.rs` — no dedicated
top-level `ociman pull` CLI-surface test file existed before this;
`ociman_tls_verify.rs`'s own `MockRegistry` already covers a real
network round trip, so this new file only covers the real, fast, no-
network-needed empty-reference error path, matching `ociman_push.rs`'s
own established "CLI-surface, no-network-needed" scope exactly),
`image_push_is_a_byte_identical_alias_for_push` (the same no-network
"unknown reference" error path `ociman_push.rs` already exercises),
`image_save_and_image_load_are_byte_identical_aliases` (a real archive
written by `ociman image save` is byte-identical to one written by
`ociman save`, and loads back correctly through `ociman image load`
into a fresh store). All existing tests in `ociman_load.rs`/
`ociman_push.rs`/`ociman_save.rs` pass unmodified; the new
`ociman_pull.rs` adds 2.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (122 test-result
blocks — one more than the prior 121, this increment's own new test
file — 0 failures on the first attempt), `python3 ci/guards.py`
(clean), `cargo deny check` (clean), `bash ci/native-ci.sh` (clean,
122/122 on the first attempt), `bash ci/build-deb.sh` (clean, real
`dpkg -i`/`--version`/`dpkg -r` round trip on the first attempt). No
benchmark re-run needed: none of `ociman image pull`/`push`/`save`/
`load` is exercised by `ci/bench.sh`, and this is a pure dispatch-
reuse addition touching no existing function's body at all.

## Deliberately still out of scope

- The rest of the same real `podman image` alias family: `import`,
  `inspect`, `mount`/`unmount`, `diff` — each its own future
  increment. `mount`/`unmount` specifically need more careful
  individual verification before porting: real `podman image mount`/
  `unmount` alias the *container* mount/unmount commands (`~/git/
  podman/cmd/podman/images/{mount,unmount}.go`), a genuinely
  different, cross-concept aliasing shape than every other member of
  this family, not yet independently confirmed in depth.
- Real `podman pull IMAGE [IMAGE...]` (multiple images in one call) —
  `ociman pull`/`ImageCommand::Pull` both still only accept a single
  `reference`, the same shape of pre-existing single-vs-multi
  divergence `0479` already found and fixed for `tag` — a real,
  separate, previously-unnoticed gap, left unfixed here to keep this
  increment's own scope to the alias addition alone.
