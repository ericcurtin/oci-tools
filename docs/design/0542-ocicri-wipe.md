# Design note 0542: `ocicri wipe`

Status: implemented
Scope: `bin/ocicri/src/main.rs`, `tests/tests/ocicri_wipe.rs`.

## What this closes

`docs/design/0532`'s own module doc comment explicitly named `wipe` as
one of real `crio`'s own remaining subcommands, deferred to "each its
own future increment." This closes it: a new `ocicri wipe` CLI
subcommand that removes every stored pod-sandbox/container record and
its bundle.

## Real, checked-directly confirmation

- Command/flag definition: `~/git/cri-o/internal/criocli/wipe.go:18-
  29` -- `WipeCommand` (`Usage: "wipe CRI-O's container and image
  storage"`), one `--force`/`-f` bool flag.
- Registered as a real top-level command:
  `~/git/cri-o/cmd/crio/main.go:161-168` (`app.Commands =
  criocli.DefaultCommands` plus `WipeCommand`).
- Real, live-consumed action: `crioWipe` (`wipe.go:31-115`), gated by
  `c.IsSet("force")` at `wipe.go:44,90`; the actual wipe,
  `ContainerStore.wipeCrio` (`wipe.go:135-153`), calls real
  `store.Unmount`/`store.DeleteContainer`/`store.DeleteImage`
  (`wipe.go:169-193`) -- traced the full call chain to confirm this is
  real, executed code, not a placeholder.

## Real functional gap vs. faithful no-op vs. deliberate narrowing

- **Real functional gap, closed here:** bulk-clearing every stored
  container/pod-sandbox record and its bundle. This project previously
  had no such operation at all -- only one-at-a-time
  `RemoveContainer`/`RemovePodSandbox` RPCs. All the primitives already
  existed and were already exercised by those same RPC handlers
  (`records::load_all`/`remove`, `bundle::remove`,
  `container::container_root`, `sandbox::sandbox_root`) -- `cmd_wipe`
  just iterates both record families and reuses them directly.
- **Faithful no-op: `--force`/`-f`.** Real crio's own `--force`
  (`wipe.go:44-60`) only ever skips a version-file-based "did the node
  reboot / did crio upgrade since the last wipe" gate
  (`version.ShouldCrioWipe`) before deciding whether to wipe at all.
  This project has no version-file/unclean-shutdown-tracking concept
  of any kind (a real, pre-existing, separate gap, not introduced
  here) -- so there is no gate here to skip in the first place: every
  `ocicri wipe` invocation already wipes unconditionally, whether
  `--force` is given or not. The same "nothing to skip" reasoning
  class `ociman commit --quiet` (`0523`) already established.
- **Deliberate, honest narrowing: no image wipe.** Real crio's own
  `wipeCrio` also deletes every image it considers "its own"
  (`getCrioContainersAndImages`, `wipe.go:161-193`, tagged via
  `storage.IsCrioContainer` -- real `containers/storage`'s own
  per-tool metadata). This project's own `ocicri` deliberately shares
  one plain `oci_store` with `ociman` instead (`image_service.rs`'s
  own module doc comment), which has no per-tool ownership tagging at
  all: an indiscriminate image wipe here would risk deleting
  `ociman`'s own images too. Wiping only what this project can
  precisely, unambiguously identify as its own -- container/sandbox
  records -- is the honestly-scoped slice, the same kind of narrowing
  this codebase already uses elsewhere.
- **Deliberate narrowing: no live-process handling.** No
  SIGKILL-and-wait cascade the way `RemoveContainer`/
  `RemovePodSandbox`'s own forceful RPC paths have
  (`force_kill_and_reconcile` in `runtime_service.rs`). This matches
  real crio's own identical `deleteContainer` (`wipe.go:169-181`),
  which likewise only ever unmounts and deletes storage, no explicit
  kill step of its own either -- real crio's own primary invocation
  model for this command is a systemd `ExecStartPre` run *before* the
  server itself starts, and this project's identical assumption (not
  running concurrently against a live `ocicri` server on the same
  storage root) is the same honest scope, not a shortcut.

## Implementation

`bin/ocicri/src/main.rs`: `Command::Wipe { force: bool }`
(`#[arg(short, long)]`) added alongside the existing `Version` variant,
full doc comment covering both narrowings above. Dispatch converted
from a single `if let` to a `match` over `cli.command`. `cmd_wipe`
iterates `container::load_all`/`sandbox::load_all` (both already
sorted newest-first) under `oci_cli_common::storage::default_root()`,
removing each container's bundle (`bundle::remove`) then its record
(`container::remove`), then every sandbox record (`sandbox::remove`),
printing one `Deleted container <id>`/`Deleted pod sandbox <id>` line
per removal (plain-text mode) or a `WipeReport { containers,
pod_sandboxes }` JSON object (global `--json`, the same "global flag,
not a redundant local one" convention `version`/`0532` already
established).

## Tests

`tests/tests/ocicri_wipe.rs`, six new integration tests, following the
same real-server-over-a-real-socket pattern `ocicri_pod_sandbox.rs`/
`ocicri_container.rs` already established (spawning the actual built
`ocicri` binary, driving real `RunPodSandbox`/`CreateContainer` RPCs
via the shared generated `tonic` client):

- `wipe_on_an_empty_store_succeeds_silently` / `wipe_json_on_an_empty_
  store_reports_two_empty_lists`
- `wipe_force_flag_is_accepted_and_behaves_identically`
- `wipe_removes_every_pod_sandbox_record` -- creates two real
  sandboxes, kills the server, wipes, respawns a fresh server against
  the same storage root, and confirms `ListPodSandbox` is empty (a
  genuine on-disk effect, not just in-memory)
- `wipe_removes_every_container_record_and_its_bundle` -- creates a
  real container (real busybox rootfs extraction via `seed_image`),
  confirms the bundle exists, wipes, and confirms both the bundle
  directory and the record are gone (again verified by respawning)
- `wipe_json_reports_every_removed_container_and_pod_sandbox_id` -- a
  mixed store (one of each), asserting the exact IDs in the JSON
  report

Manually exercised beyond the automated tests: a real, standalone
`ocicri --listen ...` server spawned in the background, killed, then
`ocicri wipe`/`wipe --json`/`wipe --force` run directly against its
storage root (all three exit 0, `--json` emitting the expected empty
`{"containers": [], "pod_sandboxes": []}`); confirmed `ocicri wipe
--help`/`ocicri --help` render correctly.

## Verification

`cargo build --workspace --locked` (clean), `cargo fmt --all` (clean,
no changes needed for the new test file), `cargo clippy --workspace
--all-targets --locked -- -D warnings` (clean), targeted
`ocicri_container.rs`/`ocicri_version_cli.rs`/`ocicri_pod_sandbox.rs`/
`ocicri_wipe.rs` runs (41/9/4/6 respectively, 0 failures; one earlier
attempt hit the already-documented transient `ocicri_container.rs`-
class flakiness from concurrent host load, confirmed transient by an
immediate clean rerun), a full `cargo test --workspace --locked` run
(132 test-result blocks -- 126 pre-existing plus this increment's own
6 new tests -- 0 failures), `python3 ci/guards.py` (clean), `cargo
deny check` (clean), `bash ci/native-ci.sh` (clean), `bash
ci/build-deb.sh` (clean, real `dpkg -i`/`--version`/`dpkg -r` round
trip). Pure CLI-parsing plus a small, already-precedented
record-and-bundle-removal addition, reusing existing RPC-handler
primitives directly -- no hot path touched (this binary's own
server-serving performance, not process startup, is what matters per
its own module doc comment; a CLI-only subcommand affects neither), no
`ci/bench.sh` rerun needed.
