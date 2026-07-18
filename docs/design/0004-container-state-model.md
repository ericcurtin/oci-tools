# Design note 0004: container state model (milestone 3, part 2)

Status: implemented (second increment of milestone 3)
Scope: `oci_runtime_core::state`; `oci_cli_common::{identity refactor,
runtime_root}`; `ocirun state`/`list`.

Continues 0003: still no namespaces, cgroups, or process execution —
`create`/`start`/`kill`/`delete`/`exec`/`run`/`features` remain future
work, and the README milestone table still shows milestone 3 as "—".
This increment builds the state bookkeeping every one of those commands
will read and write, and gets it fully tested against a model of runc's
real on-disk/CLI behavior before anything creates an actual container.

## Why this order

`create` will need to: validate the bundle, allocate a state directory,
fork/clone into namespaces, and record the result. Only the last part —
"record the result, and let `state`/`list`/`delete` read it back" — has no
dependency on namespaces or privilege, so it can be built, tested, and
shipped on its own. Same reasoning as 0003 (ship the read-only/pure half
of a create/read pair first).

## `oci_runtime_core::state`

* `PersistedState` — the on-disk record at `<root>/<id>/state.json`:
  `ociVersion`/`id`/`status`/`pid`/`bundle`/`rootfs`/`created`/
  `annotations`, matching the field set runc's `state.json` and `runc
  state`/`list` output use (checked against `list.go`'s `containerState`
  struct and `state.go`'s construction of it).
* `StateStore` — a directory of these, rooted wherever `--root` points:
  `create` (exclusive — `AlreadyExists` if the ID is taken, matching
  runc/crun refusing to reuse a live container ID), `load`, `remove`,
  `list` (skips entries with a corrupt/unreadable `state.json` rather than
  failing the whole listing, matching runc's `list` behavior of logging
  and continuing). Writes go through a temp-file-in-the-same-directory +
  rename, the same atomic-ingest pattern `oci-store` and `oci-registry`
  already use, so a crash mid-write never corrupts `state.json`.
* **Status is derived, not trusted blindly**: `PersistedState::status` is
  whatever a command last wrote, but `effective_status()` downgrades
  `Created`/`Running` to `Stopped` once the recorded pid is no longer
  alive (checked via `/proc/<pid>` existence) — the same "don't believe a
  stale cached status" behavior runc/crun have, where `runc state` always
  re-derives status from the live process rather than echoing what an
  earlier command wrote. Documented limitation: liveness is existence-only
  (no start-time cross-check yet), so a reused PID could in principle read
  as "still alive" in the narrow window between a container exiting and
  `create`/`start` recording `/proc/<pid>/stat`'s start-time for a real
  cross-check — there is no real pid to record that against yet, so this
  is deferred to the increment that has one.
* `StateView` — the runc-compatible JSON *rendering* (`PersistedState::
  to_view()`), separate from the storage format: `pid` is always present
  (forced to `0` once stopped, never omitted) and `status` is always the
  freshly computed `effective_status()`, matching `runc state`'s `pid :=
  ...; if status == Stopped { pid = 0 }`.

## `oci_cli_common` additions

* `identity::effective_uid_gid()` (added in 0003) is now also the one
  place euid is read from; `storage::default_root()`'s bespoke
  `/proc/self/status` parsing was replaced with a call to it (one fewer
  copy of the same fifteen lines).
* `runtime_root::default_root(name)` — `/run/<name>` for root,
  `$XDG_RUNTIME_DIR/<name>` rootless, deliberately matching runc's
  `shouldHonorXDGRuntimeDir` default *exactly*, quirks included (a
  rootless invocation with `$XDG_RUNTIME_DIR` unset still defaults to
  `/run/<name>`, which such a user typically can't write to — that is
  runc's real behavior, and real deployments always have `$XDG_RUNTIME_DIR`
  set or pass `--root` explicitly). One acknowledged simplification: runc
  additionally treats "euid 0 inside a user namespace with `$USER` !=
  root" as rootless too; detecting that needs `/proc/self/uid_map`
  parsing for a fairly exotic setup, so plain `euid == 0` is "root" here.

## `ocirun state`/`list`

* `ocirun --root <dir> state <id>` — prints `to_view()` as JSON; a missing
  container renders through the same `error: container "<id>" does not
  exist` path every other oci-tools error uses (the message text
  coincidentally matches what `StateError::NotFound`'s `Display`
  produces — no special-casing needed).
* `ocirun --root <dir> list [--format table|json] [--quiet]` — matches
  runc's three-mode output. The table omits runc's `OWNER` column (no code
  path yet records who created a container — `create` will add that), a
  gap the doc comments flag rather than fake with a placeholder value.
* Default `--root` comes from `runtime_root::default_root("ocirun")`.

Tests: `oci_runtime_core::state`'s unit tests (19) cover the store and the
view/status-derivation logic in isolation. `tests/tests/ocirun_state.rs`
(new) exercises the built `ocirun` binary against a state store populated
directly via `StateStore` (the same crate the binary links against, so
this proves the CLI and the library format agree) — empty list, missing
container, a populated container's `state`/`list --quiet`/`list --format
json`, and the invalid-format error path.

## Decisions and risks

* **No `OWNER` column in `list`.** See above; will be added alongside
  `create`, which is the first code that knows a container's owning uid.
* **PID-reuse window in `effective_status`.** See above; acceptable until
  there is a real pid to record a start-time against.
