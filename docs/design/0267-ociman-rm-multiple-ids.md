# Design note 0267: `ociman rm <id1> <id2> ...`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_ps.rs`.

## Closing the gap `0266` left open

`0266` gave `ociman rm` a `--all` flag but still only accepted exactly
one explicit container ID otherwise. Real `podman rm id1 id2 id3` (no
`--all` at all) removes every named container in a single call. This
note closes that gap, following up on `0266`'s own "still ahead" note.

## Semantics, checked directly against a real installed `podman`

Real `podman rm` was tested directly (`podman create`/`podman rm`
against a handful of throwaway containers) rather than assumed, and it
turned out to have two distinct, real policies depending on *why* a
given target can't be removed:

1. **Unresolvable name/ID**: every given identifier is resolved to a
   real container *before* anything is removed at all. If even one of
   them doesn't resolve to anything real, the whole call aborts and
   nothing is removed — confirmed directly: `podman rm valid1
   bogus-name valid2` left both `valid1` and `valid2` untouched, not
   just `bogus-name` unremoved. A typo in one name shouldn't
   accidentally take down unrelated, correctly-named containers given
   alongside it.
2. **A different, per-container removal failure** (still running,
   without `--force`) does *not* block removing the other,
   already-resolved targets — confirmed directly: `podman rm a b c`
   with `b` running and no `--force` still removed `a` and `c`, only
   refusing `b`. This matches `--all`'s own existing continue-past-
   failure policy (`0266`), just scoped to an explicit list instead of
   every container.

`ociman rm` now implements exactly this two-phase split: a preflight
loop calling the existing `resolve_container_id` on every given
identifier (aborting immediately, before touching anything, on the
first one that doesn't resolve), followed by a second loop that
attempts to actually remove each one, continuing past any individual
failure and surfacing the first real error at the end — the same
already-proven "continue past failure, report the first error"
pattern `--all` (`0266`) and `ocibox rm --all` both already use.

The CLI shape changes from `id: Option<String>` to `ids: Vec<String>`
(clap already treats a `Vec<String>` positional as zero-or-more args
naturally); `--all` remains mutually exclusive with a non-empty `ids`,
and an empty `ids` without `--all` is still a clear error, both
unchanged from before.

## Verified

Integration (`tests/tests/ociman_ps.rs`, three new tests):

- `ociman rm id1 id2` removes both named containers, printing each
  removed id.
- One unresolvable name among otherwise-valid ones aborts the whole
  call: neither valid container is removed, and the error names the
  unresolvable one.
- One non-stopped (created-but-never-started) container among
  otherwise-removable ones is refused on its own, but the other,
  already-resolved valid containers are still removed — matching the
  real, checked-directly `podman` behavior above.

Existing single-ID, `--all`, and error-path tests (`0266` and earlier)
all still pass unchanged, confirming no regression to prior behavior.

Full workspace: `cargo build`/`test --workspace`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`python3 ci/guards.py`, `cargo deny check`, `ci/native-ci.sh`,
`ci/build-deb.sh`.

## Still ahead

`ociman images --filter` is implemented in `0268`.
</content>
</invoke>
