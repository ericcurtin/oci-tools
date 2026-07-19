# Design note 0069: `ociman run --privileged` (milestone 3)

Status: implemented (capabilities + seccomp — the two effects checked
directly against real source; device access and SELinux/AppArmor are
honestly out of scope, see "What's still not here")
Scope: `bin/ociman/src/main.rs` (new `--privileged` flag,
`resolve_seccomp` gains a `privileged: bool` parameter, `cmd_run`
switches its capability base between the ordinary `podman` default and
every recognized capability), `tests/tests/ociman_run.rs`.

Continues milestone 3's remaining, previously-identified gaps
(0067 shipped `--cap-add`/`--cap-drop`; this increment's own doc
comment named `--privileged` as the next one).

## Two checked, real effects — not a guess at what "privileged" should mean

Real `docker`/`podman`'s own `--privileged` is a large surface (all
capabilities, no seccomp, no AppArmor/SELinux, every host device
mounted in, no device-cgroup restriction). Rather than guessing which
subset to implement, the real vendored source was read directly:
`~/git/container-libs/common/vendor/github.com/opencontainers/
runtime-tools/generate/generate.go`'s own `SetupPrivileged` grants
every capability the runtime knows about to every one of the five
capability sets and clears `Seccomp`/`SelinuxLabel`/`ApparmorProfile`
outright; `~/git/podman/pkg/specgen/generate/security_linux.go` gates
that seccomp-clearing specifically: `s.IsPrivileged() &&
(s.SeccompProfilePath == "" || ... == SeccompDefaultPath)` — meaning
`--privileged` only forces `unconfined` when the caller didn't already
ask for a *specific* seccomp profile; an explicit one still wins. Both
of these effects have direct, already-existing primitives in this
project (`oci_runtime_core::identity::ALL_CAPABILITY_NAMES`,
`resolve_seccomp`'s own `None` branch) to sit on top of, so this
increment implements exactly these two, honestly, rather than a vaguer
approximation of the real flag's full scope.

## `--privileged` composes with `--cap-add`/`--cap-drop`, not a special case

`cmd_run` picks the *base* capability set (`ALL_CAPABILITY_NAMES` if
`--privileged`, the ordinary `podman` default otherwise) and always
runs it through the same `merge_capabilities` from 0067 — `--cap-drop`
still removes a capability from an all-capabilities base exactly like
it would from the ordinary one, and a conflicting `--cap-add`/
`--cap-drop` pair is still a real, surfaced error either way. No
special-cased "privileged short-circuits the merge" branch exists; it
only changes which list `merge_capabilities` starts from.

## `resolve_seccomp` gains one new branch, not a parallel code path

`None if privileged => Ok(None)` sits directly alongside the existing
`None => <bundled default>` arm — `--security-opt seccomp=<anything>`
being explicitly given (including `seccomp=unconfined` itself, matched
by the arm below regardless) always takes priority over `--privileged`'s
own default, matching the real source's own priority exactly, verified
by hand (see below) rather than assumed from the one-line summary of
the check.

## Real, manual end-to-end verification before writing a single automated test

Built the debug binary and ran real containers reading real
`/proc/self/status` for every combination: `--privileged` alone
produced `CapEff: 0x1ffffffffff` (all 41 recognized capabilities,
matching real `--cap-add=all`'s own already-verified 0067 bitmask
exactly) and `Seccomp: 0` (`SECCOMP_MODE_DISABLED`); `--privileged
--cap-drop=chown` produced `0x1fffffffffe` (all 41 minus `CAP_CHOWN`'s
own bit 0); `--privileged --security-opt seccomp=<a real custom
profile blocking mkdirat>` still genuinely blocked `mkdir` with a real
`EPERM` — confirmed identically with and without `--privileged`
alongside that same explicit profile, proving the explicit choice
really does override `--privileged`'s own default rather than being
silently ignored. (One real, unrelated finding while testing on this
aarch64 host: a custom seccomp profile naming the syscall `mkdir`
itself, rather than `mkdirat` — the real syscall `mkdir(1)` actually
uses on this architecture — is correctly rejected by
`oci_runtime_core::seccomp::apply`'s own strict validation with a
clear "Invalid syscall name" error; not a bug, just this increment's
own manual testing needing the architecture-correct syscall name.)

## Real, automated tests

2 new unit tests for `resolve_seccomp`'s new `privileged` parameter
(no `--security-opt` at all disables seccomp under `--privileged`; an
explicit custom profile still applies). 3 new integration tests in
`tests/tests/ociman_run.rs`, each spawning the real built binary
against a real seeded `busybox` image and reading real
`/proc/self/status` output, reproducing every bitmask/behavior verified
by hand above: `--privileged` alone; `--privileged` plus `--cap-drop`;
`--privileged` plus an explicit custom seccomp profile that still
genuinely blocks the container's own `mkdir`.

## Performance

This increment touches `bin/ociman/src/main.rs`'s own `cmd_run`/
`resolve_seccomp` (both already on the container-`run` hot path, per
this project's own established policy for when to re-verify) — the
actual new work per invocation is a handful of string
comparisons/one `Vec` clone over an at-most-41-element list, done once,
nowhere near the container-launch machinery itself
(`oci-runtime-core`/`synthesize_spec`'s own namespace/cgroup/rootfs
code is completely untouched, confirmed via `git diff --stat`). A
direct `git stash`/`git stash pop` A/B `hyperfine` comparison was run
anyway (`ocirun --version`, `ociman run --rm docker.io/library/
busybox:latest -- /bin/true`, 20+ runs each): results were noise-
dominated and within this shared host's already-documented variance
(`ociman` even measured slightly *faster* after; `ocirun`'s own binary
is bit-identical either way, since this increment never touches
`ocirun`'s own source at all) — consistent with no real regression.

## What's still not here

* Real `docker`/`podman`'s own device-related `--privileged` effects
  (mounting every host device in, disabling the device-cgroup
  restriction) — this project has no device-mounting or device-cgroup
  primitive implemented at all yet, privileged or not, so there is
  nothing for this flag to loosen there yet.
* SELinux/AppArmor labeling — neither is implemented by this project at
  all (rootless-only, no MAC framework integration yet), so
  `--privileged` has nothing to disable there either — an honest,
  narrower scope, not a silently-ignored specific.
* `createContainer`/`startContainer` hooks, automated failed-systemd-
  scope cleanup — milestone 3's other remaining gaps, unrelated to
  this increment and each large/risky enough to warrant their own
  dedicated, carefully-scoped increment rather than being folded in
  here.
