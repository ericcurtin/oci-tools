# Design note 0225: disabling the RPM debug-info subpackage

Status: implemented
Scope: `packaging/rpm/oci-tools.spec`.

## The second real, RPM-native-only issue this same VM verification found

0224 fixed the first real blocker a genuine CentOS Stream 10 VM run
of `ci/build-rpm.sh` surfaced (`protoc` unavailable). Once that was
fixed and committed, re-running the exact same verification (this
project's own existing `ci/vm.sh` harness, the same real, freshly-
provisioned guest, the fix now genuinely pulled in via a fresh `git
archive HEAD` of the committed tree) got much further — the actual
Rust build now completed successfully, `rpmbuild` computed real,
correct `Requires:` automatically from the six linked binaries — but
then failed at a new, later, genuinely different step:

```
Error while writing index for '.../usr/bin/ociboot': No debugging symbols
...
error: Empty %files file .../debugsourcefiles.list
```

## Why this never showed up on the Ubuntu development host

A bare Ubuntu `rpmbuild` (used there purely as a foreign, cross-distro
tool — Ubuntu has no distro-specific RPM macro configuration at all)
never invokes `find-debuginfo`/generates a `-debugsource`/`-debuginfo`
subpackage in the first place. A genuinely RPM-native distro (CentOS
Stream, Fedora, RHEL) does this by default, as real, standard
packaging policy: every installed ELF binary's own DWARF debug info is
extracted into a separate subpackage automatically, no explicit opt-in
needed. This is exactly the class of distro-specific behavior a
"verify only on the Ubuntu dev host" story can never surface — the
entire reason 0224/0225 both exist is running the real target distro
for the first time.

## Why it fails specifically for these binaries, not a real bug

`rustc`/`cargo`'s own DWARF debug-info emission doesn't shape into the
clean, per-source-file breakdown RPM's `find-debuginfo`/`dwz` tooling
was built around (which assumes a C/C++ toolchain's own conventional
DWARF layout) — the tool doesn't crash, it just ends up with nothing
meaningful to put in the generated `-debugsource` subpackage's own
`%files` list, and RPM treats a subpackage with a genuinely empty
`%files` list as a hard build error, not a silent no-op.

## The fix: `%global debug_package %{nil}`

A single, standard, well-known RPM spec directive that disables
automatic debug-info subpackage generation entirely. This project's
own narrow first packaging slice (0216) never had a `-debuginfo`
subpackage in scope in the first place ("no sub-packages yet") — this
isn't a workaround for a real defect, it's turning off a feature this
project doesn't use yet, the correct fix for a spec this narrowly
scoped.

## Verified

- Confirmed unaffected on the Ubuntu development host: `bash
  ci/build-rpm.sh` still builds a valid RPM and every CLI binary still
  runs correctly once extracted, exactly as before.
- **The actual point**: re-ran the full, real `ci/build-rpm.sh`
  end to end inside the same genuine CentOS Stream 10 aarch64 VM this
  session already used for 0224 — this was the one remaining failure
  after the `protoc` fix, and disabling the debug package resolves it.
  See this design note's own git history for the exact before/after
  RPM build log.
- `python3 ci/guards.py` still passes (a spec-file-only change, no
  Rust code touched).

## What's still ahead

With both real blockers this session found now fixed, a real,
authoritative RPM-native verification round trip (build the RPM
inside a genuine CentOS Stream 10 guest, and — since that guest has a
real RPM package database, unlike the Ubuntu dev host — a genuine
`rpm -i`/`--version`/`rpm -e` round trip, not just extract-and-run) is
the natural next confirmation, and wiring this into the CI VM harness
for real (rather than the ad hoc, manual verification these two
sessions both did) is real, separate, still-ahead follow-up work.
