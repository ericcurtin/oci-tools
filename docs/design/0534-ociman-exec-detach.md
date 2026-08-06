# Design note 0534: `ociman exec --detach`/`-d`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_exec.rs`.

## What this closes

Real `podman exec --detach`/`-d` starts an additional process inside
an already-running container and returns immediately, printing a
real, persisted exec-session id instead of blocking until it exits.
`ociman exec` had no equivalent at all — `0533` (`ocirun exec
--detach`) explicitly named this as its own deliberately deferred,
out-of-scope gap, closed here.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/exec.go:64`: `flags.BoolVarP
  (&execDetach, "detach", "d", false, "Run the exec session in
  detached mode (backgrounded)")`.
- `~/git/podman/cmd/podman/containers/exec.go:204-219`:
  ```go
  } else if !execDetach {
      exitCode, err := registry.ContainerEngine().ContainerExec(...)
      registry.SetExitCode(exitCode)
      return err
  }
  id, err := registry.ContainerEngine().ContainerExecDetached(registry.Context(), nameOrID, execOpts)
  if err != nil { return err }
  fmt.Println(id)
  return nil
  ```
- `~/git/podman/pkg/domain/infra/abi/containers.go:1003-1035`
  (`ContainerExecDetached`): creates a real, persistent *exec
  session* (`ctr.ExecCreate`, which allocates an opaque session id
  and records it in the container's own on-disk state,
  `libpod/container_exec.go:206-247`), starts it (`ctr.ExecStart`),
  and returns that id without ever waiting for it to finish.

This project's own `oci_runtime_core::exec` has no persisted exec-
session concept of any kind — it's a one-shot fork-then-forget
primitive, with nothing recorded anywhere once a call returns. The
closest honest value to print instead of real podman's own opaque
session id is the exec'd process's own real, host-visible pid — the
exact same value `0533`'s own `ocirun exec --detach --pid-file`
already exposes (here, printed directly to stdout with no file
needed, matching real podman's own unconditional `fmt.Println`).

## Implementation

`bin/ociman/src/main.rs`:
- New `Command::Exec::detach: bool`, `#[arg(short, long)]`, and the
  identical field on the nested `ContainerCommand::Exec` alias
  (whose own doc comment previously, correctly, named `--detach` as
  part of its honestly-narrower first-slice scope — updated to note
  it's now closed).
- `cmd_exec` gains a `detach: bool` parameter (needing `#[allow
  (clippy::too_many_arguments)]`, already present on many other
  functions in this file at a similar parameter count), threaded
  through from both dispatch sites.
- The one `ExecRequest` literal's own `detach: false` (the exact spot
  `0533`'s own "deliberately out of scope" comment named) becomes the
  real, given value.
- The call site swaps from the simpler `oci_runtime_core::exec::exec`
  wrapper (which discards the pid) to `exec_reporting_pid` directly —
  already exported, already used by `ocirun exec` for the identical
  reason — whose callback now prints the exec'd pid when `detach` is
  set (a no-op for every non-detached call, the same "pay for one
  extra pipe and a 4-byte read, not a behavioral difference" cost
  every other caller of this shared function already accepts).
  `exec_reporting_pid` itself already returns `Ok(0)` unconditionally
  for a detached call (0533's own short-circuit, reused verbatim
  here with zero runtime-core changes needed) — `std::process::exit`
  at the end of `cmd_exec` applies uniformly either way, matching
  real podman's own identical "detached exec never surfaces any
  later exit code back to this invocation's own exit status at all".

## Tests

Two new integration tests in `tests/tests/ociman_exec.rs`:
- `exec_detach_returns_immediately_and_prints_the_exec_pid` — a real
  wall-clock bound (`< 2s`) on the `exec --detach` call itself, given
  a `sleep 3`-then-marker-write command; confirms the printed value
  is a real, positive pid, the container stays `running` throughout,
  and (via a *second*, ordinary non-detached `exec`, polled) the
  marker eventually appears, proving the detached command really did
  keep running in the background.
- `exec_detach_exits_zero_even_though_the_detached_command_will_eventually_fail`
  — `--detach`'s own exit code is always `0`, regardless of the
  detached command's own eventual (here, deliberately nonzero) exit.

A real test-harness pitfall hit and fixed while writing these — a
*second*, subtler instance of `0533`'s own already-documented "the
detached grandchild inherits stdio, so a captured pipe never sees
`EOF`" hazard: even redirecting only stdin/stderr to `Stdio::null()`
and capturing *just* stdout via a pipe (needed here, unlike `0533`'s
own three tests, since this project's `exec --detach` genuinely
prints something meaningful to stdout that must be read back) still
hangs, because the detached grandchild inherits *that* pipe too.
Fixed by redirecting the invocation's own stdout to a real
`NamedTempFile` instead (a file has no "every writer must close it
first" `EOF` semantics at all) and reading it back only after
`Command::status()` returns. A second, unrelated bug in the test
itself (not the feature) was also caught this way: the seeded test
image's own busybox applet list initially omitted `cat`, so the
verification step's own `/bin/cat` call failed with "not found" —
fixed by adding it to the seed.

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean after one auto-fix), `cargo clippy --workspace
--all-targets --locked -- -D warnings` (clean), the full
`ociman_exec.rs` suite (20/20, including every pre-existing test —
confirming the non-detached default path is unaffected),
`ociman_healthcheck.rs`/`ocirun_exec.rs` (both fully passing,
confirming every other caller of the shared `exec`/`exec_reporting_
pid` primitives is unaffected), manual exercise of both the
top-level `ociman exec --detach` and the nested `ociman container
exec --detach` alias against a real built image (both print a real
pid and return immediately), a full `cargo test --workspace --locked`
run (125 test-result blocks, 0 failures), `python3 ci/guards.py`
(clean), `cargo deny check` (clean), `bash ci/native-ci.sh` (failed
once on its own internal `cargo test` with two different, already-
documented transient `ocicri_container.rs` failures under this
host's own concurrent-`opencode`-session load, each confirmed
transient by isolated rerun; a fully clean rerun with
`RUST_TEST_THREADS=2`), `bash ci/build-deb.sh` (clean on the first
attempt, real `dpkg -i`/`--version`/`dpkg -r` round trip). Since this
changes `ociman exec`'s own call pattern against the shared
`exec_reporting_pid` (capturing the pid via callback instead of
discarding it via the simpler `exec` wrapper), also re-ran `bash
ci/bench.sh` in full: `ociman exec` unaffected or slightly improved
(`13.53×`/`41.38×` faster than `docker exec`/`podman exec`, up from
`10.69×`/`32.89×` in `0533`'s own last measurement, well within
ordinary run-to-run noise), `ocirun exec`/`run` likewise unaffected.
No regression anywhere.

## Deliberately still out of scope

`--detach-keys` (real podman's own key-sequence-for-detaching-a-
*terminal*-session flag) doesn't apply: this project has no PTY
allocation for `exec` at all (the same project-wide gap `0207`/`0531`
already document), so there is no live, attached terminal session a
key sequence could ever detach *from* in the first place — a
category-different, not merely narrower, gap than `--detach` itself.
