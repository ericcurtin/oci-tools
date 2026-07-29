# Design note 0312: `ociman kill --all`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_kill.rs`.

## Closing part of `0311`'s own "still ahead"

`0311` named `ociman kill`/`stop`/`restart --all`/`--cidfile`/`--ignore`
as real, separately-scoped candidates, noting all three were still
single-`<ID>` commands. This note closes the simplest slice of that:
`ociman kill --all` only. `kill` needs no signal-resolution complexity
(`stop`'s own graceful-then-escalate policy) and no `--ignore` at all
(real `kill` has none — see below) to extend, unlike `stop`/`restart`,
which remain future candidates.

## Real, checked-directly semantics — not assumed from `--help` text alone

Read `~/git/podman/cmd/podman/containers/kill.go` and `pkg/domain/
infra/abi/containers.go`'s own `ContainerKill`/`getContainers`
directly, and `libpod/container_api.go`'s own `Kill`, then verified
against a real installed podman binary:

- `ContainerKill` calls `getContainers(all: true)` — it always lists
  *every* container, running or not, rather than a running-only query.
- Per container, it calls `con.Kill(sig)`; if `--all` is set **and**
  the specific returned error is `define.ErrCtrStateInvalid` (not in a
  killable state), it silently `continue`s — no error, nothing printed
  for that one. Any other kind of failure still surfaces as a real,
  reported error, and every other container is still attempted
  regardless.
- `Container.Kill` itself (`libpod/container_api.go:345`) only allows
  `Running`, `Stopping` (this project has no separate `Stopping`
  state), or **`Paused`** — every other state (`Creating`/`Created`/
  `Stopped`, i.e. "exited"/"never started") is rejected with
  `ErrCtrStateInvalid`. Verified live: a `create`d-but-never-`start`ed
  container is silently left alone by `podman kill --all` (exit `0`,
  untouched, still `Created` afterward) — not just an already-exited
  one.
- Real `docker kill` has **no** `--all` flag at all — this is a
  podman-only extension.
- No `--ignore` flag exists for real `kill` at all (unlike `rm`/`rmi`)
  — `kill`'s `--all` is the entire story; there is nothing else to
  extend here.
- An explicit, single named non-running container (no `--all`) is
  still a hard error, completely unchanged.

## Implementation

`Command::Kill`'s `id: String` became `id: Option<String>`, plus a new
`all: bool` (`--all`/`-a`). `cmd_kill` now:

- Refuses both an explicit id and `--all` together
  (`anyhow::ensure!(id.is_none() || !all, ...)`).
- Parses the signal once upfront regardless of which branch runs.
- Under `--all`: iterates every container via `containers.list()`,
  skipping (silent `continue`) any whose `effective_status()` is
  neither `Running` nor `Paused` — matching real podman's own
  `Kill`'s exact allowed-state set (`Creating`/`Created`/`Stopped` are
  all skipped, not just `Stopped`). Every other container is signaled
  regardless of an earlier failure; the first real failure (if any) is
  returned as an error after every container has been attempted,
  matching real podman's own "attempt every one, still report a
  failure" behavior.
- Without `--all`: entirely unchanged from before this note — an id is
  required, a `Stopped` target is still a hard error, everything else
  about the single-target path is untouched.

## Verified

Manual, end-to-end, cross-checked directly against a real installed
podman: a mix of two running containers, one `create`d-but-never-
`start`ed container — `ociman kill --all` kills exactly the two
running ones (printed), silently leaves the never-started one alone
(still `created` afterward, exit `0`); a second `kill --all` with
nothing running left succeeds silently (exit `0`, no output); `ociman
kill --all some-id` (both given) is a clear, immediate "cannot give
both a container ID/name and --all" error; single-target `ociman kill`
behavior (still errors on a non-running target, still works on a
running one) is unchanged. Cross-checked identical container-state
scenarios directly against a real installed `podman kill --all` for
matching observed behavior (kills running, silently skips both a
never-started and an already-exited container, exit `0`).

Integration (`tests/tests/ociman_kill.rs`, 3 new tests, 7 total, 4
pre-existing): `--all` kills every running container and silently
skips a never-started one, leaving it untouched; `--all` combined with
an explicit id is a clear error; `--all` with no containers at all is
a successful no-op (not an error).

Regression: all 7 `ociman_kill.rs` tests pass (4 pre-existing + 3 new);
full `cargo test --workspace --locked`, 0 failures (two known,
pre-existing `ocicri_container.rs` flakes under full parallel load hit
on the first run this turn — `reopen_container_log_rotates_to_a_fresh_
file`/`create_container_bind_mounts_an_already_existing_single_file`,
neither touched by this change — both re-verified passing in isolation
and the full suite re-run clean).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).

Performance: `ociman kill` is a one-shot, offline command, not part of
any hot-path benchmark tracked in `docs/benchmarks.md` — the common,
single-target, no-`--all`-given case is entirely unchanged. No
re-benchmark needed.

## A discovered, pre-existing, out-of-scope gap

While manually verifying `--all` against a *paused* container (a real,
legitimate "attemptable" state per real podman's own `Kill`, included
above), a genuine, pre-existing correctness gap surfaced: sending
`SIGKILL` to a container whose cgroup is currently frozen reports
success (the signal is genuinely delivered to the kernel) but the
process never actually dies — a signal sent to a task in a *frozen*
cgroup is queued, not delivered, until the cgroup thaws (confirmed
directly against real runc's own `libcontainer/container_linux.go`'s
`signalInit`: "For cgroup v1, killing a process in a frozen cgroup
does nothing until it's thawed. Only thaw the cgroup for SIGKILL" — it
explicitly checks `isPaused()` and thaws after sending `SIGKILL`).
Real podman itself handles this correctly (verified live: `podman kill
--all` on a paused container genuinely reports it `Exited (137)`
afterward).

This project's `oci_runtime_core::process::kill` has no equivalent
auto-thaw-on-`SIGKILL` step, and this is **not** something this note
introduces: it reproduces identically via the pre-existing, entirely
unchanged single-target path (`ociman kill <paused-id>` without
`--all` at all, predating this session) — confirmed live before making
any change here. Fixing it properly touches both `ocirun`'s and
`ociman`'s single-target kill paths (and `oci_runtime_core::process`
or `cgroups` plumbing), well beyond this note's own `--all`-only scope.
Left as a real, separately-scoped future candidate — this note's own
`--all` correctly *attempts* a paused container exactly like real
podman does (matching the "which containers get attempted at all"
question this note is actually about); *whether that attempt reliably
succeeds* against a frozen cgroup is the separate, pre-existing gap
above.

## Still ahead

`ociman stop`/`restart --all`/`--cidfile`/`--ignore` remain real,
separately-scoped candidates — both still single-`<ID>` commands
today. The paused-container `SIGKILL`-delivery gap noted above is also
a real, separately-scoped candidate of its own.
