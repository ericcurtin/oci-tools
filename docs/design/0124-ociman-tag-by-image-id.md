# Design note 0124: `ociman tag`'s own source resolves by image ID too

Status: implemented
Scope: `bin/ociman/src/main.rs` (`cmd_tag`); `tests/tests/
ociman_tag.rs` (1 new test).

## Closing the last of the gaps 0122/0123 named

0123 explicitly deferred this: "`ociman tag`'s own source argument
still only resolves by tag reference, not by ID... lower priority than
`rmi`... left for a future increment if it turns out to matter in
practice." Picked back up here since it's small and, unlike `rmi`,
carries none of that increment's own removal-ambiguity design
question — `tag` only ever *adds* a new pointer, never removes one, so
there's nothing extra to decide: `podman tag <id> <new-tag>` against a
real installed `podman` (checked directly before writing any code)
behaves exactly like tagging by an ordinary reference, no `--force`
concept involved at all.

## Implementation

`cmd_tag`'s own `source_str` now resolves via the same
`resolve_image_by_reference_or_id` (0122) `inspect`/`rmi` already use,
instead of a bare `Reference::parse` + `store.resolve_image`. `target`
is unchanged — always a real reference, never ID-resolved (a *new* tag
being created has no existing image of its own to look up by ID in the
first place). The reported/printed `source` is the *resolved* canonical
reference (matching `ociman rmi`'s own already-established convention
for its own `RmiResult.reference`), not the raw ID the user typed, so
`ociman tag <id> <new-tag>`'s own `--json` output tells you which real
tag the ID actually meant.

## Real, automated tests

One new test: tagging by a real image's own short ID, verifying both
the JSON output (`source` is the resolved canonical reference, not the
raw ID) and that the new tag really does resolve to the same manifest
digest. All 4 pre-existing `ociman tag` tests still pass unmodified.
Full `cargo build --workspace --locked`/`cargo test --workspace
--locked` (2 clean runs)/`cargo fmt --all --check`/`cargo clippy
--workspace --all-targets --locked -- -D warnings` all clean.

With this, `ociman images`' own `DIGEST` column is now a fully usable
identifier across every command that takes an image argument except
`ociman run`/`ociman build ... FROM` (both real, checked-directly
lower priority: real workflows almost always run/build a *named*
image, and both `podman run <id>`/`docker run <id>` are far less
common in practice than `podman rmi <id>`/`podman tag <id> ...`, which
this and the previous increment already cover).
