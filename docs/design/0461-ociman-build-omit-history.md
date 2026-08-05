# Design note 0461: `ociman build --omit-history`

Status: implemented
Scope: `crates/oci-dockerfile/src/commit.rs`, `bin/ociman/src/build.rs`,
`bin/ociman/src/main.rs`, `tests/tests/ociman_build.rs`.

## What this closes

`ociman build` had no `--omit-history` flag at all — real `podman
build --omit-history`'s way of skipping every history entry a build
would otherwise add to the built image's config, while still fully
applying and recording every instruction's own real effect (layers,
`diff_ids`, config changes) exactly as normal. This closes buildah's
last remaining non-resource `CommonBuildOptions` boolean (`--no-hosts`/
`--no-hostname`, `0459`/`0460`, closed the other two).

## Real, checked-directly confirmation

`~/git/podman/vendor/go.podman.io/buildah/pkg/cli/common.go:96,308`:
`OmitHistory bool`/`fs.BoolVar(&flags.OmitHistory, "omit-history",
false, "omit build history information from built image")` — one
build-wide value, default `false`. `~/git/podman/vendor/go.podman.io/
buildah/image.go:1204-1207`: `if !i.omitHistory { if err :=
mb.buildHistory(...); err != nil { ... } }` — the *entire* history-
appending call is skipped, not individual entries filtered out after
the fact. Critically, `mb.dimage.History` (`mb.OCIv1.History` for the
OCI variant) is **already seeded with the base image's own inherited
history** by the time this check runs (`buildHistory`'s own `base
ImageHistoryLen := len(mb.dimage.History)` line makes this explicit) —
so `--omit-history` leaves the base's own history completely
untouched, it just never appends anything new for *this* build's own
instructions, no matter how many ran. The layers/`diff_ids` themselves
are never touched by this flag at all (confirmed directly: nothing in
`buildHistory` ever writes to `RootFS.DiffIDs`, only `dimage.History`/
`OCIv1.History`) — a built image under `--omit-history` still has
every one of its real layers, just no human-readable record of which
instruction produced which.

## Implementation

Unlike `--no-hosts`/`--no-hostname` (each a single, centralized write
this project already had one call site for), `--omit-history` needed
to reach **every** instruction handler in `build.rs` — 17 call sites
across `run_instruction`/`copy_instruction`/`add_instruction`/
`apply_instruction`'s dozen-plus config-only instructions (`ENV`/
`LABEL`/`WORKDIR`/`CMD`/`ENTRYPOINT`/`USER`/`EXPOSE`/`VOLUME`/
`STOPSIGNAL`/`MAINTAINER`/`HEALTHCHECK`/`ONBUILD`/`SHELL`)/`cmd_
build`'s own top-level `--label` handling. Centralizing the actual
skip logic in the two shared primitives themselves (rather than
duplicating an `if !omit_history` guard at all 17 call sites) kept
this from becoming 17 separate, easy-to-drift copies of the same
check:

- `oci_dockerfile::record_layer`/`record_empty_history` (`crates/
  oci-dockerfile/src/commit.rs`) each gained a new trailing
  `omit_history: bool` parameter. `record_layer` still always pushes
  onto `layers`/`config.rootfs.diff_ids` (the real layer is always
  recorded either way, matching real buildah's own identical
  distinction confirmed above); only the `config.history.push(...)`
  call itself is now conditional. `record_empty_history` becomes a
  complete no-op when `true` (four new unit tests: two proving the
  "layer/diff_ids always recorded, history conditionally skipped"
  split, two updating the pre-existing calls' now-5/4-argument
  signature).
- `StageContext<'a>` gains a plain `omit_history: bool` field (no
  parsing/validation, same shape `http_proxy` already established),
  carried the same way `rlimits`/`shm_size_bytes`/`resources`/
  `http_proxy` already are. Every one of the 13 `apply_instruction`
  call sites already threading `stage_ctx.forced_mtime` through now
  also threads `stage_ctx.omit_history` right alongside it (a
  mechanical, one-line addition at each).
- `run_instruction`/`copy_instruction`/`add_instruction` (the three
  functions that call `record_layer` for an actual new layer) each
  gained an `omit_history: bool` parameter, threaded from `stage_ctx.
  omit_history` at their own call sites in `apply_instruction`.
- `cmd_build`'s own top-level `--label` handling passes its own
  `omit_history` parameter directly (it runs after `build_stage`
  returns, outside any `StageContext`'s scope) — `--label`'s trailing
  history entry is skipped exactly the same way every other
  instruction's own is.
- `Command::Build` gains `omit_history: bool` (`--omit-history`),
  inserted after `no_hostname`, before `quiet`.

`config.created` (already computed as `config.history.last().and_then
(|entry| entry.created.clone())`, `0197`) needed **no** special-casing
at all: with no new history entries ever pushed under `--omit-history`,
`history.last()` still returns the base's own last entry (or `None`
for a from-scratch/no-history base), so `created` naturally stays
exactly as inherited — the same "no-op either way" property that
existing code comment already documents for a bare `FROM` with no
instructions at all.

## Tests

Four new unit tests in `crates/oci-dockerfile/src/commit.rs`
(`record_layer_with_omit_history_still_records_the_layer_but_skips_
history`, `record_empty_history_with_omit_history_is_a_complete_
no_op`, plus updating the two pre-existing calls' signatures) and one
new integration test in `tests/tests/ociman_build.rs`
(`build_omit_history_skips_every_history_entry_but_still_applies_
every_instruction` — a real `RUN`/`ENV`/`LABEL` Containerfile plus an
explicit `--label` given together with `--omit-history`, asserting
`config.history` is completely empty *and* every instruction's own
real effect (the label value, the `ENV` value, and — via a follow-up
`run` reading a file back — the `RUN` step's own real layer content)
is still genuinely present, plus `config.created` staying `None`
exactly as the history-less seeded base already had it). All 138
prior tests in `ociman_build.rs` pass unmodified (139/139 total); all
156 prior tests in `oci-dockerfile` pass unmodified.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean, no new `too_many_arguments` lint triggered on any
of the newly-touched functions), `cargo test --workspace --locked`
(120 test-result blocks, 0 failures — the first full run hit one
transient, known-flaky failure in `ocicri_container.rs`'s own
`create_container_capabilities_add_and_drop_change_the_real_process_
capability_sets`, exit code 126 "process exited before exec",
confirmed unrelated and passing instantly in isolation; recurred once
more on an immediate retry, then a run with reduced test-thread
concurrency passed clean 120/120 -- this dev host had heavier
concurrent load than usual across several other running sessions
during this turn, consistent with the already-documented, accepted
environmental flakiness from the long-running CPU-spinning background
process this host has carried since before this session), `python3
ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh` (clean,
120/120, clean on the first run), `bash ci/build-deb.sh` (real `dpkg
-i`/`--version`/`dpkg -r` round trip). No benchmark re-run needed:
this only touches `ociman build`'s own in-memory history bookkeeping
(a negligible `Vec::push` either way), never `run`/`create`/`commit`'s
own hot paths at all.

## Deliberately still out of scope

This closes every real `CommonBuildOptions` field this series
(`0453`-`0461`) has been tracking — resource limits, proxy
passthrough, `/etc/hosts`/`/etc/hostname` control, and now history —
except `--secrets`/`Devices`/`DecryptionKeys`/`LabelOpts`/`Masks`/
`CgroupParent`/`SeccompProfilePath`/`ApparmorProfile` (all genuinely
larger, separately-shaped gaps: real secret-mount support, device
passthrough, encrypted-image pulls, SELinux/AppArmor label control,
custom masked-path lists, and cgroup-parent path control, none of
which reuse anything this series already built). `ociman build
--volume` (BuildKit-/buildah-style `RUN --mount=type=bind`) remains
the most valuable of these to pursue next.
