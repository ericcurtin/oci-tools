# Design note 0240: `ocicri` `ExecSync`

Status: implemented
Scope: `bin/ocicri/src/launcher.rs`, `bin/ocicri/src/main.rs`,
`bin/ocicri/src/runtime_service.rs`, `tests/tests/ocicri_container.rs`,
`tests/tests/ocicri_version.rs`.

## The probe RPC

With real running containers (0238), `ExecSync` is the most
consequential remaining `RuntimeService` RPC: kubelet's exec
liveness/readiness probes are `ExecSync` calls, made continuously
against every probed container. It also maps directly onto machinery
this project already has: `oci_runtime_core::exec` (the same shared
code `ociman exec`/`ociman healthcheck run` use), plus 0238's own
helper-process pattern for the fork-safety problem (`exec::exec`
forks; a tokio server can't).

## Shape

A second hidden re-exec entry point, `__exec <BUNDLE_DIR> <PID>
<CMD...>` — a fresh, single-threaded `ocicri` process that builds the
`ExecRequest` exactly like `ociman exec` does (namespaces/user/
capabilities/no-new-privileges/cwd/env all from the target's own
bundle spec; the CRI request has no per-call overrides for any of
these) and execs the command inside the running container of `<PID>`.
Unlike `__launch` there's no on-disk protocol: nothing outlives the
RPC, so the server just spawns the helper with real pipes, captures
stdout/stderr directly, and takes the helper's own exit status as the
command's (with one conventional exception: a *setup* failure —
bundle unreadable, `setns` denied — exits 126, the shell's own
"command invoked cannot execute" code, with the reason on stderr,
which the response returns verbatim anyway).

The helper `setsid`s so its pid is its process group: on timeout the
server SIGKILLs the *group* (`kill(-pid)`), which takes the
namespace-joined exec child down with the helper (`setns` changes
namespaces, never process-group membership).

## Semantics, checked directly against real cri-o

(`server/container_execsync.go`, `internal/oci/runtime_oci.go`,
conmon's own `config.go`):

- Unknown container: `NotFound` ("could not find container %q").
- No living process: `NotFound` too. One documented narrowing: real
  cri-o can exec into its own *created* containers (their paused init
  is alive and fully set up); this project's created containers have
  no process at all (0236), so only `RUNNING` is exec-able here.
- Empty command: cri-o's verbatim "exec command cannot be empty".
- **Timeout is a successful response**, `exit_code: -1`, stderr
  `"command timed out"` (conmon's `TimedOutMessage`, verbatim) — real
  cri-o's own explicit comment explains why this must never be a gRPC
  error: kubelet's prober checks the exit code, and an error would
  wedge the probe in `Unknown` instead of restarting the container.
- Exit codes come back verbatim (`128+signal` for signal deaths, the
  same convention everywhere else in this project).

## A real race found (and fixed) while testing this

The launcher's pid file is written the moment the container pid
*exists* (`on_pid` fires when the child is forked, before it runs its
rootfs setup and execs) — 0238 already documented that `RUNNING` is
therefore reported pre-exec, and its own stop test had already met
one consequence (pre-exec SIGTERM discarded by a pid-ns init). This
increment met the other: an exec that joins the target's namespaces
inside that window lands in a half-set-up world — observed directly
as a real test flake, in both directions: `/proc/1/cmdline` showing
the *launcher's own pre-exec argv*, and (nastier) the exec child
joining a pre-pivot mount namespace, running against the *host*
filesystem view, and once leaving a reader-blocking orphan that made
the test take 31s instead of 1.8s.

Real runc doesn't have a window at this point because its `create`
completes all setup before pausing at the start fifo — exec against a
created container is safe by construction. The equivalent safe point
here: the target's `/proc/<pid>/cmdline` stops carrying this binary's
own pre-exec argv at exactly the `execve` that ends setup. The
`__exec` helper now blocks on that flip (10ms polls, 10s deadline,
"exited before exec" detected distinctly) before ever joining
namespaces. Three consecutive full-suite runs pass at ~1.8s each.

(A future, cleaner alternative — a real exec-readiness signal in
shared `launch`, which would also close `ociman exec`'s own
theoretical version of this window — is noted for its own increment;
the cmdline gate is correct and contained today.)

## Verified

- The integration test drives everything over a real socket: stdout
  and stderr captured separately with the command's own exit code
  (7); a probe *proving in-container execution* (`/proc/1/cmdline`
  inside the exec's own view is the container init, `/bin/sleep
  300`); the timeout shape (`-1`/`"command timed out"`, returning in
  ~1s, never 30); empty-command/unknown-container/never-started/
  exited each with their exact code; run three times consecutively to
  guard against the race this note documents.
- Leaked-process check: no `__launch`/`sleep` strays after a clean
  suite run (earlier debugging strays were from panicked test runs
  killed mid-flight, cleaned by hand).
- The wire-level unimplemented-sample test moved from the
  now-implemented `ExecSync` to `Attach`.
- Full workspace: `cargo build`, `cargo test --workspace`,
  `cargo fmt --all --check`, `cargo clippy --workspace --all-targets
  -- -D warnings`, `python3 ci/guards.py`, `cargo deny check`,
  `bash ci/native-ci.sh`, `ci/build-deb.sh`, `ci/bench.sh` sanity
  (change confined to `ocicri`; no shared crate touched).
