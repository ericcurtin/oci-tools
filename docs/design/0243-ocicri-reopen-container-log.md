# Design note 0243: `ocicri` `ReopenContainerLog`

Status: implemented
Scope: `bin/ocicri/src/launcher.rs`, `bin/ocicri/src/runtime_service.rs`,
`tests/tests/ocicri_container.rs`.

## Log rotation's other half

0242 deferred this with a named reason: rotating a CRI log means
kubelet renames the file away and then calls `ReopenContainerLog`, so
the runtime's logger starts a fresh file at the same path — which
needs a way to tell an already-running logger process to reopen.

## The control FIFO — the same mechanism real conmon uses

Checked directly (`internal/oci/runtime_oci.go`): real cri-o
implements this RPC by writing a command into conmon's own control
fifo (`ctl` in the bundle path). `ocicri` does the identical thing
with its own logger:

- `setup_cri_logging` creates a `logger-ctl` FIFO in the bundle
  directory (before the logger forks, so the RPC can never race its
  existence — and only for logged containers at all).
- The logger runs a *detached* control thread (deliberately not a
  scoped one: it blocks forever awaiting the next command, and the
  logger's own exit at stream-EOF must not wait for it): open the
  FIFO (blocks until a writer), read; any bytes mean "reopen" — a
  fresh create-or-append handle at the same path swapped into the
  shared `Mutex<File>` both copy threads write through. Append,
  never truncate: if nothing actually rotated, existing content must
  survive.
- The RPC writes one byte via an `O_WRONLY|O_NONBLOCK` open — which
  doubles as a real liveness check (`ENXIO` means no reader: the
  logger is between control rounds, retried briefly, or genuinely
  gone, a clear error at the deadline).

## RPC semantics

Matching real cri-o's own (`server/container_reopen_log.go`): unknown
container an error (`NotFound` here), anything not running its
verbatim "container is not running". One honest narrowing: a running
container that was never given a log path has no logger and no log to
rotate — a clear error rather than a silent success (real cri-o
always has a conmon to command; this project only runs a logger when
kubelet configured logging).

## Verified

- Integration, end to end over a real socket: a ticking container's
  log is renamed away (kubelet's own rotation move), the RPC issued,
  and fresh `tick` lines land in a brand-new file at the original
  path while the renamed file keeps the old ones; reopen before start
  is the verbatim not-running error; a running log-less container is
  the no-log-path error; unknown ID is `NotFound`. Run three times
  consecutively (the FIFO open/retry window is real) — stable at
  ~2s, no leaked processes.
- Full workspace: `cargo build`, `cargo test --workspace`,
  `cargo fmt --all --check`, `cargo clippy --workspace --all-targets
  -- -D warnings`, `python3 ci/guards.py`, `cargo deny check`,
  `bash ci/native-ci.sh`, `ci/build-deb.sh`, `ci/bench.sh` sanity
  (`ocicri`-only; no shared crate touched).
