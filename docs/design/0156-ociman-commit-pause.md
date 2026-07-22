# Design note 0156: `ociman commit --pause`

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Commit`'s new `pause` field;
`cmd_commit` split into itself — now just a pause/unpause bracket
around the actual work — plus a new private `commit_inner` holding
0155's own original logic unchanged); `tests/tests/ociman_commit.rs`
(2 new integration tests).

## Closing 0155's own deferred gap

0155's own "what this doesn't do yet" named this directly: real
`podman commit --pause`'s own default behavior (freeze the container
via its own cgroup freezer for the duration of the diff/commit, for a
consistent snapshot of one that's still actively writing) wasn't wired
in yet, even though this project already has real cgroup-v2-freezer
pause/resume support (0142/0143) that made this a narrow, low-risk
follow-up rather than new low-level work.

## Matches real podman's own default and its own real "only if actually running" condition

Checked directly, `~/git/podman/libpod/container_commit.go`: pausing
only takes effect if the container is currently running (or
"stopping", a status this project doesn't model), and only if
`options.Pause` is true (the real, checked-directly default). An
already-stopped container has no live process left to race against, so
pausing one is silently skipped — not an error — matching real
podman's own identical condition exactly. `cmd_commit` computes this
once, up front (`pause && state.effective_status() == Status::
Running`), then always attempts to unpause afterward if it paused,
regardless of whether the actual commit work succeeded or failed
in between — the same "always run the cleanup, never let it mask the
real result" shape `stop_container`'s own `wait_for_keeper_to_finalize`
(0154) and `remove_container`'s own force-kill path already established
elsewhere in this same file, not a new pattern.

## The CLI flag itself: matching an already-established exact pattern for a bool defaulting to `true`

`--pause`/`--pause=false` needs the exact same `clap` attribute
combination `Command::Pull`'s own `--tls-verify` already uses for the
identical shape (a boolean flag whose real default is `true`, but that
must still accept an explicit `--flag=false` override, not just bare
presence/absence): `default_value_t = true, num_args = 0..=1,
default_missing_value = "true", action = clap::ArgAction::Set`. Reused
verbatim rather than reinvented.

## No new low-level freezer logic at all

The freeze/thaw itself is the exact same `oci_runtime_core::cgroups::
set_frozen`/`resolve_running_container_cgroup` pair `cmd_pause`/
`cmd_unpause` already use — this increment only adds the bracket
around `cmd_commit`'s own existing (0155) diff/layer-commit logic, not
any new cgroup-freezer code.

## Real, automated tests: the actual kernel-level effect, not just "the CLI call succeeded"

Two new integration tests in `tests/tests/ociman_commit.rs`, following
the same real, direct-cgroup-file verification technique
`ociman_pause.rs`'s own tests already established (reading
`cgroup.freeze` straight from `/sys/fs/cgroup`, independent of
`ociman`'s own implementation), extended with a genuinely new technique
this increment needed: since `ociman commit` runs synchronously and
briefly, `commit_pauses_a_running_container_and_unpauses_it_afterward`
spawns it as a background child (rather than blocking on `.output()`)
so the test's own main thread can busy-poll `cgroup.freeze` *while it's
still running*, to actually catch the real, transient frozen window —
proving the freeze genuinely happened, not just that the container
looks fine again afterward (which a bug that skipped pausing entirely
would also produce). `commit_with_pause_false_never_freezes_a_running_
container` uses the identical busy-poll technique to instead prove the
freeze never engages at all when explicitly disabled. Both verified
stable across repeated runs.

Full `cargo build --workspace --locked`/`cargo test --workspace
--locked` (2 clean runs)/`cargo fmt --all --check`/`cargo clippy
--workspace --all-targets --locked -- -D warnings`/`python3
ci/guards.py`/`cargo deny check`/`bash ci/native-ci.sh` all clean.

## What this doesn't do yet

Everything else 0155 already named remains out of scope: `--change`,
`--config`, `--squash`, `--include-volumes`, and `image` as an optional
(untagged) argument.
