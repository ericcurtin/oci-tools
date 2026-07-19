# Design note 0026: lifecycle hooks (`poststart`/`poststop`)

Status: implemented (two of the six real hook points at the time this
note was written; `prestart`/`createRuntime` added by 0035 — see below
for what's still true)
Scope: `oci_spec_types::runtime::{Hook, Hooks}`, new
`oci_runtime_core::hooks` module, wired into `launch::run_reporting_pid`.

## The gap

`ocirun`'s own module doc for `oci-spec-types::runtime` had flagged
"hooks execution" as intentionally unmodeled since the crate's very
first increment. Real hook consumers (NVIDIA's container runtime hooks
for GPU device injection, CNI-based network setup, simple
start/cleanup notification scripts) are a real, common part of the OCI
ecosystem this project aims to be a drop-in replacement for.

## Scope decision: two of six, chosen for what's actually implementable without a bigger architecture change

The real spec defines six hook points: `prestart` (deprecated),
`createRuntime`, `createContainer`, `startContainer`, `poststart`,
`poststop` (`~/go/pkg/mod/github.com/opencontainers/
runtime-spec@v1.3.0/config.md`'s own "Summary" table). Implementing all
six *correctly* needs two things this project doesn't have yet:

* `createRuntime`/`prestart` need to run **before `pivot_root`** —
  requiring a synchronization point mid-way through the rootfs setup
  plan (between "namespaces exist" and "`pivot_root` about to run"),
  which `launch::run`'s single fork-to-exec sequence has no such pause
  in today. **Added by 0035**: a second readiness pipe, the same shape
  as 0034's own cgroup one, the child blocks on right at the start of
  rootfs setup. `createContainer`/`startContainer` need to run **inside
  the container's own namespaces** — needing either `nsenter`-style
  namespace-joining machinery around the hook's own exec, or running
  them from inside the forked child itself before it hands off to the
  user's command. Still not done, even after 0035.
* `poststop`, per the strict spec wording, runs "after the container is
  deleted but before the delete operation returns" — for the two-phase
  `create`/`start`/`kill`/`delete` lifecycle (0017), nothing in this
  project's own process tree stays alive long enough to reliably catch
  the moment a *backgrounded* container's process actually exits
  (`create` reparents it to the nearest subreaper and returns
  immediately; nothing here ever calls `waitpid` on it again) — real
  `crun`/`runc`/`containerd` solve this with a persistent per-container
  shim/monitor process (`conmon`, `containerd-shim`), which this project
  doesn't have.

`run` (the combined create+start+wait+implicit-delete path used by both
`ocirun run` and `ociman run`) doesn't have either problem for
`poststart`/`poststop` specifically: the same process that started the
container stays alive, blocking in `process::wait`, for the container's
entire lifetime — a natural, already-correct place to run both. So
**only `poststart`/`poststop`, only for `run`/`run_reporting_pid`**, in
this increment; the other four hook points, and the `create`/`start`/
`kill`/`delete` lifecycle, remain a documented gap (see below).

## `poststart`'s timing is a documented approximation, not a self-pipe trick

The spec says `poststart` hooks run "after the user-specified process
is executed" — i.e. after a successful `execve`, not merely after the
container's pid is known. This project's own pid-reporting pipe
(`report_container_pid`, used by both `create` and `run_reporting_pid`)
fires *before* `unshare`/rootfs setup/identity/seccomp/`exec`, as early
as possible, specifically so a caller can persist state promptly (see
0017/0023) — using that same signal for `poststart` is measurably
earlier than the strict spec wording.

A fully correct fix exists (and was seriously considered): a second,
`CLOEXEC`-flagged pipe whose write end the child holds open throughout
its own setup; a successful `execve` closes any `CLOEXEC` fd
automatically, so the parent's `read()` on that pipe returning `EOF`
with zero bytes is an unambiguous, race-free "the user's command is now
actually running" signal (the standard "self-pipe" technique many
`posix_spawn` implementations use) — distinguishing it from an early
setup failure requires every `fail()` call site to also write an error
message to that same pipe first, which is a wider, riskier diff across
`launch.rs`'s existing, already-tested failure paths than this
increment's "small, safe, reversible" scope calls for.

Given the dominant real-world use for `poststart` (CNI-style network
attachment, "notify container started") only actually needs the
container's pid and namespaces to already exist — which they do by the
time this project's existing signal fires — accepting the documented
timing gap now, with the self-pipe approach noted as a specific,
scoped future improvement, was the more responsible trade-off than
expanding this increment's diff to touch every existing failure path.

`poststop`, by contrast, needed no such compromise: `run` folds
`delete` into itself once the container process exits, and this
project's own `process::wait` return *is* exactly that moment — fully
correct, not an approximation.

## `Hook`/`Hooks` types, verified against the real spec Go module

Field names/casing (`createRuntime`/`createContainer`/`startContainer`
— the exact three that don't just lowercase their Rust names) checked
against the real vendored `opencontainers/runtime-spec` Go module
(`~/go/pkg/mod/.../specs-go/config.go`), not re-derived from the prose
doc alone. All six hook points are modeled and round-trip through
`Spec`'s own `Serialize`/`Deserialize` (a unit test parses the spec
doc's own worked "Example" JSON verbatim), even though only two are
executed — a bundle that sets the other four doesn't silently lose them
from `config.json`, it just doesn't act on them yet.

`Hook::env`'s replace-vs-inherit semantics (empty inherits the
runtime's own ambient environment; non-empty replaces it entirely) came
from reading real `crun`'s own `do_hooks`
(`~/git/crun/src/libcrun/container.c`), not the spec prose, which is
less precise on this point.

## `oci_runtime_core::hooks`: ordinary `std::process::Command`, not this crate's own `fork`/`exec`

Unlike everything else in this crate, a hook process has no namespace/
rootfs concerns of its own — it's just an external program. Two things
needed getting right that a naive `Command::new(path).args(&hook.args)`
gets wrong:

* `hook.args` has "the same semantics as `execv`'s `argv`" per the real
  spec — meaning `args[0]` is conventionally the program's own name,
  the same way a shell script's own `argv[0]` usually mirrors its own
  path. `Command::new` always sets the *actual* `argv[0]` to match the
  program it execs, and `Command::args` only appends *after* that, with
  no way to override it through the safe API — discovered by every
  hook test failing with exit status 2 (`/bin/sh` receiving a bogus
  extra `argv[1]` it tried to treat as a script file) until switched to
  `std::os::unix::process::CommandExt::arg0` for `args[0]` specifically
  and `.args()` for the rest.
* `wait_with_timeout` polls `Child::try_wait` (`std::process::Child` has
  no built-in wait-with-timeout) rather than blocking `wait()`
  unconditionally — `Hook::timeout` is real, honored spec behavior, not
  just a parsed-but-ignored field.

`hooks::run(hooks, state, keep_going)` runs a hook list in order,
stopping at the first failure unless `keep_going` (used for `poststop`
only, matching real `crun`'s own `keep_going=true` there — the
container has already exited, so one broken cleanup script shouldn't
block another's).

## Wiring: `run_lifecycle_hooks` in `launch.rs`, tolerant of failure by design

`run_reporting_pid` gained an `id: &str` parameter (previously entirely
absent — the container ID was only ever used for a debug log line in
`ocirun`'s own `cmd_run`, never threaded into the runtime core at all)
so hook state JSON can include it. `run_lifecycle_hooks` builds the
state (`crate::hooks::HookState`, matching the real spec's own `State`
schema exactly — deliberately not `state::StateView`, which carries
extra, non-spec fields for this crate's own CLI convenience) and calls
`hooks::run`, logging (`tracing::warn!`) and otherwise ignoring any
failure: a broken notify/cleanup hook must not change the *container's*
own exit code, which is the one thing `ocirun run`/`ociman run`'s exit
code is contractually supposed to reflect.

## Real, automated, end-to-end tests

`crates/oci-runtime-core/src/hooks.rs`'s own unit tests (8 cases): empty
hook list, stdin delivery, a failing hook reported as an error,
`keep_going` running every hook vs. stopping at the first failure,
empty/non-empty `env` inheriting vs. replacing the ambient environment,
and a real timeout actually killing a `sleep 30` hook well before it
would otherwise finish.

`tests/tests/ocirun_hooks.rs` (4 cases, a real built `ocirun run` against
a real busybox bundle, hooks configured via raw JSON injected into
`config.json` after `write_bundle`, exactly like `ocirun_run.rs`'s own
PascalCase test does for `ContainerConfig`): a `poststart` hook
receiving `status: "running"` with a real, positive pid and the correct
bundle path; a `poststop` hook receiving `status: "stopped"` with
`pid: 0`; a failing `poststart` hook *not* changing the container's own
(deliberately nonzero) exit code; and a container with no `hooks` at
all still running completely normally (no accidental new requirement).

## Performance

`run_lifecycle_hooks` is a no-op (one `Option` check, immediately
`None` for any bundle without `hooks` configured — which is every
bundle this project's own benchmark has ever used) whenever
`bundle.spec.hooks` isn't set, so `ocirun run`'s hot path is
unaffected. Re-confirmed with the same `hyperfine` methodology
0012/0018/0023/0025 already established: **2.9ms mean**, unchanged
within noise from the established ~2.9-3.1ms baseline, still
comfortably faster (this run: 3.7x) than a freshly re-measured `crun
run` (10.7ms) on this same session's host.

## What's still not here

* `prestart`/`createRuntime` — true when this note was written; added
  by 0035. `createContainer`/`startContainer` — see the scope-decision
  section above for exactly why they need more machinery this project
  doesn't have yet (container-namespace execution); still not done
  after 0035 either.
* `poststart`/`poststop` for the `create`/`start`/`kill`/`delete`
  two-phase lifecycle (0017) — needs a persistent per-container
  reaper/shim process this project doesn't have (see the scope-decision
  section's `poststop`/subreaper discussion). Only `run` (used by both
  `ocirun run` and `ociman run`) gets hooks in this increment.
* `poststart`'s timing is an approximation (fires once the container's
  pid/namespaces exist, not strictly "after the user's command has
  executed") — see the dedicated section above; the self-pipe technique
  to make it exact was deliberately deferred, not overlooked.
* `SeccompFdName`/`ContainerProcessState` (a namespaced-notification
  variant of hooks for seccomp `SCMP_ACT_NOTIFY` listener handoff) —
  irrelevant regardless, since `SCMP_ACT_NOTIFY` itself isn't supported
  (0016).
