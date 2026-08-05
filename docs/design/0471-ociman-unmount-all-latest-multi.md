# Design note 0471: `ociman unmount --all`/`--latest`/multi-id support

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_mount.rs`.

## Correction to `0470`'s own claim

`0470`'s own "still out of scope" section stated real `podman
unmount` "notably has no `--latest` of its own at all, unlike
`mount`." **This was wrong** — re-checked directly this increment:
`~/git/podman/cmd/podman/containers/unmount.go:67`/`74`:
`validate.AddLatestFlag(unmountCommand, &unmountOpts.Latest)` (and
the identical call for its `containerUnmountCommand` alias) *does*
register a real `--latest`/`-l` flag on `unmount` too. The earlier
research pass that fed `0470` only checked `unmountFlags` (lines
57-61, which registers `--all`/`--force` but not `--latest` — that
one is registered separately, in `init()`) and stopped there without
checking `init()` itself. Documented here transparently rather than
silently corrected, matching this project's own established
convention for a previously-wrong claim.

## What this closes

`ociman unmount` accepted exactly one container and nothing else —
no `--all`, no `--latest`, no multiple positional targets. Real
`podman unmount` supports all three.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/unmount.go:57-61`
  (`unmountFlags`): `--all`/`-a`, `--force`/`-f`.
- Lines 67/74: `--latest`/`-l` (see correction above).
- Lines 28-35/44-51 (`Args`): `validate.CheckAllLatestAndIDFile(cmd,
  args, false, "")` — `ignoreArgLen=false`, unlike `mount`'s own
  `true` (`0470`). Exact validation order (`~/git/podman/cmd/podman/
  validate/args.go:55-111`, ported here verbatim): `--all`+`--latest`
  together is `"--all and --latest cannot be used together"`; `--all`
  with an explicit container is `"no arguments are needed with
  --all"`; `--latest` with an explicit container is `"--latest and
  containers cannot be used together"`; none of the three given at
  all is `"you must provide at least one name or id"`.
- `~/git/podman/pkg/domain/infra/abi/containers.go:1565-1621`
  (`ContainerUnmount`): with `--all`, walks every storage container
  directly (checking `IsStorageContainerMounted` per one) *and*
  separately merges in every libpod container via `getContainers`
  (a real, checked-directly quirk of real podman's own two-source
  storage model, not replicated here — see below).

## Implementation

This project's own containers have no separate "is this one
currently mounted" refcounted state distinct from "does it exist at
all" (`Command::Unmount`'s own pre-existing doc comment: real
`unmount` is already an unconditional no-op regardless of state or
force). So real podman's own two-source (`StorageContainers()` +
`getContainers`) merge has no honest equivalent to replicate — the
single, flat container store is the only source of truth here, and
`--all` sweeps it directly, once, with no possibility of the
duplicate-report quirk real podman's own two-source model can
produce.

- `Command::Unmount`: `container: String` → `containers: Vec<String>`
  plus new `all: bool` (`-a`/`--all`), `latest: bool` (`-l`/
  `--latest`), and `force: bool` (`-f`/`--force`, accepted for real
  CLI compatibility but changes nothing at all — the same reasoning
  `rm --force` on an already-nonexistent target established).
- `cmd_unmount(ids: &[String], all: bool, latest: bool)`: the same
  four-check validation order as `CheckAllLatestAndIDFile` above,
  then: `--all` prints every existing container's own id (an
  unconditional sweep); `--latest` resolves and prints the single
  most-recently-created one (`resolve_latest_container`, the same
  shared primitive every other `--latest` command already uses); one
  explicit id keeps the original simplest path; two or more explicit
  ids follow `cmd_kill`'s own already-established two-phase
  convention (resolve every one first, aborting the whole call before
  printing anything if any of them don't exist, rather than partially
  succeeding).

## Tests

Eight new integration tests in `tests/tests/ociman_mount.rs`:
multi-id success, multi-id-with-one-unknown two-phase abort, `--all`,
`--latest`, and the four validation-error cases (`--all`+`--latest`,
`--all`+explicit, `--latest`+explicit, neither/none). A new shared
helper, `seed_and_run_stopped_container_named`, was needed for every
multi-container test: reusing the existing `seed_and_run_stopped_
container`'s own bare `ps -a -q` the moment a *second* container
shares the same store returns both containers' ids concatenated
across two lines (the exact same real bug `0470`'s own first draft
already hit and fixed once) — the new helper names each container
explicitly via `--name` and resolves it unambiguously via `ps -a -q
--filter name=...` instead. All 16 tests in the file pass (8 prior +
8 new).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (120 test-result
blocks, 0 failures on the first attempt), `python3 ci/guards.py`
(clean), `cargo deny check` (clean), `bash ci/native-ci.sh` (one
transient, already-documented flaky failure in `ocicri_container.rs`
on the first attempt, confirmed unrelated and passing instantly in
isolation, consistent with this dev host's long-running CPU-spinning
background process; second attempt clean 120/120), `bash
ci/build-deb.sh` (clean, real `dpkg -i`/`--version`/`dpkg -r` round
trip on the first attempt). No benchmark re-run needed: `ociman
unmount` is not exercised by `ci/bench.sh` at all.

## Deliberately still out of scope

`ociman mount --latest`/`--all` (the mirror-image gap on `mount`
itself, `0470`'s own first "still out of scope" bullet) — left for
its own future increment.
