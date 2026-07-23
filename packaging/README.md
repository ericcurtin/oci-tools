# Packaging

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

## What this doesn't do yet

* **DEB packaging** (Ubuntu 26.04, this project's own other first-
  class distro) — a real, separate, still-ahead increment.
* **A real RPM-native CI job** actually installing and running the
  built package end to end (see above).
* **systemd units** (most relevantly for `ocicri`, a real, long-lived
  server process — see `docs/design/0212`) and **dracut module
  integration** for `ociboot-init` (milestone 5's own still-ahead
  "dracut module" item).
* **Sub-packages** (e.g. splitting `ocicri`'s own server out, or a
  separate `-doc` package) — one single package for now, matching
  this project's own "narrow first slice" convention.
* **Signing** and a real release/version-bump workflow (also
  milestone 8's own scope, named separately: "release workflow").
