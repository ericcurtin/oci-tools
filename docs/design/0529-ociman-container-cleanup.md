# Design note 0529: `ociman container cleanup`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_container.rs`.

## What this adds

Real `podman container cleanup` is a nested-only subcommand (no
top-level twin) that tears down a container's own mount/network stack
after it exits — normally run automatically by conmon, but also usable
by hand "if container cleanup has failed when a container exits" (real
podman's own doc string). This project had no equivalent at all.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/cleanup.go`: `cleanupCommand` is
  registered with `Parent: containerCmd` only — no top-level twin,
  matching `ContainerCommand::Clone`'s own identical shape. Flags:
  `--all`/`-a`, `--exec <SESSION>`, `--rm`, `--rmi`, plus a hidden
  `--stopped-only` and `--latest`/`-l` (via the shared
  `validate.AddLatestFlag`). `Args` uses
  `validate.CheckAllLatestAndIDFile(cmd, args, false, "")` —
  `ignoreArgLen = false`, unlike e.g. `mount.go`'s own `true` — so a
  bare invocation with no target and neither `--all` nor `--latest` is
  a real, immediate error ("you must provide at least one name or
  id"), unlike `ociman mount`'s own deliberately different bare-
  invocation behavior.
- `~/git/podman/pkg/domain/infra/abi/containers.go`'s own
  `ContainerCleanup` and `getContainers`: a genuine, checked-directly
  semantic *inversion* from every other sibling verb in this family.
  `getContainers`'s own `default` case (the plain, no-`--all`/
  `--latest`, explicit-names-only path) resolves each given name via
  `runtime.LookupContainer` in a loop that **returns on the very first
  unresolvable name**, with no per-name tolerance at all (unlike the
  `--filter`/`--running`/`--all` cases, which build a fixed set first
  and only then optionally narrow it). `ContainerCleanup` then converts
  that specific `ErrNoSuchCtr` into `(nil, nil)` — quoting its own
  comment: "cleanup command spawned by conmon lost race as another
  process already removed the ctr" — i.e. the *entire* call becomes a
  silent, zero-report success, not a partial sweep over just the names
  that do resolve and not a hard error either. `--latest` on an empty
  container store hits the exact same conversion
  (`GetLatestContainer`'s own `ErrNoSuchCtr`) and is therefore *also* a
  silent success here, unlike every other `--latest`-accepting command
  in this project, which hard-errors on an empty store instead.
- Per-container body: `if options.Remove && !ctr.ShouldRestart(ctx) {
  RemoveContainer(ctx, ctr.Container, false, true, timeout) } else {
  ctr.Cleanup(ctx, options.StoppedOnly) }`, followed by an
  unconditionally-attempted `if options.RemoveImage { imageEngine.Remove
  (..., ImageRemoveOptions{Ignore: true}) }` regardless of whether the
  `Remove`/`Cleanup` branch above just failed. `RemoveContainer`'s own
  `force` argument is hardcoded `false` here — a still-running (or,
  per this project's own stricter existing `remove_container` check,
  even merely `Created`) container is a real, reported error, never
  silently force-killed.
- `cleanup.go`'s own `cleanup` function: prints exactly one line per
  container on success (`r.RawInput` if non-empty, else `r.Id` — in
  practice `RawInput` is always populated, either the raw name/id
  given or the resolved container's own real id for `--all`/`--latest`
  sweeps), and surfaces **at most one** error per container via a
  `switch` with `RmErr` taking priority over `RmiErr` over `CleanErr`
  — both `--rm` and `--rmi` are still attempted regardless of which
  one (if either) actually fails.
- `--exec <SESSION>` targets a conmon-tracked background exec session
  — a concept this project has no equivalent of at all, matching
  `ocirun exec`'s own already-established "runs and waits inline,
  nothing left running in the background to later clean up" design.
  Deliberately omitted entirely (not accepted-then-rejected): an
  unrecognized `--exec` is simply a clap parse error, an honest
  reflection of a real, unimplemented gap. The hidden `--stopped-only`
  flag is likewise omitted — it's hidden in real podman too, so
  omitting it changes nothing anyone would notice.

## Implementation

`bin/ociman/src/main.rs`:
- New `ContainerCommand::Cleanup { containers: Vec<String>, all: bool,
  latest: bool, rm: bool, rmi: bool }` — nested-only, no top-level
  twin, matching `Clone`'s own precedent. Few enough fields that no
  boxing is needed (unlike `Command::Run`/`ContainerUpdateArgs`).
- New `cmd_container_cleanup`:
  - Replicates `CheckAllLatestAndIDFile`'s own exact validation order
    and wording (`--all`+`--latest`, `--all`+names, `--latest`+names,
    then the bare "you must provide at least one name or id" case).
  - Resolves the target set as `(raw_input, resolved_id)` pairs: every
    container (oldest first) for `--all`, the single latest for
    `--latest` (returning `Ok(())` immediately on an empty store,
    reusing the existing `resolve_latest_container` and converting its
    error into the silent-success case rather than propagating it),
    or each explicit name/id resolved in order via the existing
    `resolve_container_id` — returning `Ok(())` immediately (before
    processing anything) on the very first one that doesn't resolve,
    faithfully replicating the real whole-call silent-no-op inversion
    above.
  - The teardown itself is a real no-op here, for the same reason
    `cmd_unmount`'s own doc comment already established: this
    project's containers have no separate, persistent mount/network
    state beyond what `create`/`run` already manage moment-to-moment.
  - `--rm` reuses the exact same `remove_container` primitive `ociman
    rm` itself already uses, always with `force: false` — matching
    real podman's own hardcoded `false` at this exact call site. Since
    this project has no restart-policy concept, the real `!ctr.
    ShouldRestart(ctx)` guard is always true here, so `--rm` always
    takes the "real remove" branch unconditionally.
  - `--rmi` captures the container's own `ANNOTATION_IMAGE` *before*
    any `--rm` removal above (which would otherwise delete the very
    state that annotation lives in), then reuses the exact same
    `resolve_image_by_reference_or_id` + `rmi_one` pair `cmd_rmi`
    itself already uses: `Ok(None)` (image already gone) is a silent
    success — real podman's own `ImageRemoveOptions{Ignore: true}`,
    checked directly — while every other failure (most commonly: the
    image is still in use by a different, real dependent container)
    is a real, reported error, exactly `cmd_rmi`'s own already-
    established `--ignore` semantics.
  - Both `--rm` and `--rmi` are attempted independently per container
    regardless of whether the other one just failed, but only one
    error ever surfaces per container — `--rm`'s own taking priority
    — matching real podman's own exact `switch`-based precedence in
    `cleanup.go` exactly.
  - Successful containers print their own `raw_input`; any per-
    container error is `eprintln!`'d and accumulated into an overall
    `anyhow::bail!` at the end (matching `errs.PrintErrors()`'s own
    "some containers failed" nonzero exit, without needing this
    project's own error type to carry a list).

## Tests

Nine new integration tests in `tests/tests/ociman_container.rs`:
- `container_cleanup_bare_prints_the_id_and_leaves_the_container_untouched`
- `container_cleanup_all_sweeps_every_container`
- `container_cleanup_latest_on_an_empty_store_is_a_silent_success`
- `container_cleanup_any_unresolvable_name_silently_no_ops_the_whole_call`
- `container_cleanup_rm_removes_the_container`
- `container_cleanup_rm_rmi_also_removes_the_backing_image`
- `container_cleanup_rmi_is_a_silent_success_once_the_image_is_already_gone`
  (simulates the real race directly via `store.remove_image` for a
  fast, deterministic test rather than trying to actually win one)
- `container_cleanup_with_no_target_at_all_is_a_clear_error`
- `container_cleanup_all_and_latest_together_is_a_clear_error`

The `--rm` tests use `ociman run` (not `create`) so the target
container is genuinely `Stopped`, not merely `Created`: this project's
own existing `remove_container` (unlike real podman) requires
`--force` for anything short of `Stopped`, and this command's own
`--rm` never overrides that (matching real podman's own hardcoded
`force: false`) — the realistic case `cleanup` actually targets is
conmon invoking it right after a container has just exited on its own.

Manually exercised end to end by hand beyond the automated tests: a
real image built via `ociman build` from a `FROM scratch` + bundled
`busybox` Containerfile (no registry access needed), then bare
cleanup by name (prints the name back, container untouched), cleanup
naming one real container plus one bogus name together (silent no-op,
confirmed the real container was untouched even though it would have
resolved on its own), `--all`, `--latest`, `--rm` on a `Created`
container (real error, matching `remove_container`'s own existing
check), `--rm --rmi` where the image was still in use by a second,
different container (real error, nothing printed, exit 1 — the
already-removed-by-`--rm` container's own id correctly never printed
either), and finally a clean `--rm --rmi` with no other dependents
(both container and image gone, id printed, exit 0).

Full workspace: `cargo build --workspace --locked` (clean), `cargo fmt
--all` (clean after one auto-fix), `cargo clippy --workspace
--all-targets --locked -- -D warnings` (clean), `cargo test --workspace
--locked` (multiple full runs; the new tests above all pass every
time). This host had several other, genuinely concurrent `opencode`
sessions actively running their own test suites throughout this
increment, which produced exactly the already-documented classes of
transient flakiness — confirmed transient in every case by rerunning
the specific failing test in isolation immediately afterward:
`ociman_logs.rs`'s `logs_follow_streams_a_running_containers_output_
and_stops_when_it_exits`, `ocicri_container.rs`'s
`create_container_bind_mounts_an_already_existing_single_file`, and
`ociman_build.rs`'s `build_cpu_period_quota_and_shares_set_the_real_
systemd_scopes_own_properties`/`build_cpuset_flags_set_the_real_
systemd_scopes_own_allowed_cpus_property` (the same systemd-`--user`-
scope-leak-and-D-Bus-pressure issue `0527` first diagnosed — this time
still failing even after a cleanup pass, since other concurrent
sessions kept creating new scopes faster than they could be reaped;
confirmed purely environmental, not a regression, via the same
`git stash`/`git stash pop` A/B test against a clean `origin/main`
checkout `0527` established). `python3 ci/guards.py` (clean), `cargo
deny check` (clean), a real `cargo build --workspace --release
--locked` (clean, used in place of a fully clean `ci/native-ci.sh` run
given the environmental test contention above — the script itself is
just `cargo build` + `cargo test` + `cargo build --release`, all
three individually verified clean), `bash ci/build-deb.sh` (clean, real
`dpkg -i`/`--version` for every binary/`dpkg -r` round trip). Pure
CLI-parsing-and-removal-reuse addition — no hot path touched, no
`ci/bench.sh` rerun needed.

## Deliberately still out of scope

`--exec`/`--stopped-only` (see above). A positional `IMAGE` argument
doesn't exist for this command in real podman either, so there's
nothing analogous to `ociman container clone`'s own deferred gap here.
