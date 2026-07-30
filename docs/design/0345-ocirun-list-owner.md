# Design note 0345: `ocirun list`'s own `OWNER` column

Status: implemented
Scope: `crates/oci-runtime-core/src/state.rs`, `bin/ocirun/src/main.rs`,
`tests/tests/ocirun_state.rs`.

## What this closes

A genuine, long-standing, forgotten gap: this project's own very first
state-model design note, `docs/design/0004` (milestone 3), explicitly
flagged `OWNER` as deferred — *"No `OWNER` column in `list`... will be
added alongside `create`, which is the first code that knows a
container's owning uid."* `create` shipped shortly after; the `OWNER`
column itself never did, and no later design note ever mentions it
again. Closing it now.

## Real, checked-directly semantics — two different real approaches

Both real reference implementations this binary claims compatibility
with have the identical column, computed two genuinely different ways:

- Real **runc** (`~/git/runc/list.go`): resolves it fresh at `list`
  time, per container, via `user.LookupId(uid)`, falling back to a
  literal `"#<uid>"` string if the lookup fails. Column order:
  `ID PID STATUS BUNDLE CREATED OWNER` (`fmt.Fprint(w, "ID\tPID\t
  STATUS\tBUNDLE\tCREATED\tOWNER\n")`).
- Real **crun** (`~/git/crun/src/list.c:128`, `~/git/crun/src/
  libcrun/container.c:1993`, `~/git/crun/src/libcrun/utils.c:2534`):
  resolves it once, at container-*creation* time, via `get_user_name
  (geteuid())` (a plain `getpwuid_r` call), and persists the result
  into `status.json` — `list` just reads it back. Falls back to an
  empty string (`xstrdup("")`) on lookup failure, not a placeholder.
  Same column order as runc.

This project's own `PersistedState`/`StateStore` model (`docs/design/
0004`) is explicitly "the same bookkeeping runc's `state.json` and
libcontainer's `State` do," but crun's own "resolve once, persist"
approach fits this project's already-established pattern better (the
same "capture at creation time, don't re-derive later" convention
`docs/design/0344`'s `BoxRecord.hostname` and `0207`'s image-env
capture both already use) — so this implements crun's own approach,
not runc's, and crun's own empty-string failure fallback over runc's
fabricated `"#<uid>"` placeholder (matching this project's own general
"absence over fabrication" preference, e.g. `0241`'s container-stats
doc comment).

## Implementation

`PersistedState`/`StateView` (`crates/oci-runtime-core/src/state.rs`)
each gained a new `owner: String` field — `#[serde(default)]` on
`PersistedState` so a `state.json` written before this field existed
deserializes it as an empty string (the same forward-compatible-record
convention `BoxRecord.hostname` established); always present, never
`Option`, on `StateView` (matching crun's own `status->owner` always
being a non-NULL, possibly-empty `xstrdup`, not an optional field).

New `current_user_name()` in `state.rs`: `libc::geteuid()` +
`libc::getpwuid_r`, buffer-sized via `sysconf(_SC_GETPW_R_SIZE_MAX)`
(falling back to 16KiB on the rare platform reporting no defined
limit) rather than crun's own fixed 200-byte buffer — a real, if
minor, robustness improvement over the reference implementation, not
just a port. `libc` is already a workspace dependency (used elsewhere
in this same crate for `setresuid`/`setgroups`/`kill`), so this is
**zero new dependencies** — matching this project's own established
`regex`-avoidance-style caution about growing its dependency surface
for a small feature (`docs/design/0273`).

Called from exactly one site, `StateStore::create` — the single
construction point for `PersistedState` in the entire workspace
(confirmed by grep), shared by every binary that creates a container
(`ocirun run`/`create`, `ociman run`/`create`, `ocicri`'s own
`__launch` re-exec, `ocibox enter`). Every one of them now records a
real `owner` in its own `state.json`, even though only `ocirun list`
actually *displays* it — matching real crun's/runc's own identical
scope (their own `list`/`state` asymmetry: `state` never shows
`owner`, only `list` does; ported the same way here, `ocirun state`'s
own output is genuinely unaffected).

`ocirun list`'s table gained the `OWNER` column as its last column,
matching real runc's/crun's own identical column order exactly
(`ID PID STATUS BUNDLE CREATED OWNER`); `list --format json` gained
`"owner"` automatically via the existing `StateView` serialization,
no separate wiring needed. `ociman`'s own `ContainerView`/
`ContainerInspectView` (separate structs, not a flatten of
`PersistedState`) are completely unaffected — real `podman ps`/
`inspect` have no `OWNER` concept at all, and this project's own
`ociman` output correctly stays that way.

## Verified

New unit tests in `state.rs`:
`create_records_the_real_effective_users_own_login_name_as_owner`
(checked against a real `whoami` subprocess, not just this crate's own
`current_user_name` called a second time — proving correctness against
the real system NSS database, not just self-consistency),
`to_view_carries_owner_through_unchanged`.

New integration test in `ocirun_state.rs`:
`list_table_and_json_report_the_real_owner_column` (both the table's
own header/row and `--format json`, again checked against a real
`whoami` subprocess).

Full workspace: `cargo build --workspace --locked` (confirms the new
field doesn't break `ociman`/`ocicri`/`ocibox`, every one of which
constructs a `PersistedState` indirectly through the same shared
`StateStore::create`), `cargo fmt --all --check`, `cargo clippy
--workspace --all-targets --locked -- -D warnings`, `cargo test
--workspace --locked` (113 test-result blocks, 0 failures), `python3
ci/guards.py`, `cargo deny check`.

## Still ahead

`ociman commit --iidfile` (a near-literal copy of `ociman build
--iidfile`'s own existing three-line pattern) and `ociman volume
rename`/`volume ls -q` remain separate, similarly-small, not-yet-
scoped candidates surveyed alongside this one.
