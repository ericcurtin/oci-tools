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

## Why local-only for now (not wired into `.github/workflows/ci.yml`)

`rpmbuild` itself works fine on any host with `rpm-build` installed
(confirmed directly, including on this project's own Ubuntu
development environment) — the *dependency* side is the real
complication: `BuildRequires`/`Requires` are checked (and, for an
actual `rpm -i`, enforced) against the **RPM** package database
specifically. On a non-RPM-native host (Ubuntu/dpkg, both this
project's own dev environment and GitHub's own `ubuntu-24.04`/
`ubuntu-24.04-arm` CI runners), that database is empty regardless of
what's genuinely installed via `dpkg` — confirmed directly: `gcc` is a
real, installed `dpkg` package here, but `rpm -q gcc` still reports
"not installed". `ci/build-rpm.sh` works around this with a real,
standard `rpmbuild --nodeps` (only skips the *local* ad-hoc safety
check; the produced RPM's own `BuildRequires`/`Requires` metadata is
unaffected and correct for a real RPM-based system), but that same gap
means a genuine `rpm -i` install-and-run verification can't happen on
this project's own current CI runners at all — only a real CentOS
Stream 10 (or Fedora) runner could do that meaningfully. Wiring a real
RPM-native CI job (most likely reusing this project's own existing
`ci/vm-ci.sh`/`ci/run-in-vm.sh` VM harness, already booting a real
CentOS Stream 10 guest for exactly this project's own other CI checks)
is real, still-ahead follow-up work, not done in this first slice.

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
