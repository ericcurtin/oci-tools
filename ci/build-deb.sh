#!/usr/bin/env bash
# Builds the real `oci-tools` .deb (packaging/deb/control,
# packaging/deb/copyright) from the current git tree and verifies its
# own contents -- milestone 8's own second real packaging slice,
# following packaging/rpm/oci-tools.spec's own established shape (see
# docs/design/0216 and docs/design/0217).
#
# Unlike the RPM slice, this project's own development environment
# *is* a real, native dpkg host (Ubuntu 24.04), so this script goes
# one step further than ci/build-rpm.sh's own extract-and-run-only
# verification: it does a real `sudo dpkg -i`, runs every CLI binary
# from its real installed system path, then a real `sudo dpkg -r` to
# leave the host exactly as it found it.
#
# `Depends:` is computed for real, at build time, from the actual
# built binaries via `dpkg-shlibdeps` (this project's own equivalent
# of the automatic dependency generation `rpmbuild` already did for
# free in ci/build-rpm.sh) -- never hand-maintained/hardcoded.
set -euxo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/.." && pwd)
cd "$repo"

name=oci-tools
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
arch=$(dpkg --print-architecture)

srcdir=$(mktemp -d "${TMPDIR:-/tmp}/oci-tools-debbuild-src.XXXXXX")
stagedir=$(mktemp -d "${TMPDIR:-/tmp}/oci-tools-debbuild-stage.XXXXXX")
trap 'rm -rf "$srcdir" "$stagedir"' EXIT

# A real source tree of the exact current tree (tracked files only, via
# `git archive` -- matches ci/build-rpm.sh's own established "build
# genuinely happens from source" convention), not a copy of `target/`
# or any other build-time state.
git archive HEAD | tar -x -C "$srcdir"
(cd "$srcdir" && cargo build --release --locked --offline)

mkdir -p "$stagedir"/DEBIAN
mkdir -p "$stagedir"/usr/bin
mkdir -p "$stagedir"/usr/libexec/oci-tools
mkdir -p "$stagedir"/usr/share/doc/oci-tools

for bin in ocirun ociman ocicri ocibox ociboot ocivmm; do
    install -D -m 0755 "$srcdir/target/release/$bin" "$stagedir/usr/bin/$bin"
done
install -D -m 0755 "$srcdir/target/release/ociboot-init" \
    "$stagedir/usr/libexec/oci-tools/ociboot-init"
install -D -m 0644 packaging/deb/copyright "$stagedir/usr/share/doc/oci-tools/copyright"
install -D -m 0644 LICENSE "$stagedir/usr/share/doc/oci-tools/LICENSE"
install -D -m 0644 README.md "$stagedir/usr/share/doc/oci-tools/README.md"

# Real, computed dependency detection against the actual staged
# binaries -- dpkg-shlibdeps expects a `debian/control` file to be
# present relative to its own cwd (a source-package-build
# convention), which is otherwise irrelevant here: create a throwaway
# stub for this one computation, then remove it again before
# `dpkg-deb --build` (only the top-level `DEBIAN/`, uppercase, is
# meant to ship; the lowercase `debian/` stub below is not).
mkdir -p "$stagedir/debian"
touch "$stagedir/debian/control"
depends=$(cd "$stagedir" && dpkg-shlibdeps -O usr/bin/* 2>/dev/null \
    | sed -n 's/^shlibs:Depends=//p')
rm -rf "$stagedir/debian"
if [ -z "$depends" ]; then
    echo "build-deb: dpkg-shlibdeps produced no Depends: line" >&2
    exit 1
fi

sed -e "s/@VERSION@/$version/" -e "s/@ARCH@/$arch/" -e "s/@DEPENDS@/$depends/" \
    packaging/deb/control > "$stagedir/DEBIAN/control"

mkdir -p "$repo/artifacts-deb"
deb_path="$repo/artifacts-deb/${name}_${version}_${arch}.deb"
dpkg-deb --build --root-owner-group "$stagedir" "$deb_path"

echo "built: $deb_path"
echo "--- contents ---"
dpkg-deb -c "$deb_path"
echo "--- info ---"
dpkg-deb -I "$deb_path"

# A real install-and-run verification: this environment is a genuine
# dpkg-native host, so (unlike ci/build-rpm.sh's own extract-only
# verification) a real `dpkg -i`/`dpkg -r` round trip is meaningful
# here. None of ocirun/ociman/ocicri/ocibox/ociboot already exist on
# $PATH on a clean host, so this is safe and fully reversible.
sudo dpkg -i "$deb_path"
for bin in ocirun ociman ocicri ocibox ociboot ocivmm; do
    "/usr/bin/$bin" --version
done
sudo dpkg -r "$name"

echo "build-deb: done -- $deb_path"
