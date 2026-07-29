# Design note 0290: `ociman ps -n`/`--last`, `--no-trunc`, `--noheading`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_ps.rs`.

## Three real, mechanical table-output flags with no matching logic needed

Unlike the recent `ps --filter` additions (0272-0289, each a new
matching rule), these three real, checked-directly `podman ps` flags
are pure selection/output tweaks over data `ociman ps` already
computes — a deliberate change of pace after a long run of filter
work, picking the next well-scoped, low-risk candidate from a
different part of the same command's real surface instead.

## `-n`/`--last N`: real semantics, checked directly against `~/git/podman/pkg/ps/ps.go`

```go
all := options.All || options.Last > 0
...
if options.Last > 0 {
    sort.Sort(SortCreateTime{SortContainers: cons}) // newest first
    if options.Last < len(cons) {
        cons = cons[:options.Last]
    }
}
```

- A positive value overrides the default running-only visibility rule
  exactly like `--all` does — confirmed directly against a real
  installed `podman`: `podman ps -n 2` (no `-a`) shows a `created`
  (never-started) container a plain `podman ps` would hide.
- The selection itself keeps only the `n` most-recently-created
  containers (sorted newest-first internally, then truncated) —
  verified directly that the final *display* order is still ascending
  (oldest-of-the-kept-set first), not the internal descending sort
  order.
- `0` or negative (real podman's own literal default, `-1`) is a
  complete no-op: no visibility override, no truncation, identical to
  never passing the flag — matching the `options.Last > 0` gate
  exactly (an edge case double-checked directly since a stray
  `podman ps --last -1` on this dev host showed one puzzling result
  during research that didn't reproduce on a clean re-check; the
  *source code*'s own gate is unambiguous and is what this matches).

Implementation: `ociman ps`'s own `views` vector is already sorted
ascending by creation time for display before this check runs, so
"keep the `n` most recently created, still in ascending order" is
just that vector's own trailing slice (`views.split_off(views.len() -
last)`) — no second sort-then-resort dance needed the way real
podman's own descending-sort-then-truncate implementation does.

## `--no-trunc`: real semantics, checked directly against `~/git/podman/cmd/podman/containers/ps.go`

Real podman's own default `Command()` formatter truncates to 17
characters plus `...`; `--no-trunc` shows it verbatim. Real podman's
identical flag *also* un-truncates the container/image/pod ID columns
— a real no-op here specifically, since `ociman`'s own container ids
are already always the short, 12-hex-character form (`short_id()`)
with no separate full/long form to ever reveal; a real, honest
structural difference, not an oversight.

This closes a real, previously-existing (if harmless) drop-in-fidelity
gap along the way: before this change, `ociman ps`'s own COMMAND
column was **never** truncated at all — the default behavior actually
matched what real podman calls `--no-trunc`. Implementing `--no-trunc`
properly meant first adding the real *default* truncation it's meant
to disable.

## `--noheading`

A plain, mechanical "skip the header line" — matching real `podman ps
--noheading` exactly; has no effect on `--quiet`/`--json`, neither of
which ever prints a header at all.

## Verified

Integration (`tests/tests/ociman_ps.rs`, two new tests):

- `-n 2` (no `-a`) overrides visibility and keeps exactly the 2
  most-recently-created of 3 merely-`created` containers, in ascending
  display order.
- `-n` larger than the real count is a no-op (nothing dropped).
- `-n 0` (and the implicit default) is a real no-op: no override, no
  containers shown.
- The default table truncates a long real command to 17 characters
  plus `...` and shows a header.
- `--no-trunc` shows the full command with no `...`.
- `--noheading` drops the header row.

Regression: all 21 pre-existing `ociman_ps.rs` tests still pass
unmodified.

Full workspace: `cargo build`/`test --workspace` (111 test binaries),
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets --
-D warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh`.

## Still ahead

Real podman's own remaining `ps` flags (`--namespace`/`--ns`, `--pod`,
`-s`/`--size`, `--sync`, `--watch`, `--format`, `--external`,
`--latest`) remain further, separately-scoped candidates.
