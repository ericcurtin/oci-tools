# Design note 0144: a real, computed `paused` status in `ocirun state`/`list` and `ociman ps`/`inspect`

Status: implemented
Scope: `crates/oci-runtime-core/src/state.rs` (new `Status::Paused`
variant, new `PersistedState::to_view_with_frozen`); `bin/ocirun/src/
main.rs` (`cmd_state`/`cmd_list`, new `is_frozen` helper); `bin/ociman/
src/main.rs` (`ContainerView`/`ContainerInspectView::from_state`, new
`display_status` helper); `tests/tests/ocirun_lifecycle.rs`,
`tests/tests/ociman_pause.rs`, `tests/src/lib.rs` (new `list_status`
helper).

## Closing the gap 0142/0143 both explicitly flagged

Both design docs named this directly: "no separate, derived `Paused`
status in [the CLI's own] `ps`/`state`/`list`/`inspect` output." A
frozen container's own persisted `status` field still literally reads
`"running"` — the freeze is entirely a kernel-level cgroup fact this
project never surfaced anywhere in its own display output. Picked back
up here, for both binaries.

## `Paused` is computed at display time, never persisted — matching real runc exactly

Checked directly against real runc's own source
(`~/git/runc/libcontainer/container_linux.go`): its own `isPaused()` is
a small helper called only when *rendering* a container's state,
reading the cgroup's own freezer file live every time — `runc`'s own
`state.json` itself never gains a `"paused"` status value written into
it. This project now does the exact same thing: `Status::Paused` was
added as a fifth `Status` variant (`as_str()` → `"paused"`), but it is
**never** written to `state.json`'s own `status` field — only the
original four variants (`Creating`/`Created`/`Running`/`Stopped`) are
ever persisted there, both before and after this change. Confirmed
this addition alone breaks nothing: `cargo build --workspace --locked`
and `cargo clippy --workspace --all-targets --locked -- -D warnings`
were both clean immediately after adding just the enum variant, before
any of the display-wiring work below — no other code anywhere in the
workspace assumed exactly four `Status` values.

## `ocirun`: `PersistedState::to_view_with_frozen`, wired into `cmd_state`/`cmd_list`

`PersistedState::to_view_with_frozen(&self, frozen: bool) -> StateView`
wraps the pre-existing `to_view()` and upgrades its `status` field from
`Running` to `Paused` when `frozen` is `true` — and *only* then; a
`frozen = true` argument passed against any other status (e.g. a
caller bug) is a deliberate no-op, never surfacing a nonsensical
"paused but not running" state. `to_view()` itself is unchanged and
still used wherever a frozen check genuinely doesn't apply.

`cmd_state`/`cmd_list` in `bin/ocirun/src/main.rs` gained a new private
`is_frozen(state: &PersistedState) -> bool` helper: it re-loads the
container's own bundle, resolves its real `cgroupsPath`-based cgroup
directory via the pre-existing `oci_runtime_core::cgroups::
directory_for` (the same resolution `resolve_cgroup_dir`/`cmd_update`/
`cmd_pause` already use), and reads back `cgroups::is_frozen`. Any
failure along that chain — no `cgroupsPath` set, the bundle failed to
load, the cgroup directory doesn't exist, the freezer file can't be
read — is tolerated as `false`, never a hard error: this is an
optional, best-effort display enhancement, not something that should
ever make `ocirun state`/`list` itself fail where it previously
succeeded.

## `ociman`: `display_status`, wired into `ContainerView`/`ContainerInspectView::from_state`

`ociman`'s own containers never have `cgroupsPath` set (systemd cgroup
driver, same reasoning 0143 already recorded) — so its own
`display_status(state) -> Status` helper instead re-derives the real,
current cgroup from the container's own recorded pid via the
pre-existing `cgroup_dir_for_running_pid` (the same resolution
`resolve_running_container_cgroup`/`cmd_top`/`cmd_pause` already use),
only even attempting this when `effective_status()` is already
`Running` (a `Created`/`Stopped` container has no meaningful "is it
frozen" question to ask). Both `ContainerView::from_state` ("`ps`") and
`ContainerInspectView::from_state` ("`inspect`") now call this instead
of `state.effective_status()` directly — the exact same
"one-shared-computation, two call sites" shape `resolve_running_
container_cgroup` itself already established for `cmd_top`/`cmd_pause`/
`cmd_unpause`.

## Real, automated tests

Two new unit tests in `crates/oci-runtime-core/src/state.rs`'s own test
module: `to_view_with_frozen` upgrades `Running`+`frozen=true` to
`Paused` (and leaves `Running`+`frozen=false` alone), and never
upgrades a non-`Running` status (`Creating`+`frozen=true` stays
`Creating`).

Extended the existing real, end-to-end freeze/thaw tests on both
binaries rather than adding new ones, since both already have a real
running, CPU-burning container paused and resumed for real —
`pause_freezes_and_resume_thaws_a_real_running_containers_own_cpu_usage`
(`ocirun_lifecycle.rs`) now also asserts `ocirun state` and `ocirun
list --format json` both report `"paused"` right after the real freeze
and `"running"` again right after the real thaw (new `list_status`
helper in `tests/src/lib.rs`, mirroring the pre-existing `state_status`
exactly). `pause_freezes_and_unpause_thaws_a_real_running_containers_
own_cpu_usage` (`ociman_pause.rs`) gained the equivalent assertions
against `ociman ps --json` (via the pre-existing `wait_for_container_
status` helper) and `ociman inspect --json` (new `inspect_status`
helper in the same file).

All pre-existing tests across both binaries — including every other
`ocirun state`/`list` and `ociman ps`/`inspect` test, none of which
involve a frozen container — still pass completely unmodified, since
`is_frozen`/`display_status` are both no-ops (return the pre-existing
status untouched) for anything that was never paused in the first
place. Full `cargo build --workspace --locked`/`cargo test --workspace
--locked` (2 clean runs)/`cargo fmt --all --check`/`cargo clippy
--workspace --all-targets --locked -- -D warnings`/`python3
ci/guards.py`/`cargo deny check`/`bash ci/native-ci.sh` all clean.

## What this doesn't do yet

* No `--filter status=paused` (or any `--filter` at all) on either
  `ocirun list` or `ociman ps` — pre-existing scope gap, unrelated to
  this increment.
* `ocirun ps`/`ociman top` (listing the real *processes inside* a
  container, not the container's own top-level status) are entirely
  unaffected by this change and still show a frozen container's
  processes exactly as before — real `runc`/`podman` behave the same
  way (a frozen process is still a real, listed process, merely not
  scheduled).
