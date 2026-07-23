# Design note 0216: RPM packaging — milestone 8's own first slice

Status: implemented (local verification only; not wired into
`.github/workflows/ci.yml` yet)
Scope: `packaging/rpm/oci-tools.spec`; `ci/build-rpm.sh`;
`packaging/README.md`; `.gitignore`.

## Milestone 8 was completely untouched

`packaging (rpm/deb), docs polish, release workflow` — before this
increment, zero infrastructure existed for any of it. This is the
first, real, narrow slice: a genuine RPM spec that builds every one of
this project's own six binaries from source and installs them at real,
correct paths, matching this project's own "CentOS Stream 10 is a
first-class distro" pillar (Fedora/CentOS/RHEL all use RPM).

## A real, from-source build, not a "copy already-built binaries" package

`%build` runs the exact same `cargo build --release --locked
--offline` every other CI check in this project already uses (no new
build recipe invented just for packaging) against a real `git archive`
source tarball — not a package that merely repackages whatever
happens to already be sitting in `target/release/`. Verified directly:
a genuinely clean, from-scratch `rpmbuild -bb` run, starting from
nothing but the tracked git tree, produces a real, working RPM.

## `ociboot-init` installs to `/usr/libexec/oci-tools/`, not `/usr/bin/`

Matches `ociboot-init`'s own module doc comment exactly: it's meant to
be picked up by a real dracut module (`90ociboot`, still ahead —
milestone 5) and run inside the initramfs, never invoked directly by a
user. Installing it outside `$PATH`, at a location a future dracut
module's own `install` script would reference directly, matches real
`bootc`-style tooling's own established convention for this exact
kind of helper binary.

## The real RPM-vs-dpkg database mismatch this environment surfaced

A bare `rpmbuild -bb` on this project's own Ubuntu/dpkg development
environment fails its own `BuildRequires` check even though `gcc` is
genuinely installed — confirmed directly: `dpkg -l | grep gcc` shows
it installed, but `rpm -q gcc` reports "not installed", because
`rpmbuild`'s own dependency check queries the *RPM* package database
specifically, which is empty on a dpkg-native host regardless of what
`dpkg` itself has installed. `ci/build-rpm.sh` uses the standard,
real `rpmbuild --nodeps` flag to work around this for *local*
verification only — the produced package's own `BuildRequires`/
`Requires` metadata is completely unaffected and correct for a real
RPM-based system (confirmed: `rpm -qip` on the built package still
shows the real, correct `Requires:` list, computed automatically from
the actual linked binaries — `ld-linux-aarch64.so.1`, `libc.so.6`,
`libgcc_s.so.1`, `libm.so.6`, all with their own real minimum symbol
versions). The same root cause means a genuine `rpm -i` install
verification isn't meaningful on this project's own current CI runners
either (also Ubuntu) — `ci/build-rpm.sh` instead extracts the built
package directly (`rpm2cpio | cpio`) and runs every CLI binary's own
`--version`, a real, honest smoke test that doesn't depend on the
host's own package manager matching the target format at all.

## Verified by hand

`bash ci/build-rpm.sh`: a clean run from the current git tree produces
a real `.rpm` containing exactly the six binaries at their intended
paths (`rpm -qlp`), correct metadata (`rpm -qip`: name, version,
license, summary, description, real computed `Requires:`), and every
one of the five CLI binaries (`ocirun`/`ociman`/`ocicri`/`ocibox`/
`ociboot`) runs and reports its own real version string after being
extracted from the package — proving the packaged binaries are
genuinely the same, working binaries this project's own `native-ci.sh`
already builds and tests, not something packaging-specific that could
have silently diverged.

## What this doesn't do yet

DEB packaging (Ubuntu 26.04, this project's own other first-class
distro); a real RPM-native CI job that can actually `rpm -i` and run
the result end to end (most likely by reusing this project's own
existing CentOS Stream 10 VM harness, `ci/vm-ci.sh`/`ci/run-in-vm.sh`);
systemd units (`ocicri` most relevantly, a real long-lived server);
dracut module integration for `ociboot-init`; sub-packages; signing;
and a real release/version-bump workflow — all real, substantial,
still-ahead milestone-8 work, see `packaging/README.md` for the full
list.
