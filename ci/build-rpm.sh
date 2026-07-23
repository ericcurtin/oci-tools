#!/usr/bin/env bash
# Builds the real `oci-tools` RPM (packaging/rpm/oci-tools.spec) from the
# current git tree and verifies its own contents -- this project's own
# first, real packaging slice (milestone 8), local-verification-only for
# now (see packaging/README.md for exactly why this isn't wired into
# .github/workflows/ci.yml yet: a real RPM-native distro, matching this
# project's own CentOS Stream 10 target, not a bare `rpmbuild` invocation
# on whatever distro happens to be running this script).
#
# `--nodeps` (real rpmbuild flag, not a shortcut around anything the
# resulting package's own metadata omits): a bare `rpmbuild -bb` on a
# non-RPM-native host (this environment is Ubuntu/dpkg) queries the *RPM*
# package database for `BuildRequires`, which is empty here regardless of
# what's actually installed via `dpkg` -- confirmed directly: `gcc` is a
# real, installed `dpkg` package, but `rpm -q gcc` still reports "not
# installed", so a bare `rpmbuild -bb` here fails a dependency check that
# has nothing to do with whether the real prerequisite is actually
# present. The produced RPM still declares `BuildRequires: gcc` correctly
# for real RPM-based systems (`dnf builddep`/`mock` before this script
# would ever run there).
set -euxo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/.." && pwd)
cd "$repo"

name=oci-tools
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

topdir=$(mktemp -d "${TMPDIR:-/tmp}/oci-tools-rpmbuild.XXXXXX")
trap 'rm -rf "$topdir"' EXIT
mkdir -p "$topdir"/{BUILD,BUILDROOT,RPMS,SOURCES,SPECS,SRPMS}

# A real source tarball of the exact current tree (tracked files only,
# via `git archive` -- matches this project's own established "no build
# artifacts, no scratch state committed" convention), not a copy of
# `target/` or any other build-time state.
git archive --prefix="$name-$version/" -o "$topdir/SOURCES/$name-$version.tar.gz" HEAD
cp packaging/rpm/oci-tools.spec "$topdir/SPECS/"

rpmbuild --define "_topdir $topdir" -bb --nodeps "$topdir/SPECS/oci-tools.spec"

rpm_path=$(find "$topdir/RPMS" -name '*.rpm' -print -quit)
echo "built: $rpm_path"

echo "--- contents ---"
rpm -qlp "$rpm_path"
echo "--- info ---"
rpm -qip "$rpm_path"

# A real, direct extract-and-run smoke test (never a full `rpm -i`: this
# environment's own `rpm` refuses that for the identical RPM-vs-dpkg
# database mismatch `--nodeps` above already works around, and installing
# system-wide isn't necessary to prove the binaries themselves are
# correctly built/placed/executable).
extract_dir=$(mktemp -d "${TMPDIR:-/tmp}/oci-tools-rpm-extract.XXXXXX")
trap 'rm -rf "$topdir" "$extract_dir"' EXIT
(cd "$extract_dir" && rpm2cpio "$rpm_path" | cpio -idm --quiet)
for bin in ocirun ociman ocicri ocibox ociboot; do
    "$extract_dir/usr/bin/$bin" --version
done

mkdir -p "$repo/artifacts-rpm"
cp "$rpm_path" "$repo/artifacts-rpm/"
echo "build-rpm: done -- $repo/artifacts-rpm/$(basename "$rpm_path")"
