# Design note 0238: `ocicri` `StartContainer`/`StopContainer` — real processes

Status: implemented
Scope: `bin/ocicri/src/launcher.rs` (new), `bin/ocicri/src/main.rs`,
`bin/ocicri/src/runtime_service.rs`, `bin/ocicri/src/container.rs`,
`tests/tests/ocicri_container.rs`.

## The launcher-keeper: this project's own conmon

0237 left `StartContainer` scoped out with a real technical reason:
`oci_runtime_core::launch`'s fork-based entry points require a
single-threaded calling process (`process::fork`'s own safety
contract), which `ocicri`'s multithreaded tokio server can never
satisfy. The answer, landed here, is the same architecture real cri-o
uses (conmon — one small monitor process per container), built from
this project's own pieces:

`StartContainer` spawns a **fresh `ocicri` process** re-executing the
current binary with an internal `__launch` argv (`std::process::
Command` is a real fork+immediate-exec, safe from a multithreaded
parent; the fresh child is single-threaded at entry, exactly like
`ociman run -d`'s keeper is at its own fork point). Re-exec of self is
the same technique real `runc` uses for `runc init` — a process-model
necessity, not a "shell out to an external tool" (this project's
shelling-out policy is about the latter; nothing external is invoked).

The launcher (`launcher.rs`):
- `setsid`-detaches (a Ctrl+C delivered to the server's group never
  takes running containers down), and deliberately outlives the
  server — a running container and its eventual real exit code
  survive an `ocicri` restart, matching conmon's lifetime exactly.
- launches the 0237-prepared bundle via the same shared
  `run_reporting_pid` every other real launch here uses, with the
  same systemd transient scope `ociman run` gives its containers
  (also what `ocicri`'s own `RuntimeConfig` RPC already tells kubelet
  this project uses; one scope per container, never reused — a CRI
  container starts at most once, restarts are new containers with
  `attempt+1`).
- speaks a tiny atomic on-disk protocol in the bundle dir: `pid`
  (written from `on_pid`; the server's start waits for it),
  `exit.json` (`{exit_code, finished_at_nanos}`, the whole reason the
  keeper sticks around; `128+signal` for signal deaths,
  `process::exit_code`'s documented convention), `start-error` (a
  launch failure the server reads instead of parsing anyone's
  stderr). The server reaps the launcher child from a detached thread
  so it never lingers as a zombie.

## RPC semantics, checked directly against real cri-o

- **StartContainer** (`server/container_start.go`): unknown ID a real
  `NotFound`; only `CONTAINER_CREATED` can start — its verbatim
  "container %s is not in created state" otherwise; on success the
  record becomes `RUNNING` with the real pid and `started_at`.
- **StopContainer** (`server/container_stop.go`,
  `internal/oci/runtime_oci.go`): unknown ID a silent, idempotent
  success ("must not return an error if the container has already
  been stopped"); already-exited likewise; a never-started container
  just gets its finished state settled (cri-o's own `Living()`-fails
  path — no exit code ever existed, reported as `-1`, its own
  `ExitCode == nil` fallback); a running one gets SIGTERM (per-image
  `STOPSIGNAL` is a documented later increment), up to `timeout`
  seconds to comply (async polling, never parking a tokio worker),
  then SIGKILL via the blocking pool.
- **State reconciliation**: `ContainerStatus`/`ListContainers` (and
  every mutation) reconcile a `RUNNING` record against the launcher's
  own `exit.json`/pid liveness first, so a state filter sees the
  genuinely current state; a dead pid without an exit record gets a
  bounded re-poll (the launcher writes it moments after death) before
  the honest `-1` fallback. `EXITED` status reports real
  `started_at`/`finished_at`/`exit_code` and the kubelet-conventional
  `Completed`/`Error` reason.
- **Forceful paths**: `RemoveContainer` of a running container
  SIGKILLs first (the proto's contract), as do `StopPodSandbox`'s and
  `RemovePodSandbox`'s container cascades.

## Two real behaviors the tests had to learn (not bugs anywhere)

- A pid-namespace **init with no handler installed silently discards
  SIGTERM** from the parent namespace (a kernel rule). `RUNNING` is
  reported from the moment the pid exists — before exec — so a stop
  issued the instant `RUNNING` appears can race the container's own
  handler installation (real `docker stop` on a handler-less pid 1
  waits out its whole grace period and SIGKILLs for the same
  underlying reason). The graceful-stop test therefore waits for the
  container's own in-rootfs signal (a `touch`ed file after `trap`)
  before stopping, and asserts the trap's own exit code (42) comes
  back — proving the graceful path end to end.
- busybox `sh` redirects a backgrounded job's stdin from `/dev/null`,
  which this project's containers don't populate in `/dev` yet — a
  `sleep 300 & wait` test variant exited 0 instantly because the
  background spawn itself failed. (Populating default `/dev` nodes is
  a real, known runtime-core gap worth its own increment; the test
  uses a foreground loop.)

## Verified

- `tests/tests/ocicri_container.rs` (9 tests, all over a real Unix
  socket against the real spawned binary): a real `/bin/true`
  container runs to completion (exit 0, `Completed`, real
  timestamps); a second start is the verbatim not-in-created-state
  error; graceful stop delivers SIGTERM for real (trap exit code 42
  comes back), second stop idempotent; stopping a never-started
  container settles it at `-1`; forceful remove kills a live sleeper
  and removes bundle+record; `StopPodSandbox` SIGKILLs its running
  containers (`128+9` recorded); plus all pre-existing create/list/
  filter/cascade/restart-persistence tests unchanged.
- Full workspace: `cargo build`, `cargo test --workspace`,
  `cargo fmt --all --check`, `cargo clippy --workspace --all-targets
  -- -D warnings`, `python3 ci/guards.py`, `cargo deny check`,
  `bash ci/native-ci.sh`, `ci/build-deb.sh`, `ci/bench.sh` sanity
  (change confined to `ocicri`, the deliberate long-lived-server
  exception; no shared crate touched).

## Still ahead

Sandbox-namespace joining (needs 0233's deferred namespace pinning),
per-image `STOPSIGNAL`, the CRI log path, exec/attach/port-forward,
stats, and populating default `/dev` nodes in runtime-core.
