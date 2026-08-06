# Design note 0530: `ociman init` / `ociman container init`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_init.rs`,
`tests/tests/ociman_container.rs`.

## What this adds

Real `podman init`/`podman container init` — "Initialize one or more
containers, creating the OCI spec and mounts for inspection" (real
podman's own doc string, quoted verbatim) — is a *dual-registered*
subcommand: a top-level `podman init` **and** a nested `podman
container init`, sharing one flag set. Real docker has no equivalent
at all (docker's own `--init` is an unrelated `run`-time flag, already
implemented here). This project had no `init` of any kind.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/init.go`: a top-level
  `initCommand` and a nested `containerInitCommand` (`Parent:
  containerCmd`), sharing one `initFlags`/`RunE`/`Args`/`Example`
  (unlike `ContainerCommand::Cleanup`'s own nested-only registration,
  0529). `Args` uses `validate.CheckAllLatestAndIDFile(cmd, args,
  false, "")` — the exact same call `cleanup` already replicates
  verbatim, so a bare invocation with no target and neither `--all`
  nor `--latest` is a real, immediate error ("you must provide at
  least one name or id"). Flags: `--all`/`-a`, `--latest`/`-l`.
- `~/git/podman/pkg/domain/infra/abi/containers.go:1436-1454`
  (`ContainerInit`):
  ```go
  containers, err := getContainers(ic.Libpod, getContainersOptions{all: options.All, latest: options.Latest, names: namesOrIds})
  if err != nil {
      return nil, err
  }
  reports := make([]*entities.ContainerInitReport, 0, len(containers))
  for _, ctr := range containers {
      report := entities.ContainerInitReport{Id: ctr.ID(), RawInput: ctr.rawInput}
      err := ctr.Init(ctx, ctr.PodID() != "")
      // If we're initializing all containers, ignore invalid state errors
      if options.All && errors.Is(err, define.ErrCtrStateInvalid) {
          err = nil
      }
      report.Err = err
      reports = append(reports, &report)
  }
  return reports, nil
  ```
  Crucially, the top-level `getContainers` error here is propagated
  directly (`return nil, err`) with **no** `ErrNoSuchCtr`-swallowing
  special case — a genuinely *different*, and simpler, policy than
  `ContainerCleanup`'s own deliberate whole-call *silent* success
  inversion (0529): an unresolvable explicit name is a real,
  immediate error here, matching every *other* multi-target command's
  own "resolve everything first" convention (`cmd_mount`'s own
  already-established two-or-more-explicit-targets phase). Per-
  container, the loop never aborts early on one failure — every
  container in the same call is still attempted — but only `--all`
  swallows the specific `ErrCtrStateInvalid` class (still printing
  that container's own id as if successful); outside `--all`, it's a
  real, reported error.
- `~/git/podman/libpod/container_api.go:33-74` (`Init`/
  `initUnlocked`):
  ```go
  func (c *Container) initUnlocked(ctx context.Context, recursive bool) error {
      if !c.ensureState(define.ContainerStateConfigured, define.ContainerStateStopped, define.ContainerStateExited) {
          return fmt.Errorf("container %s has already been created in runtime: %w", c.ID(), define.ErrCtrStateInvalid)
      }
      ...
  }
  ```
  `Configured`/`Stopped`/`Exited` are accepted; `Created`/`Running`/
  `Paused` are rejected with the exact `"container %s has already been
  created in runtime"` wording. This project's own already-established
  two-name state split (`Status::Created`/`Status::Stopped`, see
  `ociman create`'s own doc comment collapsing real podman's
  `Configured`+`Created` into one name) means every container that has
  ever reached `Created` here has *already* had its own real OCI-
  runtime `create` step run eagerly — exactly real podman's own post-
  `Init` `Created` state, never its accepted, pre-`Init` `Configured`
  one. So a `Created` container here always hits the *rejected*
  branch, matching real podman's own identical "already initialized"
  refusal exactly. A `Stopped` one is eligible, but a real, faithful
  no-op: this project's own `start` always does a full, fresh launch
  from the bundle regardless of whether the container was previously
  `Created` or `Stopped` (`cmd_start`'s own doc comment), so there is
  no separate, in-advance "reinitialize the runtime container" step for
  this command to actually perform — the same "the real work is
  already a no-op here" reasoning class `cleanup`'s own teardown
  already established, applied to the setup side instead.

## Implementation

`bin/ociman/src/main.rs`:
- New `Command::Init { containers: Vec<String>, all: bool, latest:
  bool }` and identical `ContainerCommand::Init { .. }` (dual-
  registered, matching real podman's own shape rather than `Clone`'s/
  `Cleanup`'s nested-only one) — both dispatch into the same shared
  `cmd_init`, the same "one function, two enum entry points" shape
  `cmd_stop`/`cmd_kill`/etc. already established.
- New `cmd_init`:
  - Replicates `cmd_container_cleanup`'s own exact
    `CheckAllLatestAndIDFile` validation block verbatim.
  - Resolves targets as `(raw_input, resolved_id)` pairs: `--all`
    lists every container (oldest first); `--latest` reuses the
    existing `resolve_latest_container`, propagating its error
    normally on an empty store (a real, deliberate *non*-replication
    of `cleanup`'s own special silent-success case, since real `init`
    has no analogous "conmon lost the race" reasoning); explicit names
    are resolved one at a time via the existing `resolve_container_id`,
    with `?` propagating the very first failure immediately — the
    same "resolve everything first, abort before touching anything"
    policy `cmd_mount`'s own multi-target path already established,
    deliberately *not* `cleanup`'s own silent-no-op inversion.
  - Per container: `Status::Stopped` → a real, faithful no-op success
    (prints `raw_input`); anything else → real podman's own exact
    `"container {id} has already been created in runtime"` error,
    silently tolerated (still printed as a success) under `--all`,
    otherwise accumulated into an overall `anyhow::bail!` at the end —
    the same per-container error-aggregation pattern `cmd_container_
    cleanup` already established, without needing this project's own
    error type to carry a list.

## Tests

Eight new integration tests in `tests/tests/ociman_init.rs`:
- `init_on_a_created_container_is_a_real_already_created_error`
- `init_on_a_stopped_container_is_a_real_no_op_success`
- `init_on_a_running_container_is_a_real_error`
- `init_all_tolerates_an_ineligible_container_and_still_prints_its_id`
- `init_latest_on_an_empty_store_is_a_real_error`
- `init_with_one_unresolvable_name_aborts_the_whole_call_with_a_real_error`
- `init_with_no_target_at_all_is_a_clear_error`
- `init_all_and_latest_together_is_a_clear_error`

Plus one new alias-proof test in `tests/tests/ociman_container.rs`
(`container_init_is_a_byte_identical_alias_for_top_level_init`),
matching every prior alias's own established "full semantics live in
the top-level command's own test file, `ociman_container.rs` only
proves the alias reaches the identical function" convention (checked
directly against `ociman_mount.rs`/`ociman_container.rs`'s own
existing split for `mount`/`unmount` before writing this).

Manually exercised end to end beyond the automated tests: a real
image built via `ociman build` from a `FROM scratch` + bundled
busybox Containerfile, then `create` + `init` on the resulting
`Created` container (real error, exact wording), `run` (to
completion) + `init` on the resulting `Stopped` one (real no-op
success, id printed), `--all` with one of each (the `Created` one's
error silently tolerated, both ids printed), the `container init`
alias (identical error on the same `Created` container), and multiple
explicit names with one bogus one (real, immediate error, nothing
printed — confirmed the real container was never even attempted,
unlike `cleanup`'s own silent-success inversion).

Full workspace: `cargo build --workspace --locked` (clean), `cargo fmt
--all` (clean after two auto-fixes), `cargo clippy --workspace
--all-targets --locked -- -D warnings` (clean), a full `cargo test
--workspace --locked` run (124 test-result blocks, 0 failures, 0
`FAILED` lines — a fully clean run on the first attempt, no
environmental flakiness this time), `python3 ci/guards.py` (clean),
`cargo deny check` (clean), `bash ci/native-ci.sh` (clean on the first
attempt), `bash ci/build-deb.sh` (clean on the first attempt, real
`dpkg -i`/`--version`/`dpkg -r` round trip for every binary). Pure
CLI-parsing-and-status-check addition — no hot path touched, no
`ci/bench.sh` rerun needed.

## Deliberately still out of scope

`--recursive`-style pod-dependency handling (real `ctr.Init(ctx,
ctr.PodID() != "")`'s own `recursive` argument, and the
`checkDependenciesAndHandleError`/`startDependencies` branches it
gates) doesn't apply: this project has no pods and no container-
dependency-graph concept at all (already established, e.g. `0513`'s
own `rm --depend` no-op).
