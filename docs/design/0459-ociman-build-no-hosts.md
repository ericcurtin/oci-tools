# Design note 0459: `ociman build --no-hosts`

Status: implemented
Scope: `bin/ociman/src/build.rs`, `bin/ociman/src/main.rs`,
`tests/tests/ociman_build.rs`.

## What this closes

`ociman build` had no `--no-hosts` flag at all — real `podman build
--no-hosts`'s way of skipping the real, synthesized `/etc/hosts`
every `RUN` step otherwise gets by default (localhost entries plus
any `--add-host` entries — this project's own already-existing
default, matching real buildah's own identical default), leaving
whatever `/etc/hosts` the base image's own rootfs already has (or
none at all) completely untouched instead. With the resource-limit
cluster (`0453`-`0458`) now complete, this starts on buildah's own
remaining non-resource `CommonBuildOptions` booleans.

## Real, checked-directly confirmation

`~/git/podman/vendor/go.podman.io/buildah/pkg/cli/common.go:93,289`:
`NoHosts bool`/`fs.BoolVar(&flags.NoHosts, "no-hosts", false, "do not
create new /etc/hosts file for RUN instructions, use the one from
the base image.")` — one build-wide value, default `false` (i.e. the
synthesized `/etc/hosts` is the default, matching this project's own
already-existing default exactly). `~/git/podman/vendor/go.podman.io/
buildah/run_linux.go:401`: `if !options.NoHosts && ... { hostsFile,
err = b.createHostsFile(...) }` — the exact gate this closes, wrapped
around the same real, transient, never-committed `/etc/hosts` write
this project's own `RUN` steps already perform (`0148`). No explicit
validation exists anywhere in buildah's own source for the
`--no-hosts`+`--add-host` combination either — confirmed directly,
not assumed — so giving both silently makes `--add-host` a no-op
rather than erroring, ported here exactly the same way.

## Implementation

- `build_stage` (the per-stage function `write_etc_hosts` actually
  lives in, distinct from `StageContext`, which only carries values
  every `RUN`-step-threading function needs — `write_etc_hosts` is
  called once per stage, before any instruction, not per `RUN` step)
  gains a new `no_hosts: bool` parameter, inserted right after
  `add_host` in both its signature and its one call site in
  `cmd_build`.
- The existing `oci_runtime_core::etc_hosts::write_etc_hosts` call is
  now wrapped in `if !no_hosts { ... }` — the entire write is skipped
  outright rather than writing an empty/partial one, matching real
  buildah's own "use the one from the base image" wording literally.
- `Command::Build` gains `no_hosts: bool` (`--no-hosts`), inserted
  after `cpu_shares`, before `quiet`.

No changes needed to `StageContext`, `run_instruction`, or `run_step_
spec` at all — unlike every flag in the `0453`-`0458` series, this one
never reaches a `RUN` step's own process spec construction; it only
ever affects whether a file gets written to the stage's own rootfs
*before* any `RUN` step runs, the same shape `--add-host` itself
already has.

## Tests

One new test in `tests/tests/ociman_build.rs`:
`build_no_hosts_leaves_the_base_images_own_etc_hosts_completely_
untouched` — seeds a base image with a real, distinctive `/etc/hosts`
of its own (`seed_image_with_files`'s own `extra_files` parameter,
not this project's own synthesized default), builds with both
`--no-hosts` and `--add-host` given together, and confirms a `RUN`
step captures the base image's own `/etc/hosts` byte-for-byte,
proving both that the synthesized default was skipped *and* that
`--add-host`'s entry never appears — the same
capture-then-follow-up-`run` pattern `build_add_host_flag_is_visible_
during_run_steps` (this flag's own already-existing "default" test)
already established. All 135 prior tests in the file pass unmodified
(136/136 total).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (120 test-result
blocks, 0 failures, clean on the first full run), `python3 ci/
guards.py`, `cargo deny check`, `bash ci/native-ci.sh` (clean, 120/120,
clean on the first run too), `bash ci/build-deb.sh` (real `dpkg -i`/
`--version`/`dpkg -r` round trip). No benchmark re-run needed: the
no-flag default path is a plain `if !no_hosts` around an
already-existing, already-benchmarked write, with `no_hosts`
defaulting to `false` — behavior and cost for every build not using
this new flag are provably unchanged.

## Deliberately still out of scope

`--no-hostname`/`--omit-history` remain buildah's own two other
non-resource `CommonBuildOptions` booleans. `--no-hostname` in
particular is *not* the same simple "wrap an existing write" shape
this one turned out to be: real buildah's own default (`!options.
NoHostname`) actually *writes a fresh `/etc/hostname`* for every `RUN`
step, something `ociman build`'s own `RUN` steps have never done at
all (a real, previously-unnoticed, separately-scoped gap found while
researching this increment — this project's build currently behaves
like `--no-hostname` was *always* given, for every build, whether or
not the flag exists). Implementing `--no-hostname` correctly would
mean implementing the *default* real-`/etc/hostname`-synthesis
behavior first, then the flag to disable it — deliberately deferred
to its own future increment rather than folded into this one.
`ociman build --volume`/`--secret` (BuildKit-/buildah-style mounts)
also remain a larger, differently-shaped gap.
