# Design note 0218: wire DEB packaging into CI

Status: implemented
Scope: `.github/workflows/ci.yml` (`native-test` job); `packaging/README.md`.

## Why only DEB, not RPM, and why the `native-test` job specifically

`ci/build-deb.sh` (`docs/design/0217`) and `ci/build-rpm.sh`
(`docs/design/0216`) were both, until now, local-verification-only.
DEB can move to real, on-every-push CI coverage today because the
`native-test` job's own runner (`ubuntu-24.04-arm`) is genuinely
dpkg-native — the exact same real host/target match this project's
own development environment already has, and already relied on for
`ci/build-deb.sh`'s own real `sudo dpkg -i`/`--version`/`sudo dpkg -r`
round trip (confirmed identical behavior here: same package, same
runner distro family, same real install verification, not an
emulation or a narrower subset of it).

RPM stays local-only: none of this project's own current CI runners
(`ubuntu-24.04`, `ubuntu-24.04-arm`) are RPM-native, so a real `rpm -i`
there would hit the identical RPM-vs-dpkg package-database mismatch
already documented in 0216 — wiring it in would only add a weaker,
extract-and-run-only check that doesn't need CI automation to stay
correct (it's already run locally every packaging-related turn). A
real RPM-native CI job is real, separate, still-ahead follow-up work
(most likely reusing this project's own existing CentOS Stream 10 VM
harness, `ci/vm-ci.sh`/`ci/run-in-vm.sh`).

## No new `ci/vm-prepare.sh` package needed

`ci/build-deb.sh` needs `dpkg-deb` (part of the base `dpkg` package,
always present) and `dpkg-shlibdeps` (part of `dpkg-dev`). Checked
directly: `dpkg-dev` is already a transitive dependency of
`build-essential`, which `ci/vm-prepare.sh` already installs on the
`apt-get` branch for both call sites (`native-test`'s own bare runner,
and `ci/run-in-vm.sh`'s Ubuntu 26.04 guest) — confirmed with
`apt-cache depends build-essential | grep dpkg-dev` on this project's
own development host. No new package list entry, no new install step.

## Placement: an extra step in the existing job, not a new one

Added as one more step inside `native-test`, right after
`ci/native-ci.sh`'s own build/test/release-build/stage step, reusing
the same checkout, the same warmed `~/.cargo/registry` cache, and the
same runner — not a new job with its own checkout/cache/runner
overhead. `ci/build-deb.sh` does its own independent, genuinely
from-source build (a fresh `git archive` extraction, its own `cargo
build --release --locked --offline`) rather than reusing
`ci/native-ci.sh`'s already-built `target/release/`, matching this
project's own established "the packaging build must genuinely happen
from source, not by copying already-built binaries into place"
principle (0216, 0217) — the `--offline` flag still works here because
`~/.cargo/registry` (already warmed by the preceding build/test steps)
is shared across both, and `Cargo.lock` is tracked and included by
`git archive`.

The built `.deb` is also uploaded as its own artifact
(`deb-native-aarch64`), matching the existing `binaries-native-aarch64`
artifact upload's own pattern, so a real, from-CI-built package is
inspectable after every run without needing to reproduce the build
locally.
