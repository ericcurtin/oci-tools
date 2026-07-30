# Design note 0319: `kill` actually thaws a genuinely paused container

Status: implemented
Scope: `crates/oci-runtime-core/src/cgroups.rs`, `bin/ociman/src/main.rs`,
`bin/ocirun/src/main.rs`, `tests/tests/ociman_kill.rs`,
`tests/tests/ocirun_lifecycle.rs`.

## Closing the real gap `0312` first found

`0312` discovered, but deliberately deferred, a real correctness gap:
sending a signal to a container whose cgroup is currently frozen
(paused) can be left stuck rather than actually taking effect — so
`kill` on a paused container previously reported success (the signal
genuinely was sent) while the container silently stayed alive and
paused forever. This note closes it, rather than leaving it deferred
any longer.

## Correction (`docs/design/0325`)

This note's own original premise — that a frozen cgroup's freezer
"queues every signal completely identically" (see the next section) —
was later shown to be imprecise for cgroup v2 specifically. The
kernel's own authoritative docs and direct empirical testing both
confirm: a genuinely *fatal* signal (`SIGKILL`, or any signal the
target process hasn't installed a handler for) reaches and terminates
a frozen process immediately, no thaw required at all. What actually
stays queued until thaw is a signal the target process *has* installed
a live handler for (not "fatal" from the kernel's own point of view) —
exactly the case a real container's own graceful-shutdown handler puts
`stop`'s first signal in. **The fix below (thaw after signaling) is
still fully correct and necessary — only the original reasoning was
wrong.** See `0325` for the full corrected account and a permanent
regression test (`cgroups::tests::
cgroup_v2_freezer_lets_a_fatal_signal_through_but_queues_a_handled_one`)
locking in the real distinction.

## Real, checked-directly semantics — neither reference runtime gets this right in general

Read both reference runtimes' own source directly rather than assume:

- Real runc's own `signalInit` (`~/git/runc/libcontainer/
  container_linux.go`): thaws the cgroup, but **only after `SIGKILL`
  specifically** — any other signal sent to a paused container is
  still silently queued forever, exactly like this project's own
  previous behavior.
- Real crun's own `libcrun_kill_linux`/`libcrun_kill_linux_no_pidfd`
  (`~/git/crun/src/libcrun/linux.c`): a plain `kill(2)` syscall, **no
  cgroup-freezer awareness at all**, for any signal — verified by
  reading the entire function; there is no thaw path whatsoever.
- Real `podman kill` on a paused container, though, genuinely works
  end to end — verified live: `Exited (137)` afterward, not a silent
  no-op. (Real podman's own `Container.Kill` calls into the OCI
  runtime it's configured with — `crun` by default on most distros,
  `runc` elsewhere — so which of the two source-level findings above
  actually applies depends on the deployment; either way, the
  *podman-level* user-visible contract is "kill actually works",
  which is the contract this project's own `kill` must honor
  regardless of which lower-level runtime detail happens to explain
  any one particular real installation's own success.)

Given neither individual reference runtime actually gets this right
for every signal, this project's own fix deliberately generalizes:
thaw after sending *any* signal, not just `SIGKILL` (see this note's
own "Correction" above for the precise, cgroup-v2-accurate reason this
still matters: a signal the target process has installed a handler
for genuinely is queued until thaw, unlike a fatal one). This is a
genuine improvement over both real crun and real runc's own individual
implementations, not merely matching one of them.

## Implementation

New `oci_runtime_core::cgroups::kill_thawing_if_paused(cgroup_root,
pid, signal)`: sends `signal` via the existing `process::kill`, then
resolves `pid`'s own real, current cgroup (via the already-existing
`cgroup_dir_for_running_pid`, the same technique `ociman`'s own
`display_status`/`cmd_top` already use) and thaws it if — and only if
— it's genuinely frozen right now. Best-effort at resolving the
cgroup itself (a pid that's already exited between the signal and this
check is simply left alone, matching `display_status`'s own identical
tolerance for that same class of race), but a *found*, genuinely
frozen cgroup that then fails to actually thaw is a real, propagated
error: the entire point of this function succeeding is that the
signal actually took effect.

Both `ociman`'s `kill_one` (shared by the single-target, multi-target,
and `--all` paths — the `--all` branch's own previously-duplicated
inline signal-sending logic was also unified to call `kill_one`
directly rather than diverge from this same fix) and `ocirun`'s
`cmd_kill` (single-target; `--all`'s own separate freeze/sweep/thaw
semantics, 0277, are crun's *own* different meaning for that flag —
signal every process in the cgroup reliably, not "was this container
already paused" — and already thaw unconditionally at the end
regardless, so were never affected by this particular gap) now call
this shared primitive instead of a plain `process::kill`.

## Verified

Manual, end-to-end: `ociman kill <paused-id>` and `ociman kill --all`
(with a paused container in the mix) both now genuinely terminate the
paused target, transitioning it to `stopped` — confirmed directly,
before and after this fix, that the pre-fix behavior really did leave
it silently alive and paused forever despite reporting success.
`ocirun kill <paused-id> KILL` against a real, delegated-cgroup
rootless container (the same `systemd-run --user --scope` carrier
setup `ocirun_lifecycle.rs`'s own existing pause/resume test already
established) verified identically.

Integration: one new test in `tests/tests/ociman_kill.rs`
(`kill_on_a_still_paused_container_actually_terminates_it`, 10 total,
9 pre-existing) and one new test in `tests/tests/ocirun_lifecycle.rs`
(`kill_on_a_still_paused_container_actually_terminates_it`, 14 total,
13 pre-existing) — both pause a real, running container, then `kill`
it *without* first unpausing, and assert it actually reaches
`stopped`, not stuck `paused` forever.

Regression: all `ociman_kill.rs`/`ocirun_lifecycle.rs` tests pass; the
new `oci_runtime_core::cgroups` unit test suite (60 tests) passes
unchanged. Full `cargo test --workspace --locked`: 112 test result
blocks, 0 failures.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).

Performance: `kill` is not part of any hot-path benchmark tracked in
`docs/benchmarks.md` (the earlier `ociman exec`/`run`/`rm`/`commit`/
`build` comparisons don't exercise it). A direct `hyperfine` sanity
check of `ociman kill` against a real, non-paused running container
still measures ~1.5ms mean — consistent with this project's other
already-benchmarked one-shot commands (e.g. `ociman rm`) — confirming
the two extra, best-effort file reads this fix adds (`/proc/<pid>/
cgroup`, `cgroup.freeze`) cost nothing measurable relative to process
startup itself. No re-benchmark needed for the tracked comparisons.

## Still ahead

`ociman stop`/`restart` still hard-refuse a genuinely paused container
outright rather than attempting to thaw-then-signal it the way `kill`
now correctly does (`0313`'s own deliberate choice, matching real
podman's own identical refusal there) — teaching them to actually
succeed against a paused container instead remains a real, separately-
scoped, deliberately deferred future candidate, since `stop`/`restart`
both need a graceful-signal-then-escalate *policy* on top of "the
signal takes effect at all", not just a single delivered signal.
