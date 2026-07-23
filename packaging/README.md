# Packaging

## RPM

Milestone 8's own first real slice: `packaging/rpm/oci-tools.spec`, a
real RPM spec that builds every one of this project's own six
binaries from source (`cargo build --release --locked --offline`,
the exact same build every other CI check in this project already
uses) and installs them:

* `ocirun`/`ociman`/`ocicri`/`ocibox`/`ociboot` → `/usr/bin/`
* `ociboot-init` → `/usr/libexec/oci-tools/` (not `/usr/bin/`: real
  `ociboot-init` is meant to be picked up by the *initramfs*, via a
  real dracut module — still ahead, milestone 5 — not invoked
  directly by a user; this location is where that future module would
  find it, matching real `bootc`-style tooling's own convention of
  keeping an initramfs helper binary outside the normal `$PATH`)

Verify locally with `ci/build-rpm.sh`: builds the spec (a real
`git archive` of the current tree as the source tarball, so the RPM's
own build genuinely happens from source, not by copying already-built
binaries into place), inspects the result (`rpm -qlp`/`rpm -qip`), and
extracts + runs every CLI binary's own `--version` as a real smoke
test.

## Why plain `rpmbuild -bb --nodeps` on a non-RPM-native host

`rpmbuild` itself works fine on any host with `rpm-build` installed
(confirmed directly, including on this project's own Ubuntu
development environment) — the *dependency* side is the real
complication: `BuildRequires` is checked against the **RPM** package
database specifically. On a non-RPM-native host (Ubuntu/dpkg, this
project's own dev environment), that database is empty regardless of
what's genuinely installed via `dpkg` — confirmed directly: `gcc` is a
real, installed `dpkg` package here, but `rpm -q gcc` still reports
"not installed". `ci/build-rpm.sh` works around this with a real,
standard `rpmbuild --nodeps` (only skips the *local* ad-hoc safety
check; the produced RPM's own `BuildRequires`/`Requires` metadata is
unaffected and correct for a real RPM-based system) — the produced
RPM package's own content and metadata are unaffected either way.

## Now wired into CI for real (`docs/design/0227`)

The `vm-test` matrix's own `centos-stream10` cell (`.github/workflows/
ci.yml`) — a real CentOS Stream 10 guest, already booted there for its
own workspace build/test — now also runs `ci/build-rpm.sh` with
`OCI_RPM_VERIFY_INSTALL=1` (via `ci/vm-ci.sh`), on every push and pull
request: a real `sudo rpm -i`, every CLI binary's own `--version` from
its real installed path, and a real `sudo rpm -e` leaving no residue —
not just extract-and-run. That flag has its own real, automatic safety
guard (`rpm -q rpm` must already resolve) so it can never be set by
mistake on a non-RPM-native host and risk writing untracked files
there — checked directly, the hard way: an earlier draft of this
project's own assumption that `rpm -i` "refuses" on a non-RPM-native
host was wrong (see `docs/design/0227`). The `ubuntu-26.04` cell never
attempts this at all (RPM packaging is CentOS-specific); the built RPM
is uploaded as its own CI artifact only for the `centos-stream10`
cell.

## Verified for real, once by hand, then wired into CI for good

Manually verified end to end (`docs/design/0224`/`0225`) using this
project's own existing `ci/vm.sh` harness, booting a real CentOS
Stream 10 aarch64 guest (the same base `ci/run-in-vm.sh` already uses
for its own CI cell): a real `bash ci/build-rpm.sh` run, a genuine
`sudo rpm -i` install (not just extract-and-run — this guest has a
real, non-empty RPM package database), every CLI binary running
correctly from its real installed `/usr/bin/` path, `rpm -q`/`rpm -ql`
confirming real package metadata, and a clean `sudo rpm -e` removal
leaving no residue. Two real, previously-undiscovered blockers found
and fixed in the process, neither of which the Ubuntu development
host could ever have surfaced:

* `protoc` isn't dnf-installable on CentOS Stream 10 at all, not even
  via EPEL — fixed by vendoring it (`protoc-bin-vendored`,
  `docs/design/0224`), removing the host dependency everywhere, not
  just for this one distro.
* A real RPM-native distro auto-generates a `-debugsource`/`-debuginfo`
  subpackage by default, which failed outright for these Rust
  binaries' own DWARF shape — fixed with the standard `%global debug_
  package %{nil}` directive (`docs/design/0225`).

Both blockers had to be fixed before a real, repeatable, wired-into-CI
version of this same verification became possible at all — see above
for that (`docs/design/0227`), now live in `.github/workflows/ci.yml`
itself rather than only ever run by hand.

## DEB

Milestone 8's own second real slice: `packaging/deb/control` (a
template, `@VERSION@`/`@ARCH@`/`@DEPENDS@` substituted at build time)
and `packaging/deb/copyright`, installing the exact same six binaries
at the exact same paths as the RPM slice above.

Verify locally with `ci/build-deb.sh`: builds from a real `git
archive` of the current tree (same "genuinely from source" rationale
as `ci/build-rpm.sh`), stages the six binaries plus `LICENSE`/
`README.md`/`copyright` under `usr/share/doc/oci-tools/`, computes a
real `Depends:` line via `dpkg-shlibdeps` run directly against the
staged binaries (this project's own hand-rolled equivalent of the
automatic dependency generation `rpmbuild` already did for free in the
RPM slice — confirmed identical across all six binaries: `libc6 (>=
2.39), libgcc-s1 (>= 4.2)`, never hand-maintained/hardcoded), builds
the `.deb` with `dpkg-deb --build --root-owner-group`, and — since
this project's own development environment genuinely *is* a dpkg-
native host (Ubuntu 24.04), unlike the RPM slice's own host/target
mismatch — goes one step further with a real `sudo dpkg -i`, runs
every CLI binary's own `--version` from its real installed system
path, then a real `sudo dpkg -r` to leave the host exactly as it found
it. Verified directly, twice: a clean install, a correct `--version`
from every one of the five CLI binaries, a clean removal (confirmed
via `dpkg -l`/`command -v` afterward — no residue).

Now wired into `.github/workflows/ci.yml`'s own `native-test` job
(milestone 8's own third real slice, `docs/design/0218`): since that
runner is genuinely dpkg-native (`ubuntu-24.04-arm`, the same as this
project's own development host), the real `sudo dpkg -i`/`dpkg -r`
round trip described above runs for real, on every push and pull
request, immediately after `ci/native-ci.sh`'s own build/test — not
just locally on demand. `dpkg-dev` (the package providing
`dpkg-shlibdeps`) needed no new `ci/vm-prepare.sh` entry: it's already
a transitive dependency of `build-essential`, which that script
already installs.

## What this doesn't do yet

* **A real RPM-native CI job** actually installing and running the
  built RPM end to end (see above) — DEB is now wired in (see above),
  RPM still isn't; it needs a real RPM-native runner (most likely this
  project's own existing CentOS Stream 10 VM harness), not the bare
  dpkg-native `native-test` runner DEB now uses.
* **systemd units** (most relevantly for `ocicri`, a real, long-lived
  server process — see `docs/design/0212`) and **dracut/
  initramfs-tools integration** for `ociboot-init` (milestone 5's own
  still-ahead "dracut module" item).
* **Sub-packages** (e.g. splitting `ocicri`'s own server out, or a
  separate `-doc` package) — one single package per format for now,
  matching this project's own "narrow first slice" convention.
* **Signing** and a real release/version-bump workflow (also
  milestone 8's own scope, named separately: "release workflow").
