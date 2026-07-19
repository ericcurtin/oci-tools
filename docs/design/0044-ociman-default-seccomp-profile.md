# Design note 0044: `ociman run`'s own default seccomp profile

Status: implemented
Scope: `crates/oci-runtime-core/src/seccomp.rs` (`default_profile`,
`filter_to_supported_syscalls`), `bin/ociman/src/main.rs`
(`synthesize_spec`, `cmd_stop`'s own signal-retry fix — see below).

0036 built full multi-action seccomp support, closing the gap that had
blocked this from working at all — but `ociman run` itself has never
set `linux.seccomp` on any container it starts, meaning **every
container `ociman run` has ever started has had zero seccomp
confinement**, explicitly flagged as a real, known gap in 0037/0038's
own "what's still not here" sections. This closes it: `ociman run` now
always applies a real default profile, matching real `podman run`'s
own default behavior.

## The bundled profile: a real capture, not a reimplementation of `container-libs`' own richer schema

The real, authoritative default profile source is `container-libs`'
own `common/pkg/seccomp/seccomp.json` — but that file uses a
*different*, richer schema than the plain OCI runtime-spec
`linux.seccomp` shape this crate already supports: per-syscall
`includes`/`excludes` conditions (e.g. `{"caps": ["CAP_SYS_ADMIN"]}`,
meaning "only apply this rule if the container has this capability"),
resolved by `container-libs`' own Go code *before* it ever becomes a
real container's own flat `config.json`. This project has no
capability-conditional seccomp-resolution logic of its own, and every
container it runs so far gets exactly the same (default) capability
set — so reimplementing that conditional-resolution machinery just to
immediately resolve it for one fixed capability set would be pure
overhead for no benefit.

Instead, the bundled profile (`crates/oci-runtime-core/src/data/
default_seccomp_profile.json`) is extracted directly from this
project's own existing test fixture,
`podman-generated-config-with-seccomp.json` (0016's own real capture:
`podman run`'s actual on-disk `config.json`, podman 4.9.3/crun 1.14.1)
— i.e. the *already-resolved*, flat, OCI-spec-shaped output of exactly
that conditional resolution, for exactly the default capability set
this project's own containers already get. Embedded via
`include_str!` (a compile-time, version-controlled resource, not
runtime file I/O).

## Tolerating architecture-portability gaps the same way real `container-libs` does

The bundled profile, since it's built from `container-libs`' own
union-of-every-architecture syscall table, still lists syscall names
that simply don't exist on every architecture (legacy 32-bit-compat
names like `bdflush`/`fcntl64`/`chown32` genuinely aren't syscalls on
aarch64 at all — confirmed directly against a real kernel while first
verifying this same profile end to end in 0036). Checked directly
against real `container-libs`' own behavior
(`common/pkg/seccomp/filter_linux.go`'s `matchSyscall`): *"If we can't
resolve the syscall, assume it's not supported on this kernel. Ignore
it, don't error out."* — real `podman`/`crun`, via real `libseccomp`,
silently tolerate exactly this on every architecture they run on.

`filter_to_supported_syscalls` replicates this: it drops any syscall
name that doesn't actually resolve on the current architecture (by
attempting to compile a trivial single-syscall document for it,
reusing the existing `compile_single_syscall` — there's no cheaper way
to query `seccompiler`'s own name table, per this module's own
established constraint), dropping an entry entirely once it loses
every one of its own names. This is deliberately scoped to the
*default* profile specifically — a hypothetical future user-supplied
profile (`ociman` has no way to accept one yet) should stay strict, the
same way `apply` already is: an unknown syscall name there is much more
likely a real typo worth surfacing loudly, not an architecture
portability non-issue.

## A real regression found and fixed while verifying this end to end: a signal-delivery race, not a seccomp bug

Wiring this in initially broke `stop_lets_a_signal_handling_container_
exit_gracefully` (`tests/tests/ociman_stop.rs`) consistently — but not
because the seccomp profile blocks anything the test's own trap
mechanism needs (confirmed directly: `exit`/`exit_group`/
`rt_sigaction`/`rt_sigreturn`/`kill`/`wait4` are all in the bundled
profile's own `ALLOW` list). A `git stash` A/B comparison confirmed the
regression was real (the test passed reliably without this increment's
own changes, failed consistently with them) before looking for why.

The actual cause is a genuine, pre-existing race this increment's own
added latency (compiling the default profile — many more syscalls than
any profile this project has ever applied before) made *practically
observable* for the first time, rather than a new bug in the
translation itself: **a plain, unhandled-by-default signal (like
`SIGTERM`) sent to a PID-namespace's own init process is *silently
ignored by the kernel* for as long as it has no handler installed *at
the moment the signal arrives*** (`man 7 pid_namespaces`) — not queued
for later. This is the *same* real kernel behavior 0017 already
documented and tested (`ocirun kill`'s own "a plain SIGTERM to a
PID-namespace's own init is silently ignored" finding) — but 0017's own
case was about a container that *never* installs a handler at all
(matching real `docker`/`podman`/`runc` too, not a bug). This
increment's own case is subtly different: the container's own command
(`trap 'exit 0' TERM; while true; do sleep 0.2; done`) *does*
eventually install a real handler — just not yet, at the exact moment
`ociman run`'s own pid-reporting (which fires early, right after fork,
long before rootfs setup/identity/`seccomp` even start — 0017/0023's
own deliberate design, so a concurrent `ociman ps`/`stop`/`exec` sees a
live pid as soon as possible) lets a *concurrent* `ociman stop` send
its very first `SIGTERM`. Before this increment, that window (fork to
actual `exec`) was negligible; adding a genuinely more expensive
`seccomp` compilation step immediately before `exec` widened it enough
to turn a previously near-impossible race into a reliably reproducible
test failure.

### The fix: retry the initial signal a few times, early, not indefinitely

`cmd_stop` now re-sends the same initial signal up to 4 more times, 200ms
apart (skipped entirely for an explicit `--time 0`, so an immediate-
escalation request isn't delayed at all), *before* falling back to its
existing passive wait-then-escalate-to-`KILL` loop for the rest of the
grace period. This closes the race without needing to know anything
about *why* the very first attempt might have arrived too early — as
long as the container's own command eventually installs its handler
within that short initial window, a later resend gets through. Bounded
to a handful of early resends, not the whole grace period: plenty of
real entrypoints treat a *second* signal as "stop being graceful, exit
now" (a well-known convention), so resending indefinitely would risk
forcibly escalating an ordinary graceful shutdown that simply takes a
few real seconds to finish, which would defeat the entire point of a
grace period in the first place.

Verified the fix actually closes the race, not just reshapes it: the
same test that failed consistently before now passes consistently
across many repeated runs (5 full test-file runs, no failures) with
this increment's own default-seccomp-profile change still in place.

## Real, automated, end-to-end tests

`run_applies_a_default_seccomp_profile_blocking_a_real_syscall`
(`tests/tests/ociman_run.rs`): a real, unconfounded verification —
`swapon` against a real, existing-but-not-swap-formatted file fails
with `Operation not permitted` specifically (seccomp's own `ERRNO`
action, which real `podman`'s own default profile also blocks) rather
than some other, unrelated error. Confirmed by hand first (not assumed)
that the *same* command, with *no* seccomp at all (`ocirun run`, unset
`linux.seccomp`), instead fails with a distinct, real kernel
filesystem-validation error ("file has holes") — proof the syscall
genuinely reaches the kernel without seccomp, and genuinely doesn't
with it, ruling out the confound that `swapon` might just as easily
fail for an unrelated reason (e.g. a rootless container's own real
capability restrictions) regardless of seccomp.

`crates/oci-runtime-core/src/seccomp.rs` gained unit tests for
`default_profile` (parses successfully, survives filtering, and
filtering demonstrably drops at least one name while keeping others —
proving it's not a no-op by accident) and `filter_to_supported_
syscalls`/`is_syscall_name_supported` (a real syscall kept, a fake one
dropped).

## Performance

`ocirun run` completely unaffected (2.8ms, unchanged): `ocirun` never
sets a default profile, only ever the spec-driven one a real
`config.json` explicitly asks for.

`ociman run` (real busybox pull-already-cached run-destroy cycle): a
direct `git stash` A/B comparison on the same host measured 44.0ms ±
10.6ms before this change, 52.9ms ± 9.0ms after (100 runs each,
10 warmups) — a real, modest cost (compiling ~300 architecture-valid
syscalls, once, per container start) within roughly one standard
deviation of either measurement, not a dramatic regression. Directly
compared against real `podman run --rm` on the same host for the same
operation: **167.2ms mean** — `ociman run` remains **~3.2× faster**
even with this real security feature now included, not a benchmark
regression the project's own stated goal would consider unacceptable.

## What's still not here

* No way for a user to supply their *own* seccomp profile, or opt out
  entirely (`--security-opt seccomp=`/`--privileged`, in real `podman`/
  `docker` terms) — this default is unconditional for every container
  `ociman run` starts.
* `ARG`/`--build-arg`... not relevant here; carried over from 0037's
  own still-open items: `--memory-swap`/`--cpuset-cpus`/
  `--cpuset-mems` CLI flags still don't exist.
* The signal-retry fix in `cmd_stop` is a general-purpose mitigation
  for *any* slow-starting container (not seccomp-specific at all), but
  it's still probabilistic in principle — a container whose own
  startup work somehow takes longer than the ~800ms retry window
  could still lose its very first graceful-stop signal. Not observed
  in practice (this project's own containers, even with the new
  default profile, start in tens of milliseconds), but worth noting as
  a real, if now very unlikely, remaining edge case rather than a
  fully eliminated one.
