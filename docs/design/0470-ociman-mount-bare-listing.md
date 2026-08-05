# Design note 0470: `ociman mount` bare-invocation listing mode

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_mount.rs`.

## What this closes

`Command::Mount`'s own doc comment (`0362`) had already explicitly
flagged real `podman mount`'s bare-invocation "list every currently-
mounted container" mode as deliberately deferred for that first
slice. This closes it.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/mount.go:17-23`: the command's
  own doc string, verbatim: *"podman mount\n    Lists all mounted
  containers mount points if no container is specified\n\n  podman
  mount CONTAINER-NAME-OR-ID\n    Mounts the specified container and
  outputs the mountpoint"*.
- Lines 79-104 (`mount()`): with no positional args, `--all`, or
  `--latest` given, `ContainerMount` is called with an empty
  `nameOrIDs`, which (per `getContainers`'s own `default:` case with
  an empty `names` slice) resolves to nothing directly — falling
  through the empty-`reports` branch into a real, separate
  `StorageContainers()` walk that checks `IsStorageContainerMounted`
  per container and reports only the ones actually mounted right now.
  The final default (non-JSON, non-`--format`) output template
  (`report.New`+`.Parse(..., "{{range . }}{{.ID}}\t{{.Path}}\n{{end
  -}}")`) has **no header row at all**, just one `<id>\t<path>` line
  per container.
- `mountReporter.ID()` (lines 156-161): truncates to `m.Id[0:12]`
  unless `--no-trunc` is given — the default 12-character id this
  project's own `ps`/etc. already established the identical
  convention for.

## Implementation

Real podman's own bare-mode fallback walk exists because its own
storage layer tracks a genuine, separate "is this container's own
overlay currently mounted" refcounted state distinct from "does the
container exist at all." This project's own containers have no such
distinction (`Command::Mount`'s own original doc comment: "already
there, nothing to actually mount") — so the honest equivalent of
"every currently-mounted container" is "every existing container,"
except the one real, already-documented rootless-overlay-rootfs gap
case (`resolve_container_root`'s own doc comment, `docs/design/0146`)
this project genuinely can't resolve a plain root path for — silently
skipped from the listing (not a hard error aborting the whole call;
one unresolvable container shouldn't hide every other one's own
listing) rather than invented.

- `Command::Mount::container`: `String` → `Option<String>`.
- `cmd_mount(container: Option<&str>)`: with `None`, opens the
  container store, lists every `PersistedState`, sorts by `created`
  ascending (matching this project's own established `ps` default
  order — real podman's own bare-mode order is an unspecified
  storage-iteration order with no documented guarantee to match
  instead), skips any container whose bundle directory has a real
  `upper` dir (`rootfs_setup::upper_dir(&bundle_dir).exists()`), and
  prints `<first-12-hex-chars-of-id>\t<rootfs-path>` per line — the
  exact real default template, no header row. With `Some(container)`,
  behavior is unchanged from before this increment.

## Tests

Three new integration tests in `tests/tests/ociman_mount.rs`:
`mount_with_no_container_lists_every_container_sorted_by_creation_
time` (two named, distinguishable containers in the same store,
verified against the real exact `<id>\t<path>` two-line output in
creation order — named explicitly via `--name` and resolved via
`ps -a -q --filter name=...` rather than the shared `seed_and_run_
stopped_container` helper's own bare `ps -a -q`, which only
disambiguates correctly with exactly one container in the store at a
time; a real, previously-hit bug in this test's own first draft,
caught and fixed before landing: reusing that helper for a second
container already sharing the store returned both containers' ids
concatenated across two lines, silently corrupting the "one clean id"
assumption the rest of the test needed), `mount_with_no_container_and_
no_containers_at_all_prints_nothing` (a real, honest empty listing,
never an error), `mount_with_no_container_silently_skips_a_rootless_
overlay_rootfs_container` (written to pass either way this host
happens to support that optimization, the same convention the
existing single-container overlay-gap test already established). All
8 tests in the file pass (5 prior + 3 new).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (120 test-result
blocks, 0 failures), `python3 ci/guards.py` (clean), `cargo deny
check` (clean), `bash ci/native-ci.sh` (clean, 120/120 on the first
attempt), `bash ci/build-deb.sh` (clean, real `dpkg -i`/`--version`/
`dpkg -r` round trip on the first attempt). No benchmark re-run
needed: `ociman mount` is not exercised by `ci/bench.sh` at all.

## Deliberately still out of scope

- `ociman mount --latest`/`-l` and `--all` (real podman also supports
  targeting the single most-recently-created container, or every
  container explicitly, with the same mutual-exclusivity rules
  `0469` just established for `update`) — left for its own future
  increment.
- `ociman unmount` gaining multi-id/`--all` support (real `podman
  unmount CONTAINER [CONTAINER...]`/`--all`, checked directly,
  `~/git/podman/cmd/podman/containers/unmount.go` — notably has no
  `--latest` of its own at all, unlike `mount`) — a separate, still
  real gap, left for its own future increment.
- `--format`/`--no-trunc`/`--all` on `mount` itself (real podman's
  own richer output shapes for this same bare-mode listing) — this
  project's own single default line shape is the only one
  implemented so far.
