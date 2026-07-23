# Design note 0227: wire real RPM verification into the CentOS Stream 10 CI cell

Status: implemented
Scope: `ci/build-rpm.sh`; `ci/vm-prepare.sh`; `ci/vm-ci.sh`;
`ci/run-in-vm.sh`; `.github/workflows/ci.yml`.

## Closing the loop after 0224/0225

0224/0225 fixed the two real blockers a manual, ad hoc CentOS Stream 10
VM run found; 0225's own "what's still ahead" named the natural next
step directly: "wiring this into the CI VM harness for real (rather
than the ad hoc, manual verification these two sessions both did)."
This increment does exactly that — the `vm-test` matrix's own
`centos-stream10` cell (already booting a real CentOS Stream 10 guest
for its own workspace build/test) now also builds and verifies the
RPM package on every push and pull request, with a real, genuine
`rpm -i`/`--version`/`rpm -e` round trip.

## `ci/build-rpm.sh` needed to work without a real `.git` present

This project's own CI VM harness (`ci/vm.sh push`) deliberately pushes
the tree *without* `.git` at all — `ci/build-rpm.sh`'s own `git
archive HEAD` would fail outright there (confirmed directly: `git:
command not found`, since a stock CentOS Stream 10 cloud image doesn't
even have `git` installed by default). Rather than adding `git` as a
new guest package just to satisfy one command, `ci/build-rpm.sh` now
falls back to a plain, recursive `tar` copy (excluding the same
`.git`/`target`/`artifacts*` paths `ci/vm.sh push` itself already
excludes) when no real `.git` directory is present — satisfying the
identical real property `git archive` was chosen for in the first
place ("genuinely build from source, never copy an already-built
`target/`"), since `target/`/`artifacts*` were never pushed there
either.

## A real safety finding while designing the opt-in real-install flag

`ci/build-rpm.sh` needed a way to do a genuine `rpm -i`/`rpm -e` round
trip only where it's actually safe (a real RPM-native guest), not on
this project's own Ubuntu development host. Checked directly, by hand,
rather than assumed: an earlier comment in this same script claimed
"this environment's own `rpm` refuses" a real install — **false**.
`sudo rpm -i --nodeps` on this Ubuntu host silently writes every file
to its real destination path (`/usr/bin/ocirun` and so on) while never
registering the package in the (non-functional, dpkg-native host) RPM
database at all — real, genuinely orphaned files on a real system,
found and cleaned up by hand once during this same investigation. The
new `OCI_RPM_VERIFY_INSTALL=1` opt-in flag therefore does **not** just
trust the caller to only ever set it on a genuine RPM-native host: it
checks `rpm -q rpm` first (the `rpm` package itself is always real,
installed, and queryable on any actual RPM-based distro) and refuses
outright if that doesn't resolve — a real, automatic safety net,
verified directly to correctly refuse on this Ubuntu host (leaving no
residue) and to correctly proceed inside the real CentOS Stream 10 VM.

## What changed, concretely

- `ci/build-rpm.sh`: git-independent tarball fallback (above);
  `OCI_RPM_VERIFY_INSTALL=1` opt-in real install/erase round trip with
  its own automatic `rpm -q rpm` safety guard; corrected the earlier,
  factually wrong "rpm refuses" comment.
- `ci/vm-prepare.sh`: `rpm-build` added to the CentOS (`dnf`) package
  list — the one new guest package this needed (`protoc` needed no
  such addition at all, per 0224's own vendoring fix).
- `ci/vm-ci.sh`: after the existing build/test/release-build/stage
  step, detects a CentOS guest via `/etc/os-release`'s own `ID` field
  and runs `OCI_RPM_VERIFY_INSTALL=1 bash ci/build-rpm.sh`, staging the
  built RPM into `~/artifacts-rpm` for the host to pull — a real,
  honest per-base conditional, not a blanket "always attempt this and
  hope it's a no-op elsewhere."
- `ci/run-in-vm.sh`: excludes `./artifacts-rpm`/`./artifacts-deb` from
  the push too (a local dev run's own stale build output should never
  leak into a fresh VM push); pulls `artifacts-rpm` back out, but only
  after confirming the guest actually created it (the ubuntu-26.04
  cell never does, and `ci/vm.sh pull` has no graceful "source doesn't
  exist" handling of its own to rely on instead).
- `.github/workflows/ci.yml`: a new `if: matrix.base ==
  'centos-stream10'` upload-artifact step for the built RPM, alongside
  the existing (both-bases) binaries upload.

## Verified

Real, direct, hands-on verification, not just reading the scripts back
(the exact hard-earned lesson every prior blocker in this arc came
from skipping this step): booted a fresh, isolated CentOS Stream 10
aarch64 VM (this project's own `ci/vm.sh`, a dedicated scratch
`VM_DIR`/cache disk, never touching this project's own existing cached
VM state), installed `rpm-build` (no `protoc` package needed at all —
confirmed absent, confirmed unnecessary), pushed the tree *without*
`.git` (matching `run-in-vm.sh`'s own real push behavior exactly), and
ran `OCI_RPM_VERIFY_INSTALL=1 bash ci/build-rpm.sh` directly: the
git-independent tarball fallback engaged correctly, the build
succeeded, and a real `sudo rpm -i` / every CLI binary's own
`--version` from its real installed path / `sudo rpm -e` round trip
all succeeded with zero residue. Also reconfirmed on this project's own
Ubuntu development host: the default (no `OCI_RPM_VERIFY_INSTALL`)
path still behaves exactly as before, and setting the flag there
correctly refuses via the new `rpm -q rpm` safety guard, again with
zero residue. Both VM scratch directories used for these two rounds of
manual verification were torn down and removed afterward.

`python3 -c "import yaml; ..."` confirms `.github/workflows/ci.yml`
still parses; `shellcheck` on every modified script is clean (one
info-level `SC1091` note about not following a sourced `/etc/os-
release`, expected and harmless). Full workspace: `cargo build`,
`cargo test --workspace` (95/95 result blocks, 0 failures — no Rust
code changed at all this increment, purely shell/YAML), `cargo fmt
--check`, `cargo clippy --all-targets -- -D warnings`, `python3
ci/guards.py` (18 capability groups, unaffected), `cargo deny check`,
`bash ci/native-ci.sh`, `bash ci/build-deb.sh` (confirmed unaffected),
hyperfine perf sanity on `ociman run --rm` (no regression, as expected
for a packaging/CI-only change).

## What's still ahead

The actual GitHub Actions run of this new step (a real `vm-test`
matrix job, on a real GitHub-hosted x86_64 runner) can only be
confirmed once this lands on `main` and CI actually runs there — this
session's own local verification used the aarch64 combination of the
identical script/logic (`ci/run-in-vm.sh` already treats
`centos-stream10/aarch64` as a real, supported case, just not one the
current CI matrix uses), which exercises the exact same code path,
but isn't a substitute for seeing the real workflow run pass. Signing,
a real release/version-bump workflow, DEB's own already-wired-in
native-aarch64 job staying the primary dpkg verification path, and
everything else `packaging/README.md`'s own "what this doesn't do
yet" section names remain real, separate, still-ahead work.
