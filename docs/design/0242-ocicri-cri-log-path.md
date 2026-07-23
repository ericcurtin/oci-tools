# Design note 0242: `ocicri` CRI log path

Status: implemented
Scope: `crates/oci-spec-types/src/time.rs`, `bin/ocicri/src/launcher.rs`,
`bin/ocicri/src/container.rs`, `bin/ocicri/src/runtime_service.rs`,
`tests/tests/ocicri_container.rs`.

## `kubectl logs` needs a file to read

Kubelet never streams a running container's output through the CRI —
it reads a real file, written by the runtime in the CRI logging
format (one line per entry: `<RFC3339Nano> <stream> <P|F> <content>`,
e.g. `2016-10-06T00:17:09.669794202Z stdout F log content`), at the
path it supplied at `CreateContainer` time: the sandbox config's
`log_directory` joined with the container config's `log_path`
(matching real cri-o's own `filepath.Join(sb.LogDir(), ...)`).
Until now `ocicri`'s launcher discarded container output entirely
(0238 noted this as a later increment); this lands it.

## The logger: conmon's other half, as another forked process

0238's launcher-keeper already is half of conmon (keep the pid,
record the exit). The other half is log formatting, and it lands the
same way everything else here respects `launch`'s fork-safety
contract — as a *process*, never a thread in the launcher:

- When a log path was configured, the launcher (still
  single-threaded) creates two pipes, wires its own fds 1/2 to the
  write ends, and forks a **logger** child that owns the read ends.
  The container inherits 1/2 through the same `run_reporting_pid`
  call as always (`discard_output` false now) — no shared-crate
  change needed at all.
- The logger (which never forks, so it's free to thread) drains both
  streams through a line splitter into the log file (parent
  directories created — kubelet's own convention routinely nests
  `<name>/<restart#>.log`), one CRI-format line per write. Complete
  lines are `F`; a line exceeding 8192 bytes is cut into `P` chunks
  (real conmon's own buffer-driven behavior), with the cap checked
  *before* the newline deliberately — bytes accumulate across pipe
  reads, so a newline can arrive after the cap is already exceeded,
  and the cap must still win (found by the unit test, not by luck).
  An unterminated EOF tail is a final `P`.
- Ordering: the launcher re-points its own 1/2 at `/dev/null` right
  before recording the exit, so the logger's EOF (and final flush)
  arrives no later than the exit becomes observable.

`format_rfc3339_nanos_utc` joins the existing hand-rolled
second-precision formatter in `oci-spec-types` (same civil-calendar
math, no date/time dependency), unit-tested against the CRI
documentation's own example timestamp byte for byte.

## Wiring

- `ContainerRecord.log_path` (serde-default, `None` for pre-0242
  records): the joined absolute path, stored at `CreateContainer`
  only when kubelet supplied both halves (`crictl` routinely supplies
  neither — no log config means no pipes, no logger, output discarded
  exactly as before).
- `StartContainer` passes it to the launcher as an optional third
  argv; `ContainerStatus` reports it (empty when none), which is
  exactly where kubelet learns the path back from.
- `ReopenContainerLog` (log rotation) stays honestly unimplemented:
  it needs a way to tell the running logger to reopen its file — a
  real, small protocol of its own, deferred.

## Verified

- Unit: the CRI line shape byte for byte; the splitter against a real
  pipe (F lines, an oversize line cut into a `P` chunk at exactly the
  cap plus its terminated remainder, the unterminated EOF tail as
  `P`); the timestamp formatter against the CRI doc's own example.
- Integration (real socket, real container): stdout and stderr lines
  land in the real file with correct streams/tags/timestamp shape, an
  unterminated `printf` becomes a real `P` entry, the file is
  complete right after the exit is observable, `ContainerStatus`
  reports the joined path, and a container with no log config gets no
  file and an empty `log_path`. One real property the first version
  of the test got wrong: entries from *different* streams have no
  guaranteed relative order (two pipes, two logger threads — real
  conmon behaves identically, kubelet orders by timestamp), so the
  assertions are per stream, where order is real.
- Full workspace: `cargo build`, `cargo test --workspace`,
  `cargo fmt --all --check`, `cargo clippy --workspace --all-targets
  -- -D warnings`, `python3 ci/guards.py`, `cargo deny check`,
  `bash ci/native-ci.sh`, `ci/build-deb.sh`, `ci/bench.sh` sanity
  (`ocicri`-only launch-path change; no other binary's code touched —
  the one shared addition is a cold-path timestamp formatter).
