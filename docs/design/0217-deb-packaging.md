# Design note 0217: DEB packaging — milestone 8's second slice

Status: implemented (local verification only; not yet wired into
`.github/workflows/ci.yml`)
Scope: `packaging/deb/control`; `packaging/deb/copyright`;
`ci/build-deb.sh`; `.gitignore`.

## Same shape as the RPM slice (docs/design/0216), one real step further

Same six binaries, same install paths (`ocirun`/`ociman`/`ocicri`/
`ocibox`/`ociboot` in `$PATH`; `ociboot-init` in
`/usr/libexec/oci-tools/`, not `$PATH`, for the same reason as the RPM
slice — meant for a future dracut/initramfs-tools module to pick up,
never invoked directly), same "genuinely build from a real `git
archive` source tree via `cargo build --release --locked --offline`"
principle, same narrow, no-sub-packages, no-systemd-units, no-signing
scope.

The one real difference: this project's own development environment
*is* a genuine dpkg-native host (Ubuntu 24.04) — unlike the RPM slice,
where a real `rpm -i` was never meaningful here (see 0216). That means
`ci/build-deb.sh` can, and does, go one real step further than
`ci/build-rpm.sh`: after building the `.deb`, it does a real `sudo
dpkg -i`, runs every CLI binary's own `--version` from its actual
installed system path (`/usr/bin/ocirun`, not an extracted temp dir),
then a real `sudo dpkg -r` to remove it again. Verified directly that
this is safe and fully reversible: none of the five CLI binary names
already exist anywhere on `$PATH` on this host, and `oci-tools` wasn't
already registered with `dpkg` before this slice existed — confirmed
both before and after running the script (`dpkg -l | grep oci-tools`,
`command -v ocirun ociman ocicri ocibox ociboot`), twice, that the
host ends up in exactly the state it started in.

## `Depends:` is computed for real, not hand-maintained

`rpmbuild` computes `Requires:` automatically from the actual linked
binaries (already noted in 0216: `ld-linux-aarch64.so.1`, `libc.so.6`,
`libgcc_s.so.1`, `libm.so.6`). The dpkg-native equivalent tool,
`dpkg-shlibdeps`, exists on this host too, but is normally only
invoked as part of a full `debhelper`/`dpkg-buildpackage` source
package build — not installed or wired up here (this project's own
"narrow first slice, no new heavyweight build-time dependency"
convention, matching the RPM slice's own choice of raw `rpmbuild`
over `mock`/`dnf builddep`). Verified directly that `dpkg-shlibdeps`
still works standalone against the real staged binaries, given only a
throwaway `debian/control` stub file to satisfy its own
source-package-convention check (removed again immediately after,
before `dpkg-deb --build` runs — only the top-level, uppercase
`DEBIAN/` is meant to ship in the package itself): run against all six
real binaries together, it reports the identical, real, computed
line - `libc6 (>= 2.39), libgcc-s1 (>= 4.2)` - substituted into
`packaging/deb/control`'s `@DEPENDS@` placeholder at build time, never
hand-copied into the tracked template.

## What this doesn't do yet

Not wired into `.github/workflows/ci.yml` yet — this project's own
GitHub runners (`ubuntu-24.04`/`ubuntu-24.04-arm`) are genuinely
dpkg-native, so unlike the RPM slice there's no real host/target
mismatch blocking this; it's simply sequenced after the RPM slice and
not yet done. Same still-ahead list as 0216 otherwise: systemd units,
dracut/initramfs-tools integration, sub-packages, signing, and a real
release/version-bump workflow. See `packaging/README.md` for the full,
current list.
