#!/usr/bin/env bash
# Builds the real `oci-tools` RPM (packaging/rpm/oci-tools.spec) from the
# current tree and verifies its own contents -- milestone 8's own
# packaging slice. Safe and meaningful on any host (this project's own
# Ubuntu/dpkg development host, or a real RPM-native guest, e.g. this
# project's own CentOS Stream 10 CI VM cell -- see below for the one real
# extra step that only happens there).
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
#
# `OCI_RPM_VERIFY_INSTALL=1`: also does a real `rpm -i`/`--version`/
# `rpm -e` round trip, not just extract-and-run -- opt-in, and only ever
# meant to be set on a genuine RPM-native host (e.g. inside
# `ci/vm-ci.sh`'s own CentOS Stream 10 branch). Checked directly, the
# hard way, on this project's own Ubuntu development host: `rpm -i
# --nodeps` there does *not* cleanly refuse (an earlier version of this
# comment assumed it would) -- it silently writes every file to its real
# destination path (`/usr/bin/ocirun` and so on) while never actually
# registering the package in the (non-functional, dpkg-native host) RPM
# database at all, so a later `rpm -e`/`rpm -q` can't find or remove
# anything it just wrote -- real, orphaned files on a real system, found
# and manually cleaned up by hand once, not something this script must
# ever risk doing to a caller's own host again by default.
set -euxo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/.." && pwd)
cd "$repo"

name=oci-tools
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

topdir=$(mktemp -d "${TMPDIR:-/tmp}/oci-tools-rpmbuild.XXXXXX")
extract_dir=$(mktemp -d "${TMPDIR:-/tmp}/oci-tools-rpm-extract.XXXXXX")
trap 'rm -rf "$topdir" "$extract_dir"' EXIT
mkdir -p "$topdir"/{BUILD,BUILDROOT,RPMS,SOURCES,SPECS,SRPMS}

# A real source tarball of the exact current tree (tracked files only,
# via `git archive` when a real `.git` is present -- matches this
# project's own established "no build artifacts, no scratch state
# committed" convention), not a copy of `target/` or any other build-time
# state. This project's own CI VM harness (`ci/vm-ci.sh`'s source sync)
# deliberately syncs the tree *without* `.git` at all -- `target`/
# `artifacts*` are never synced there either, so a plain recursive copy
# (excluding those same paths) satisfies the identical real property
# that matters here just as well when there's no `.git` to archive from.
if git -C "$repo" rev-parse --git-dir >/dev/null 2>&1; then
    git archive --prefix="$name-$version/" -o "$topdir/SOURCES/$name-$version.tar.gz" HEAD
else
    workdir="$topdir/SOURCES/$name-$version"
    mkdir -p "$workdir"
    tar --exclude=./.git --exclude=./target --exclude=./artifacts \
        --exclude=./artifacts-rpm --exclude=./artifacts-deb -cf - . |
        tar -C "$workdir" -xf -
    tar -czf "$topdir/SOURCES/$name-$version.tar.gz" -C "$topdir/SOURCES" "$name-$version"
    rm -rf "$workdir"
fi
cp packaging/rpm/oci-tools.spec "$topdir/SPECS/"

rpmbuild --define "_topdir $topdir" -bb --nodeps "$topdir/SPECS/oci-tools.spec"

rpm_path=$(find "$topdir/RPMS" -name '*.rpm' -print -quit)
echo "built: $rpm_path"

echo "--- contents ---"
rpm -qlp "$rpm_path"
echo "--- info ---"
rpm -qip "$rpm_path"

# A real, direct extract-and-run smoke test -- safe and meaningful on
# any host regardless of whether `OCI_RPM_VERIFY_INSTALL` below also
# runs.
(cd "$extract_dir" && rpm2cpio "$rpm_path" | cpio -idm --quiet)
for bin in ocirun ociman ocicri ocibox ociboot ocivmm; do
    "$extract_dir/usr/bin/$bin" --version
done

if [ "${OCI_RPM_VERIFY_INSTALL:-0}" = 1 ]; then
    # A real, automatic safety net, not just trusting the caller to only
    # ever set this on a genuine RPM-native host: `rpm` is always a
    # real, installed package on any actual RPM-based distro, so `rpm -q
    # rpm` failing here means this host's own RPM database isn't
    # functional (the exact Ubuntu/dpkg case the doc comment above
    # warns about) -- refuse outright rather than risk repeating the
    # same real, orphaned-file mistake found and cleaned up by hand.
    if ! rpm -q rpm >/dev/null 2>&1; then
        echo "build-rpm: OCI_RPM_VERIFY_INSTALL=1 requires a genuine RPM-native host (rpm -q rpm doesn't even resolve here); refusing rather than risk writing untracked files" >&2
        exit 1
    fi
    echo "build-rpm: OCI_RPM_VERIFY_INSTALL=1, doing a real rpm -i/rpm -e round trip"
    sudo rpm -i "$rpm_path"
    rpm -q "$name"
    for bin in ocirun ociman ocicri ocibox ociboot ocivmm; do
        "/usr/bin/$bin" --version
    done
    sudo rpm -e "$name"
    if rpm -q "$name" >/dev/null 2>&1; then
        echo "build-rpm: $name still shows installed after rpm -e" >&2
        exit 1
    fi
    echo "build-rpm: real rpm -i/rpm -e round trip succeeded and left no residue"
fi

mkdir -p "$repo/artifacts-rpm"
cp "$rpm_path" "$repo/artifacts-rpm/"
echo "build-rpm: done -- $repo/artifacts-rpm/$(basename "$rpm_path")"
