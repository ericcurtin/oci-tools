# Design note 0431: `ociman container list`/`ociman container ls`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_container.rs`,
`README.md`.

## What this closes

`ociman container` had only `exists`/`prune` — no `list`/`ls`,
completing the sibling gap `0430`'s own design note deliberately
deferred (real `podman container list`/`ls` are the identical alias
relationship to `podman ps` that `podman image list`/`ls` are to
`podman images`, just with a larger, nine-field flag set to
duplicate).

## Real, checked-directly confirmation

`~/git/podman/cmd/podman/containers/list.go:10-21`:

```go
var listCmd = &cobra.Command{
    Use:     "list [options]",
    Aliases: []string{"ls"},
    RunE:    ps,
    ...
}
```

registered with `Parent: containerCmd` (`init()`, same file).
`listFlagSet` — the exact flag-registration function `listCmd` calls
— is defined once in `~/git/podman/cmd/podman/containers/ps.go:74`
and reused verbatim by both top-level `podman ps` and `podman
container list`/`ls`: confirmed directly, not assumed from the
similar name alone, the same care `0430`'s own research already took
for the `images`/`image list` pair.

## Implementation

- New `ContainerCommand::List` variant, `#[command(alias = "ls")]`
  (clap's own native alias mechanism, matching real podman's
  `Aliases: []string{"ls"}` exactly), with the identical nine fields
  `Command::Ps` already has (`all`/`quiet`/`filter`/`last`/
  `no_trunc`/`noheading`/`format`/`size`/`sort`) — each field's own
  doc comment is a one-liner referencing `Command::Ps`'s own field,
  matching the exact "thin alias, one-liner doc comments" convention
  `ImageCommand::List` (`0430`) already established.
- The dispatch match arm for `ContainerCommand::List` calls the exact
  same already-existing `cmd_ps` free function `Command::Ps` itself
  dispatches to — zero logic duplication, only duplicated flag
  *declarations*.
- Updated `Command::Container`'s own top-level doc comment (which
  previously, accurately at the time, said this family existed
  solely to host `exists`/`prune`) to also describe `list`/`ls` as a
  real, genuine alias — the identical update `0430` already made to
  `Command::Image`'s own doc comment for the analogous case.

## Tests

One new test in `tests/tests/ociman_container.rs`,
`container_list_and_ls_are_byte_identical_aliases_for_ps`: asserts
`ociman container list`/`ociman container ls` produce
**byte-identical** stdout to `ociman ps` for the same fixture state,
including through the shared `-a -q` flags. All 5 prior tests in
`ociman_container.rs` continue to pass unmodified (6/6 total).

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

With this increment, both real podman `list`/`ls` subresource
aliases this project's own existing `ImageCommand`/`ContainerCommand`
families had a natural home for (`image`/`container`) are now
closed. No further `list`/`ls` aliases of this exact shape remain
identified in this codebase.
