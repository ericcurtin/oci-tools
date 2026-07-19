# Design note 0085: `ocirun create/run --pid-file` (milestone 3)

Status: implemented
Scope: `bin/ocirun/src/main.rs` (`Command::Run`/`Command::Create`'s new
`pid_file` flag, `cmd_run`'s own switch to calling
`launch::run_reporting_pid` directly, new `write_pid_file`/
`write_pid_file_inner`), `bin/ocirun/Cargo.toml` (new `libc`
dependency), `tests/tests/ocirun_lifecycle.rs`.

Real `runc create --pid-file`/`runc run --pid-file` write the
container's own pid to a file as soon as it's known — used by process
supervisors (systemd units, shell scripts) that need to track a
container's real pid without parsing `ocirun state`'s own JSON output.
`ocirun` had no equivalent at all.

## Checked directly against real runc's own `createPidFile`

`~/git/runc/utils_linux.go`'s own `createPidFile`, read directly:
create a temp file (`.{basename}`, same directory) with
`O_RDWR|O_CREATE|O_EXCL|O_SYNC`, mode `0o666`, write the bare decimal
pid (no trailing newline), then atomically rename it into place — so a
concurrent reader (the whole point of `--pid-file`) can never observe
a half-written file. `write_pid_file_inner` replicates this exactly,
including the `O_SYNC` flag (the write reaches disk before the rename
makes the file visible) and the file's own exact content/mode.

## Almost entirely already-built plumbing, once the right entry point was found

`ocirun run`'s own `cmd_run` previously called the library's plain
`launch::run`, which (checked directly) is defined as just
`run_reporting_pid(id, bundle, rootfs, None, CgroupSetup::FromSpec,
|_pid| {})` — a thin wrapper around the exact primitive `ociman run`
already uses for its own, more involved pid-reporting needs (recording
a container's own persistent state as soon as it starts). Switching
`cmd_run` to call `run_reporting_pid` directly, with a *real* `on_pid`
closure instead of a no-op one, needed no changes to `oci-runtime-core`
at all — the pid-reporting hook already existed for exactly this kind
of caller.

## A real, deliberate divergence from real runc, documented rather than silently accepted

Real runc's own `run`/`create` **abort the whole invocation** if
`createPidFile` fails (`if err = createPidFile(...); err != nil {
return -1, err }`) — but `run_reporting_pid`'s own `on_pid: impl
FnOnce(i32)` callback has no way to propagate a failure back to its
caller (deliberately, to avoid the real complexity of correctly
tearing down an already-forked, possibly-hook-blocked child process
from deep inside that shared, delicate fork/pipe-orchestration
function — a change judged too large/risky for this increment, unlike
the small, additive change actually made). `write_pid_file` instead
logs and tolerates a write failure, matching this project's own
already-established "auxiliary bookkeeping write: log and tolerate,
don't abort a container that's already running" pattern (`ociman
run`'s own state-record write, cgroup/hook fallbacks elsewhere in this
same file) — a real, honest, documented choice, not an oversight.

## Real, manual end-to-end verification before writing a single automated test

Built the release binary and ran real `ocirun create --pid-file`/
`ocirun run --pid-file` invocations against a real bundle (a real
`busybox` rootfs, `ocirun spec --rootless`-generated `config.json`):
the pid file's own content matched `ocirun state`'s own reported
`pid` field exactly for `create`, and `ps -p <pid>` confirmed the real,
live container process for `run`.

## Real, automated tests

`create_pid_file_writes_the_real_pid` (compares the file's own content
against `ocirun state`'s own real `pid` field) and
`run_pid_file_writes_the_real_pid` (polls for the file's own
appearance since `run` blocks in the foreground, then confirms the pid
is a real, live process via `rustix::process::test_kill_process` — a
signal-0 "is this pid alive" check, not a real signal).

## Performance

Touches `bin/ocirun/src/main.rs`'s own `cmd_run`/`cmd_create` — not
`oci-runtime-core` itself (no changes to `launch.rs`), and not
`bin/ociman`'s own `cmd_run`/`synthesize_spec`/`resolve_seccomp` (a
different binary entirely). Still, `ocirun run` is one of this
project's own two explicitly benchmarked binaries, and `cmd_run`'s own
call site did change (from `launch::run` to `launch::run_reporting_pid`
directly) — so a `git stash`/`git stash pop` A/B `hyperfine`
comparison was run out of caution anyway: noise-dominated as expected
at this sub-millisecond scale (`before` measured 1.06× "faster", well
within one stddev, consistent with this project's own documented
"`ocirun run`/`--version` ~0.2-0.9ms, noise-dominated" baseline). No
plausible regression mechanism: `run_reporting_pid` with a no-op
closure is definitionally identical work to `run` itself.

## What's still not here

* `-v`/`--volume` CLI override for `ociman run` — the last remaining
  item from the small-CLI-gaps survey that produced 0080–0085.
* The build cache, `ONBUILD`/`HEALTHCHECK`, anonymous/untagged build
  mode, `createContainer`/`startContainer` hooks, automated
  failed-systemd-scope cleanup — unchanged, unrelated leftovers from
  earlier milestones.
