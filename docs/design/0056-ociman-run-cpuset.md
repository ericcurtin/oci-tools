# Design note 0056: `ociman run --cpuset-cpus`/`--cpuset-mems` (milestone 3)

Status: implemented, with a real, honestly-documented limitation (see
"A real limitation, found by hand, not shipped silently" below)
Scope: `crates/oci-runtime-core/src/cgroups.rs` (new `cpuset.cpus`/
`cpuset.mems` writes, new `cpuset_string_to_bitmask`),
`crates/oci-runtime-core/src/systemd_cgroup.rs` (`AllowedCPUs`/
`AllowedMemoryNodes`), `bin/ociman/src/main.rs`,
`tests/tests/ociman_run.rs`.

0055's own "what's still not here" named this exact gap, deliberately
deferred from that same increment: *"a larger, separate increment ...
the systemd driver's own equivalent additionally needs a real
range-list-to-bitmask conversion ... not just a value pass-through."*

## The cgroupfs driver: a two-line addition, no conversion needed

`LinuxCpu.cpus`/`.mems` already existed in `oci-spec-types` (their own
doc comments already said "not yet translated to a cgroup write").
`cgroups::plan_cpu` now writes them straight through to `cpuset.cpus`/
`cpuset.mems` — unlike every other write in this function, the
runtime-spec's own string is already exactly what the cgroupfs
interface file expects, matching real `crun`'s own
`write_cpuset_resources` (`~/git/crun/src/libcrun/cgroup-resources.c`),
checked directly.

## The systemd driver: a real bitmask conversion, ported directly from real `crun`

`AllowedCPUs`/`AllowedMemoryNodes` are the only two systemd resource
properties in this whole codebase that aren't a plain integer — their
real D-Bus signature is `ay` (a byte-array bitmask), not the human-
readable range-list string cgroupfs itself accepts. `cpuset_string_
to_bitmask` (`cgroups.rs`, shared with `systemd_cgroup.rs` the same way
`convert_memory_swap_to_v2`/`convert_cpu_shares_to_weight` already are)
is ported directly from real `crun`'s own `cpuset_string_to_bitmask`
(`~/git/crun/src/libcrun/utils.c`): bit `i` lives in byte `i / 8`, at
position `1 << (i % 8)` within it — not guessed, not re-derived from
documentation. An unparseable string is tolerated (the property is
skipped, not a hard error), matching this same function's own existing
stance on an unconvertible `--memory`/`--memory-swap` pair.

## A real limitation, found by hand, not shipped silently

Manually running the very first real `--cpuset-cpus 0-1` invocation
(before writing any automated test, per this project's own established
practice) revealed something the unit tests alone could never catch:
`systemctl --user show <scope> -p AllowedCPUs` correctly reported
`0-1` — the property genuinely reaches systemd with the right value —
but `-p EffectiveCPUs` came back **empty**, and the scope's own real
`cpuset.cpus` cgroupfs file never even got created. Cross-checked
directly against the *already-shipped* `--memory`/`--cpus` flags on the
same host: `MemoryMax`/`CPUQuotaPerSecUSec` both cause systemd to
reliably enable the `memory`/`cpu` controllers all the way down through
the rootless `app.slice`/`user@.service` hierarchy to the scope (their
own real cgroupfs files, `memory.max`/`cpu.max`, genuinely appear with
the right values) — `cpuset` simply does not get enabled the same way,
even when combined with a working `--cpus` in the same invocation.
`man systemd.resource-control` itself warns exactly this: *"Setting
AllowedCPUs=... doesn't guarantee that all of the CPUs will be used by
the processes as it may be limited by parent units."*

This is a real, structural rootless-`systemd --user`-session limitation
for the `cpuset` controller specifically — not a bug in this
increment's own code (the property value is provably correct; systemd
receives and stores it faithfully) and not unique to this project
either (rootless CPU pinning is a widely-acknowledged real-world rough
edge for container tooling generally). Shipping the flags anyway, with
this limitation documented prominently in three places (the systemd-
driver's own doc comment, the CLI's own `--help` text, and this note),
is the honest choice: the property is still set correctly (so a host
that *does* delegate `cpuset` — a more privileged systemd configuration
than this project's own default rootless setup — benefits from it
immediately, with no code changes needed), and a user who *does* rely
on it for real isolation is told plainly, in the flag's own help text,
not to assume it works. Silently shipping this as if fully functional,
after having personally confirmed it usually isn't on a typical host,
would have been the dishonest choice.

## Real, automated tests

7 new unit tests: `cgroups.rs` gains 2 (`cpuset.cpus`/`.mems` written
verbatim; absent when unset) plus 5 for `cpuset_string_to_bitmask`
itself (single bits, a range, spanning multiple bytes, the exact real
fixture value `"0-1"` this crate's own `matches_real_runc_fixture_
resources` test already uses, and rejecting garbage) — the existing
fixture test's own expected-writes list was updated to include the new
`cpuset.cpus` write the fixture's own `"cpus": "0-1"` now produces.
`systemd_cgroup.rs` gains 2 (translating a real range list into the
correct bitmask bytes; tolerating an unparseable one). `main.rs` gains
3 (`LinuxCpu` built correctly with no quota at all when only cpuset
flags are given; combined with `--cpus`; the "something was given"
check correctly considers the new flags too). `tests/tests/ociman_
run.rs` gains 1 integration test — deliberately scoped to check *only*
that the real systemd scope's own `AllowedCPUs`/`AllowedMemoryNodes`
properties are set correctly (the same technique the existing `--cpus`
test already uses for a property this project can't cleanly prove
kernel-level enforcement for either), not a claim of real CPU pinning
this note's own findings above show isn't reliably true.

## Real, benchmark re-verification (this touches both cgroup drivers)

Direct git-stash A/B hyperfine comparisons, no new flags used: real
`ociman run --rm docker.io/library/busybox:latest -- /bin/true` (30
runs each, exercises the systemd driver): 46.1ms before, 42.6ms after.
Real `ocirun run` against a plain bundle (30 runs each, exercises the
cgroupfs driver via `CgroupSetup::FromSpec`): 3.3ms before, 3.0ms
after. No regression either way (both within this project's own
established noise band) — expected, since neither driver does any new
work when `cpu.cpus`/`.mems` are empty (the default, unchanged case).

## What's still not here

* A real fix for the rootless `cpuset` delegation gap above — would
  need either a documented, working D-Bus incantation this project
  hasn't found yet, or accepting that genuine CPU pinning under
  rootless `ociman run` may need a more privileged host configuration
  than this project's own default targets.
* `createContainer`/`startContainer` hooks, a custom/opt-out seccomp
  profile, automated failed-systemd-scope cleanup — still untouched,
  same as 0055 left them.
