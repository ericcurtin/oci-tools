# Design note 0057: `ociman run --security-opt seccomp=<unconfined|path>` (milestone 3)

Status: implemented (`seccomp=` key only — every other `--security-opt`
key, and `--privileged`, are explicitly out of scope, see below)
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_run.rs`.

0044's own "what's still not here" named this exact gap: *"No way for
a user to supply their own seccomp profile, or opt out entirely
(`--security-opt seccomp=`/`--privileged`, in real podman/docker
terms) — this default is unconditional for every container `ociman
run` starts."*

## Almost the entire mechanism already existed — this is a CLI-surface increment, like 0055/0056

`oci_spec_types::runtime::LinuxSeccomp`/`LinuxSyscall` already
deserialize the exact `{"defaultAction": ..., "syscalls": [...]}`
shape a real Docker/podman seccomp profile JSON file uses (confirmed
directly: the real, 442-syscall upstream moby default profile,
`~/git/moby/vendor/github.com/moby/profiles/seccomp/default.json`,
parses via this project's own existing types with zero schema
changes). `oci_runtime_core::seccomp::apply` was already strict about
an unrecognized syscall name (returns a real error, not a silent
skip) — exactly the behavior a user-supplied profile should get.
**Zero lines changed in `oci-runtime-core` itself.** The only actual
gap was `synthesize_spec`'s own single, unconditional `linux.seccomp =
Some(filter_to_supported_syscalls(&default_profile()))` line, with no
way for a caller to override it.

## `resolve_seccomp`: three outcomes, matching real `podman`'s own semantics exactly

Checked directly against real podman's own `pkg/specgen/generate/
security_linux.go`/`config_linux_seccomp.go` (`~/git/podman`), not
re-derived: `--security-opt seccomp=<value>` resolves to exactly three
outcomes — no flag at all (the bundled default, filtered to this
build's own supported syscall set, unchanged from 0044);
`seccomp=unconfined` (seccomp fully disabled, `linux.seccomp = None`);
or `seccomp=<path>` (a real file, parsed via the existing spec types
and used **unfiltered** — a caller-supplied profile is presumed
already scoped to its own intended architecture, so an unknown syscall
name in it is a real, surfaced error rather than something to
silently drop the way the bundled default's rarely-relevant extras
are). `--security-opt` is accepted as a repeatable flag (matching real
`docker`/`podman`'s own CLI shape, which supports several independent
security options at once) but only the `seccomp=` key is implemented;
any other key (real `docker`/`podman` also have `apparmor=`/`label=`/
`no-new-privileges`) is rejected with a clear error.

## `--privileged` is explicitly not implemented

Real `docker run --privileged`/`podman run --privileged` disable far
more than seccomp — the full capability set, device access, and more,
none of which this project implements yet. Claiming `--privileged`
support while only touching seccomp would be misleading; it's left
out of this increment entirely rather than partially, dishonestly
implemented.

## A real capability-set discovery while verifying by hand, not assumed

Manually testing `--security-opt seccomp=unconfined` against a real
`swapon`/`sethostname` probe (mirroring 0044's own verification
technique) initially looked like it *wasn't* working — both syscalls
still failed with `Operation not permitted` even with seccomp fully
disabled. Tracing it down: this project's own rootless default
capability set (`oci_spec_types::runtime::default_capabilities()`) is
extremely minimal — `CAP_AUDIT_WRITE`/`CAP_KILL`/
`CAP_NET_BIND_SERVICE` only, versus real `docker`/`podman`'s own
~14-capability rootless default. Both probe syscalls need a capability
this project's containers simply don't have at all, independent of
seccomp — a real, separate, pre-existing gap (not touched by this
increment; flagged below), not a bug in this increment's own code.

Confirmed the fix was actually working via the single most direct,
unambiguous check available: reading the real `config.json` a
`--security-opt seccomp=unconfined` invocation actually wrote —
`linux.seccomp` is genuinely `null`. Then found a real, capability-
independent, unambiguous behavioral probe instead: a minimal custom
profile blocking only `getcwd` (a syscall every unprivileged process
can always call on its own current directory) with `SCMP_ACT_ERRNO`.
`/bin/pwd` succeeds under the ordinary default profile and fails with
exactly `Operation not permitted` under the custom one — real,
distinguishing proof that a caller-supplied profile is genuinely
loaded and enforced, not merely accepted and ignored.

## Real, automated tests

6 new unit tests in `main.rs` (`resolve_seccomp` returns the bundled
default with no flag; `unconfined` disables it; a real custom profile
file is loaded and used verbatim/unfiltered; a missing file surfaces a
clear error naming the path; an unsupported key is rejected by name;
the last of several repeated `seccomp=` values wins). 4 new integration
tests in `tests/tests/ociman_run.rs`: `unconfined` leaves `linux.
seccomp` unset in the real written `config.json`; the `getcwd`-blocking
custom profile scenario above (both the working-baseline and the
blocked case); rejecting an unsupported `--security-opt` key through
the real CLI.

## Performance

Direct git-stash A/B hyperfine comparison, real `ociman run --rm
docker.io/library/busybox:latest -- /bin/true` (no `--security-opt`
used), 30 runs each: 58.9ms before, 57.9ms after — no regression
(expected: the default, no-flag path resolves to the exact same
`filter_to_supported_syscalls(&default_profile())` call as before,
just via one more function call). `ocirun`/`oci-runtime-core`
themselves are untouched by this commit.

## What's still not here

* Every other `--security-opt` key (`apparmor=`, `label=`, `no-new-
  privileges`) and `--privileged` — both explicitly out of scope for
  this increment, as covered above.
* This project's own rootless default capability set is far more
  minimal than real `docker`/`podman`'s own (3 capabilities vs. their
  ~14) — discovered while verifying this increment by hand, a real,
  separate gap worth its own future increment, not touched here.
* `createContainer`/`startContainer` hooks, automated failed-systemd-
  scope cleanup — still untouched, same as 0056 left them.
