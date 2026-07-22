# Design note 0142: `ocirun pause`/`ocirun resume`

Status: implemented
Scope: `crates/oci-runtime-core/src/cgroups.rs` (new `set_frozen`/
`is_frozen`/`wait_frozen`); `bin/ocirun/src/main.rs` (`Command::Pause`/
`Command::Resume`, new shared `resolve_cgroup_dir` helper factored out
of `cmd_update`'s own existing body); 6 new unit tests, 2 new
integration tests in `tests/tests/ocirun_lifecycle.rs`.

## Why this, now

Comparing `ocirun --help` against real `crun`/`runc --help` (the same
survey technique 0134-0141 already used for `ociman`'s own flag
surface) found `pause`/`resume` missing entirely from `ocirun`'s own
subcommand list — a real, well-scoped gap: a simple cgroup-freezer
operation, no new dependencies, directly reusable existing
infrastructure (`cgroups.rs`'s own cgroup-v2-only scope, `cmd_update`'s
own already-proven cgroup-directory resolution). `checkpoint`/`restore`
(CRIU-based) and `events` (streaming stats) were also missing but are
each a much larger undertaking, deliberately left for their own future
increments.

## Checked directly against real runc's own current source

The exact real protocol was read directly from `~/git/runc/vendor/
github.com/opencontainers/cgroups/fs2/freezer.go` rather than assumed:
write `"1"` to `cgroup.freeze` to freeze, `"0"` to thaw. Thawing
returns as soon as the write succeeds (real runc's own `readFreezer`
only ever polls when the state it just read back is `"1"`, never
`"0"` — releasing already-frozen tasks doesn't take real time the way
stopping every one of them does); freezing polls `cgroup.events` for a
real `frozen 1` line (the kernel's own authoritative confirmation that
every task has actually stopped, since the `cgroup.freeze` write only
*requests* a freeze asynchronously) for up to real runc's own exact
budget (`10ms` per attempt, `1000` attempts, ~10 seconds total).

Real runc's own `Pause`/`Resume` validation was also read directly
(`~/git/runc/libcontainer/container_linux.go`): `Pause` is allowed for
`Running` or `Created`; `Resume` requires exactly `Paused`. This
project doesn't yet track a separate `Paused` status (see "what this
doesn't do yet" below), so both `cmd_pause`/`cmd_resume` here instead
allow the same `Created`/`Running` states — a deliberate, documented
narrowing rather than a faithfulness gap: writing `"1"`/`"0"` to an
already-frozen/-thawed cgroup is itself a harmless, idempotent no-op at
the kernel level regardless, so there's no real behavioral difference
for the common case, only a difference in exactly which state name a
rejection error message would use.

## A real, genuinely tricky rootless verification, done properly rather than assumed

Manually verifying this needed real care: a raw, unprivileged shell
process cannot migrate itself (or a child) into an arbitrary cgroup
path purely because that path's own `cgroup.procs` is writable — the
kernel also requires write access to the *common ancestor* cgroup
between the process's current cgroup and the target, a real constraint
this session hit directly (a plain `mkdir` + `echo $$ > cgroup.procs`
into a manually-created directory under the delegated `app.slice`
failed with a real `EACCES`, tracked down to `user-<uid>.slice`'s own
`cgroup.procs` — the actual common ancestor — being root-owned, not
delegated). Fixed by launching the *migrating* step
(`ocirun create`, which is the one invocation that actually moves a
pid into the target cgroup) via `systemd-run --user --scope
--slice=app.slice`, making it a sibling of the target under the fully-
delegated `app.slice` instead — the exact same technique `ocirun_run.
rs`'s/`ocirun_lifecycle.rs`'s own pre-existing cgroup tests already
established, confirmed to still apply here.

Once genuinely running, `pause`/`resume`/`kill`/`delete` were confirmed
to need *no* such carrier at all — they only ever read or write a
single already-delegated file directly (no cross-cgroup migration),
governed by ordinary Unix file permissions on a file this uid already
owns. Verified end to end with a real, CPU-burning busy-loop container
and its own real `cpu.stat`'s `usage_usec` counter: paused, the
counter stayed *exactly* flat across a full measured second (confirmed
manually with two samples one second apart, bit-for-bit equal); resumed,
it jumped by hundreds of thousands of microseconds within half a
second — the actual, real kernel-level effect this command exists for,
not merely that the CLI call itself exits `0`.

## A small, low-risk refactor along the way

`cmd_update`'s own cgroup-directory resolution (load state, load
bundle, resolve `cgroupsPath`) was factored into a new, shared
`resolve_cgroup_dir(root, id)` helper, now used by `cmd_update`/
`cmd_pause`/`cmd_resume` alike — matching this project's own "one
implementation per function" pillar rather than three near-identical
copies. The error message's own wording changed slightly as a direct
result (`"container {id:?} has no cgroup to update (...)"` → `"...
has no cgroup (...)"`, since the same message is now shared by
`pause`/`resume` too, which have nothing to "update"); one pre-existing
test (`ocirun_update.rs`'s own `update_without_a_cgroup_is_a_clear_
error`) asserted on the old, narrower wording and was updated to match
— confirmed the new wording is still an equally clear, real error.

## Real, automated tests

Six new unit tests in `oci-runtime-core` (`set_frozen(false)`/thaw
never waits at all; `is_frozen` round-trips a real file's own content;
`wait_frozen`'s own three real shapes — succeeds immediately, keeps
polling until a background writer reports frozen, and times out
clearly if the kernel never confirms it — each using a small,
test-only poll budget rather than really waiting up to real runc's own
ten-second one). Two new CLI-level integration tests in `tests/tests/
ocirun_lifecycle.rs`: the full real end-to-end CPU-freeze/-thaw
round trip described above, and a dedicated test confirming
`pause`/`resume` against an already-`Stopped` container are clear,
real errors (`"cannot pause/resume a container in the stopped
state"`), not silent no-ops. All pre-existing tests (including the
one whose own assertion needed updating) still pass. Full `cargo build
--workspace --locked`/`cargo test --workspace --locked` (2 clean
runs)/`cargo fmt --all --check`/`cargo clippy --workspace
--all-targets --locked -- -D warnings`/`python3 ci/guards.py`/`cargo
deny check` all clean.

## What this doesn't do yet

* No separate, persisted/derived `Paused` status — real runc/crun both
  report `"paused"` via `state`/`list` once a container's own cgroup
  freezer reports frozen (dynamically computed at query time, not a
  separately-persisted field, confirmed directly against real runc's
  own `isPaused()`). Wiring this into `ocirun state`/`ocirun list`
  (and `PersistedState::effective_status`, which currently has no
  cgroup-directory access at all) is a real, separately-scoped future
  increment — `pause`/`resume` are already fully functional at the real
  kernel level without it, just not yet reflected in status output.
* `ociman`'s own `run`/`ps`/`stop`/etc. don't gain a `pause`/`unpause`
  subcommand of their own yet (real `podman pause`/`podman unpause`
  exist) — this increment is `ocirun`-level only, matching how this
  project's own milestone-3 flag/subcommand additions have generally
  started at the runtime level first.
* `checkpoint`/`restore` (CRIU) and `events` (streaming cgroup/OOM
  stats) — both real `runc`/`crun` subcommands still entirely missing,
  each a substantially larger, separately-scoped future increment.
