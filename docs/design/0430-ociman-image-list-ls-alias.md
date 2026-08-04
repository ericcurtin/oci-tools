# Design note 0430: `ociman image list`/`ociman image ls`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_images.rs`,
`README.md`.

## What this closes

`ociman image` had only `exists`/`prune` — no `list`/`ls` at all,
even though real `podman image list`/`ls` are genuine, literal
aliases for top-level `podman images`, not a separate command with
its own behavior. This closes that gap.

## Real, checked-directly confirmation

`~/git/podman/cmd/podman/images/list.go:33-56`:

```go
imageListCmd = &cobra.Command{
    Use:     "list [options] [IMAGE]",
    Aliases: []string{"ls"},
    RunE:    images,
    ...
}
imagesCmd = &cobra.Command{
    Use:  "images [options] [IMAGE]",
    RunE: imageListCmd.RunE,
    ...
}
```

Confirmed directly: `imageListCmd` (registered with `Parent:
imageCmd`, giving `podman image list`/`ls`) and `imagesCmd`
(top-level `podman images`) share the exact same `RunE` function and
flag set verbatim — not two separately-maintained implementations
that merely happen to look similar.

## Implementation

- New `ImageCommand::List` variant, `#[command(alias = "ls")]`
  (clap's own native alias mechanism, matching real podman's
  `Aliases: []string{"ls"}` exactly), with the identical three fields
  `Command::Images` already has (`quiet`/`filter`/`format`) — each
  field's own doc comment is a one-liner referencing `Command::
  Images`'s own field for the full semantics, matching this
  project's own established convention for identical shared flags
  (e.g. `ocirun Create`'s fields referencing `Run`'s doc comments).
- The dispatch match arm for `ImageCommand::List` calls the exact
  same already-existing `cmd_images` free function `Command::Images`
  itself dispatches to — zero logic duplication, only duplicated
  flag *declarations* (clap needs each variant to own its own field
  list; the function underneath is shared).
- Updated `Command::Image`'s own top-level doc comment, which
  previously (accurately, at the time) said this subcommand family
  existed solely to host `exists`/`prune` (verbs with no flat
  top-level alias in real docker/podman at all) — now also correctly
  describes `list`/`ls` as a real, genuine alias for the
  already-existing flat `Command::Images`, hosted here because real
  podman itself puts it here too, not because this project needed a
  new home for otherwise-homeless logic.

## Tests

One new test in `tests/tests/ociman_images.rs`,
`image_list_and_ls_are_byte_identical_aliases_for_images`: asserts
`ociman image list`/`ociman image ls` produce **byte-identical**
stdout to `ociman images` for the same fixture state (not merely
"similar" output), including through the shared `-q`/`--quiet` flag.
All 20 prior tests in `ociman_images.rs` continue to pass unmodified
(21/21 total).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean, no diff), `cargo clippy --workspace --all-targets --locked --
-D warnings`, `cargo test --workspace --locked` (119 test-result
blocks, 0 failures, clean on the first full run), `python3 ci/
guards.py`, `cargo deny check`, `bash ci/native-ci.sh` (clean,
119/119), `bash ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg
-r` round trip). Touches only CLI dispatch (a new alias routing to
existing, unchanged logic), not any hot path at all — no benchmark
re-run needed.

## Deliberately still out of scope

`ociman container list`/`ociman container ls` (the identical real
alias relationship for `podman container list`/`ls` → `podman ps`,
`~/git/podman/cmd/podman/containers/list.go:11-21`) — a real,
confirmed, separate gap, deliberately not bundled into this same
increment since `Command::Ps` has nine fields to duplicate (`all`/
`quiet`/`filter`/`last`/`no_trunc`/`noheading`/`format`/`size`/
`sort`) versus `Command::Images`'s three, making it a meaningfully
larger, separate increment rather than a trivial twin of this one.
