# Design note 0143: `ociman pause`/`ociman unpause`

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Pause`/`Command::Unpause`;
new shared `resolve_running_container_cgroup` helper, also now used by
the pre-existing `cmd_top`); `tests/tests/ociman_pause.rs` (new, 3
tests).

## Closing the gap 0142 explicitly flagged

0142's own "what this doesn't do yet" named this directly: "`ociman`'s
own `run`/`ps`/`stop`/etc. don't gain a `pause`/`unpause` subcommand of
their own yet (real `podman pause`/`podman unpause` exist)." Picked
back up here — this is the `ociman`-level (systemd-cgroup-driver)
counterpart to `ocirun pause`/`resume`'s own `cgroupsPath`-driven
version.

## A different resolution mechanism than `ocirun`'s own, reusing what `ociman top` already established

`ociman`'s own containers never have `cgroupsPath` set at all (they
use the systemd cgroup driver instead, whose real cgroup path is only
known at container-creation time and never persisted — see 0142's own
finding that `ocirun`'s own `resolve_cgroup_dir` can't be reused here).
`ociman top` already had exactly the right resolution mechanism for
this: `cgroup_dir_for_running_pid`, which re-derives a running
container's own *current* cgroup directly from `/proc/<pid>/cgroup`,
correctly regardless of which driver placed it there. That logic
(resolve container id → load state → require `Running` → get pid →
resolve cgroup) was factored out of `cmd_top`'s own body into a new,
shared `resolve_running_container_cgroup`, now used by `cmd_top`/
`cmd_pause`/`cmd_unpause` alike — the same "one implementation per
function" refactor 0142 already did for `ocirun`'s own three cgroup
commands, applied here to `ociman`'s equivalent three.

## Checked directly against real `podman pause`/`podman unpause` — one real, deliberate divergence from `ocirun`'s own

Real `podman pause`/`unpause`'s own exact behavior was confirmed
directly (a real `docker run -d`/`podman run -d` container, paused and
unpaused for real): both print the given container name/id back on
success (matched exactly here); `pause` on a merely-`created` (not yet
running) container is a real, clear error ("... is not running, can't
pause ..."). This is **stricter** than `ocirun pause`'s own real-runc-
matched behavior (which permits `Created` too, per 0142) — `ociman
pause` instead requires `Running` outright, matching real `podman`'s
own stricter check exactly rather than `ocirun`'s more permissive
lower-level one, since `ociman` mirrors `podman`'s own CLI while
`ocirun` mirrors `runc`'s. `unpause` requires a formally `Paused`
status in real podman; this project has no separate `Paused` status
of its own yet (same gap 0142 already recorded for `ocirun resume`),
so `cmd_unpause` instead also requires `Running` — thawing an already-
thawed cgroup is a harmless, idempotent no-op at the kernel level
regardless, so this narrowing has no real behavioral cost for the
common case.

## Real, end-to-end manual verification before writing any test

Before writing the automated test, manually verified the full round
trip against a real `ociman run -d` container running a genuine CPU-
burning busy loop: found its own real cgroup via `/proc/<pid>/cgroup`
(the same path `ociman top` already showed), paused it, and confirmed
`cpu.stat`'s own `usage_usec` counter stayed *exactly* flat
(`22963105` → `22963105`, bit-for-bit, across a full second) while
frozen; unpaused, and confirmed it jumped substantially
(`23466612`, roughly half a second's worth of CPU time) within another
half second — the real kernel-level effect, not merely a successful
exit code.

## Real, automated tests

Three new CLI-level integration tests in `tests/tests/
ociman_pause.rs`, following `ociman_top.rs`'s own already-established
pattern exactly (no `systemd-run --user --scope` carrier needed at
all — `ociman run` always attempts the systemd cgroup driver itself,
only a reachable `systemd --user` session is required, confirmed by
the same `systemd_user_session_available` probe `ociman_top.rs`
already uses): the same real CPU-freeze/-thaw round trip verified
manually above, now automated; `pause`/`unpause` against an already-
`Stopped` container being clear errors; and against an unknown
container id. All pre-existing tests (including `ociman_top.rs`'s own,
now running through the refactored shared helper) still pass
unmodified. Full `cargo build --workspace --locked`/`cargo test
--workspace --locked` (2 clean runs)/`cargo fmt --all --check`/`cargo
clippy --workspace --all-targets --locked -- -D warnings`/`python3
ci/guards.py`/`cargo deny check` all clean.

## What this doesn't do yet

* No separate, derived `Paused` status in `ociman ps`/`ociman
  inspect`'s own output — same gap 0142 already recorded for `ocirun
  state`/`list`, not yet closed on either binary.
* No `--all`/`--latest`/`--cidfile`/`--filter`/multiple-container
  support — real `podman pause`/`unpause` accept several containers at
  once and a handful of selection flags; this increment matches this
  project's own already-established single-container-per-invocation
  scope every other `ociman` lifecycle command (`kill`/`wait`/
  `rename`/...) already has, not a new narrowing specific to this
  increment.
