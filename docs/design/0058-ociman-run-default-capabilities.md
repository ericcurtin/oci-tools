# Design note 0058: `ociman run`'s real `podman`-default capability set (milestone 3)

Status: implemented
Scope: `crates/oci-spec-types/src/runtime.rs` (new `podman_default_
capabilities`, `default_capabilities` untouched), `bin/ociman/src/
main.rs`, `bin/ociman/src/build.rs`.

0057's own "what's still not here" flagged this exact gap, discovered
while verifying that increment by hand: *"This project's own rootless
default capability set is far more minimal than real `docker`/
`podman`'s own (3 capabilities vs. their ~14) — ... a real, separate
gap worth its own future increment."*

## Two different "defaults", deliberately, for two different clones

`Spec::example()`'s own 3-capability set (`CAP_AUDIT_WRITE`/
`CAP_KILL`/`CAP_NET_BIND_SERVICE`) is checked directly against real,
installed `runc spec`'s own output — a deliberate choice (this
crate's own module doc, quoted in 0057's own research) so `ocirun
spec`'s output stays structurally interchangeable with real `runc`'s.
Confirmed directly this increment: `runc spec` on this same host, run
fresh, still emits exactly those 3 capabilities. **That default must
not change** — `ocirun` is a `runc` clone; changing it would break its
own byte-for-byte parity with real `runc spec`, silently regressing
`crates/oci-spec-types/tests/fixtures/runc-spec.json`'s own equality
test.

`ociman` is a different clone — of `podman` — and real `podman run`
gives every container it starts a much richer default: 11
capabilities, checked directly against `~/git/container-libs/common/
pkg/config/default.go`'s own `DefaultCapabilities`, and cross-checked
live against a real `podman run --rm alpine cat /proc/self/status`'s
own `CapEff` bitmask (podman 4.9.3). **Not** real `docker`'s own
14-capability default (the same 11 plus `CAP_MKNOD`/`CAP_NET_RAW`/
`CAP_AUDIT_WRITE`) — `ociman` is a `podman` clone, so it should match
`podman`'s own default exactly, not a different real tool's slightly
larger one.

`podman_default_capabilities()` is a new, separate, `pub` function in
`oci-spec-types::runtime` — `default_capabilities()` (private,
`Spec::example()`-only) is completely untouched. `ociman`'s own
`synthesize_spec` (`main.rs`, used by `ociman run`) and `run_step_spec`
(`build.rs`, used by `ociman build`'s own `RUN` steps — a `RUN` step
is a real container process too, not a special trusted case) both
overwrite `Spec::example()`'s own bounding/effective/permitted sets
with the richer `podman` list right after calling `into_rootless`.
`ocirun` itself is never touched by either code path.

## Real, direct kernel-truth verification, not just a unit-test assertion

Manually running a container after this change and reading its own
`/proc/self/status`: `CapEff: 00000000800405fb` — matching, byte for
byte, the exact real `podman`-reported value cited above. This is the
most direct, unambiguous proof available (the kernel's own tracked
capability state for the real running process, not a value this
project's own code merely claims to have set). Also confirmed the
change is a genuine improvement, not something that already worked:
the identical A/B comparison against the pre-change binary showed a
real capability difference is now exercisable (`chown 0:0`, a `CAP_
CHOWN`-gated operation, was blocked before this change and succeeds
after it) — though most of the 11 capabilities turned out surprisingly
hard to demonstrate with a *behaviorally distinguishing* probe in this
project's own single-uid-mapped rootless architecture (see "A related
finding" below), which is exactly why the `/proc/self/status` bitmask
check, not a probe syscall, is this increment's own primary evidence
and its only automated test.

## A related finding: several of these 11 capabilities are less useful here than in a normal rootless setup

This project's own single-uid-mapping design (only container uid 0 is
ever mapped, no subordinate range) makes `CAP_SETUID`/`CAP_SETGID`
close to inert (`setuid(2)`/`setgid(2)` to any id absent from `/proc/
<pid>/{uid,gid}_map` fails `EINVAL` regardless of the capability, and
the only mapped id is 0, which the process already runs as) — the same
observation applies in practice to `CAP_CHOWN` when attempting to
`chown` to any uid *other* than the single mapped one. This is an
inherent, already-understood consequence of the single-uid-mapping
architecture (not a new limitation introduced here, and not unsafe —
if anything, it makes these particular capabilities *less* exploitable
than in a real subuid-range rootless setup), not a reason to withhold
them: real `podman` grants the identical 11 capabilities to its own
rootless containers under the same broad tradeoff (and, notably,
today's real `podman` rootless deployments commonly *do* use subordinate
uid ranges via `/etc/subuid`, where these same capabilities are fully
meaningful — this project's own narrower, single-mapping scope is what
makes them partially inert here, not anything about the capabilities
themselves).

## Real, automated tests

1 new unit test in `oci-spec-types::runtime` confirming
`podman_default_capabilities()`'s exact 11-name content, and
explicitly confirming the 3 real-`docker`-only names it does *not*
include. 1 new integration test in `tests/tests/ociman_run.rs`
asserting the exact real `/proc/self/status` `Cap{Inh,Prm,Eff,Bnd,Amb}`
bitmask a running `ociman run` container reports — the same technique
`tests/tests/ocirun_run.rs`'s own pre-existing, untouched `run_applies_
the_default_capability_set_and_no_new_privileges` test uses for
`ocirun`'s own (smaller, unchanged) default, now diverging on purpose:
the two binaries no longer share one capability default, matching
their own two different real upstream clones. All 21 `ociman_run` and
all 11 `ocirun_run` tests pass unchanged otherwise, confirming
`ocirun`'s own runc-scaffold parity (and its own fixture-equality test)
is genuinely untouched.

## Performance

This increment adds a fixed, small (11-entry) `Vec<String>` allocation
per container start, replacing the previous 3-entry one — no loops, no
I/O, no new syscalls. Direct git-stash A/B hyperfine comparisons (real
`ociman run --rm docker.io/library/busybox:latest -- /bin/true`, 3
separate 50-88-run batches, alternating which binary ran first each
time) showed no consistent directional difference at all (each batch
flipped which binary looked "faster," within roughly one standard
deviation either way) — this project's own shared, contended dev host
produces noise (~20-25% relative stddev) far larger than any plausible
cost this specific change could add, so no regression is discernible
or plausible. `ocirun`/`oci-runtime-core` themselves are untouched.

## What's still not here

* `--cap-add`/`--cap-drop` — no CLI override for `ociman run`'s own
  new richer default yet, matching this project's own established
  "ship the default first" pattern (`--security-opt seccomp=` existed
  as an override before the default it overrides was even
  configurable at the CLI level; the same sequencing applies here).
* `createContainer`/`startContainer` hooks, automated failed-systemd-
  scope cleanup, `--privileged` — still untouched, same as 0057 left
  them.
