# Design note 0478: `ociman image tag`/`image untag` aliases

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_tag.rs`.

## What this closes

`ImageCommand` (`ociman image ...`) had exactly three variants —
`Exists`, `List` (`0430`), `Prune` (`0359`) — but real `podman` also
nests `tag`/`untag` (and further verbs, see "still out of scope"
below) under `podman image` as byte-identical wrappers around its
already-existing flat top-level commands. `ociman image tag`/`ociman
image untag` previously failed with clap's plain "unrecognized
subcommand" error rather than doing anything.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/images/tag.go:12-33`: `tagCommand`
  (top-level `podman tag`) and `imageTagCommand` (`Parent: imageCmd`,
  giving `podman image tag`) share `Args`/`Use`/`Short`/`Long`/
  `RunE`/`ValidArgsFunction` verbatim — a second `cobra.Command`
  struct pointing at the exact same fields, not a separately-
  maintained implementation. The exact same shape this project's own
  `0430`/`0431` already established for `list`/`ls`.
- `~/git/podman/cmd/podman/images/untag.go:10-31`: `untagCmd`/
  `imageUntagCmd` — the identical pattern.

## Implementation

A pure CLI-surface dispatch-reuse increment — no new logic of any
kind, matching the established `list`/`ls` alias precedent exactly:

- `ImageCommand` gains `Tag { source: String, target: String }` and
  `Untag { image: String, references: Vec<String> }` — field-for-
  field identical to the already-existing `Command::Tag`/`Command::
  Untag`.
- Two new dispatch arms: `ImageCommand::Tag { source, target } =>
  cmd_tag(&source, &target, cli.global.json)` and `ImageCommand::
  Untag { image, references } => cmd_untag(&image, &references)` —
  the exact same free functions the top-level `Command::Tag`/
  `Command::Untag` already call.
- `Command::Image`'s own top-level doc comment updated to document
  both new aliases and name the rest of the still-unported family
  explicitly (see below), matching how `0430`/`0431` each updated
  this same doc comment for their own additions.

## A real, separate gap noticed but deliberately not fixed here

`Command::Tag` itself only ever accepts a single `target`, while real
podman's own `tag IMAGE TARGET_NAME [TARGET_NAME...]`
(`cobra.MinimumNArgs(2)`, `args[1:]` as a slice) accepts *multiple*
target names in one call. This is a real, previously-unnoticed
divergence — but a separate concern from adding the `image tag`
alias (which simply inherits whatever `Command::Tag`'s own existing
signature already is), so it is deliberately left unfixed here rather
than silently bundled into an "alias" increment; flagged as a future
candidate below instead of attempted now.

## Tests

Two new integration tests in `tests/tests/ociman_tag.rs`:
`image_tag_is_a_byte_identical_alias_for_tag`/`image_untag_is_a_byte_
identical_alias_for_untag`, matching `ociman_images.rs`'s own
established "byte-identical alias" test shape. All 12 tests in the
file pass (10 prior + 2 new).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (121 test-result
blocks, 0 failures on the first attempt), `python3 ci/guards.py`
(clean), `cargo deny check` (clean), `bash ci/native-ci.sh` (one
transient, already-documented flaky failure in `ocicri_container.rs`
on the first attempt, confirmed unrelated and passing instantly in
isolation; second attempt clean 121/121), `bash ci/build-deb.sh`
(clean, real `dpkg -i`/`--version`/`dpkg -r` round trip on the first
attempt). No benchmark re-run needed: neither `ociman image tag` nor
`untag` is exercised by `ci/bench.sh`, and this is a pure dispatch-
reuse addition touching no existing function's body at all.

## Deliberately still out of scope

- The rest of the same real `podman image` alias family: `history`,
  `rm` (maps onto the already-existing `cmd_rmi`), `push`, `pull`,
  `save`, `load`, `import`, `inspect`, `mount`/`unmount`, `diff` —
  each checked directly as the identical shared-`RunE` shape
  (`~/git/podman/cmd/podman/images/{history,rm,push,pull,save,load,
  import,inspect,mount,unmount,diff}.go`), each its own equally small
  future increment.
- `Command::Tag`'s own single-target-only scope (see above) — a real,
  separate, previously-unnoticed divergence from real podman's own
  `tag IMAGE TARGET [TARGET...]`, left unfixed here.
