#!/usr/bin/env bash
# Downloads a distro's own, real, officially-published aarch64 kernel
# package (not a synthetic/self-built one) and extracts just its
# vmlinuz/Image payload, for the ocivmm hvf backend's own real-kernel
# boot tests (crates/oci-vmm/tests/hvf_boot.rs) to point
# OCIVMM_TEST_KERNEL_IMAGE at directly -- see docs/design/0249.
#
# oci_vmm::hvf::load_image() (crates/oci-vmm/src/hvf/boot.rs) already
# transparently unwraps whichever real packaging format each distro
# happens to use underneath (CentOS Stream 10's own kernel-core RPM
# ships an EFI zboot-wrapped image; Ubuntu's own linux-image .deb
# ships a bare gzip stream), so this script's only job is "get the
# real, current vmlinuz/Image file out of the real, current package
# a user would actually install" -- no distro-specific unwrapping
# here, deliberately, to avoid the two ever drifting out of sync.
#
# Always resolves whatever the *current* latest kernel package is
# (CentOS Stream 10's own "kernel-core" is a rolling stream, not a
# fixed release; Ubuntu resolves its own "linux-image-generic"
# meta-package's current dependency) rather than a hardcoded version,
# so this keeps working release after release with no manual bumps.
#
# Usage: ci/fetch-aarch64-kernel.sh <centos-stream10|ubuntu-26.04> <output-path>
set -euo pipefail

if [[ $# -ne 2 ]]; then
    echo "usage: $0 <centos-stream10|ubuntu-26.04> <output-path>" >&2
    exit 1
fi
distro="$1"
out="$2"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

case "$distro" in
centos-stream10)
    # BaseOS's own flat Packages/ directory listing has no repodata
    # index this script needs to parse separately -- just list it and
    # take the highest NEVRA (`sort -V`, real semantic-ish version
    # sort) kernel-core-*.aarch64.rpm href directly.
    base="https://mirror.stream.centos.org/10-stream/BaseOS/aarch64/os/Packages"
    rpm="$(
        curl -fsSL --retry 5 "$base/" |
            grep -oE 'href="kernel-core-[^"]*\.aarch64\.rpm"' |
            sed -E 's/href="(.*)"/\1/' |
            sort -V | tail -1
    )"
    if [[ -z "$rpm" ]]; then
        echo "fetch-aarch64-kernel: no kernel-core rpm found under $base/" >&2
        exit 1
    fi
    curl -fsSL --retry 5 -o "$work/kernel-core.rpm" "$base/$rpm"
    # A real kernel-core RPM's own vmlinuz always lives at exactly
    # this depth (one arch-specific ${uname -r} directory under
    # /lib/modules/), confirmed against the real package above -- an
    # `--include` glob extraction rather than a hardcoded path since
    # the exact ${uname -r} component changes every release. bsdtar
    # (libarchive) reads RPM payloads directly, no rpm2cpio needed;
    # this requires the real bsdtar, i.e. macOS's own bundled `tar`,
    # not GNU tar.
    bsdtar -xf "$work/kernel-core.rpm" -C "$work" --include='*/vmlinuz'
    found="$(find "$work/lib/modules" -maxdepth 2 -name vmlinuz | head -1)"
    if [[ -z "$found" ]]; then
        echo "fetch-aarch64-kernel: $rpm has no lib/modules/*/vmlinuz" >&2
        exit 1
    fi
    cp "$found" "$out"
    ;;
ubuntu-26.04)
    # Ubuntu 26.04's own real codename, confirmed live against the
    # real mirror's dists/ listing and its own Release file's
    # "Version: 26.04" field -- not guessed.
    codename="resolute"
    base="https://ports.ubuntu.com/ubuntu-ports"
    curl -fsSL --retry 5 -o "$work/Packages.gz" \
        "$base/dists/$codename/main/binary-arm64/Packages.gz"
    gunzip -f "$work/Packages.gz"
    # linux-image-generic (what a real `apt-get install
    # linux-image-generic` actually resolves to) is a meta-package
    # depending on linux-image-<version>-generic, the *signed*
    # kernel-image build -- but that one's own vmlinuz turned out to
    # be a non-zboot, seemingly-broken artifact on ports.ubuntu.com
    # specifically (confirmed directly: arm64 has no real Secure Boot
    # signing infrastructure the way amd64 does, so this looks like a
    # non-functional template of that architecture, not something
    # oci_vmm::hvf::load_image should be expected to unwrap -- see
    # crates/oci-vmm/src/hvf/boot.rs's own module docs). Resolve to
    # the concrete version this names, then substitute the *unsigned*
    # sibling package instead, which is a real, working zboot image.
    concrete="$(
        awk '/^Package: linux-image-generic$/{f=1} f && /^Depends:/{print; exit}' \
            "$work/Packages" |
            sed -E 's/Depends: linux-image-([0-9][a-zA-Z0-9.+-]*)-generic.*/\1/'
    )"
    if [[ -z "$concrete" ]]; then
        echo "fetch-aarch64-kernel: linux-image-generic not found for $codename/arm64" >&2
        exit 1
    fi
    unsigned="linux-image-unsigned-${concrete}-generic"
    deb_path="$(
        awk -v p="Package: $unsigned" '$0==p{f=1} f && /^Filename:/{print $2; exit}' \
            "$work/Packages"
    )"
    if [[ -z "$deb_path" ]]; then
        echo "fetch-aarch64-kernel: no Filename for $unsigned" >&2
        exit 1
    fi
    curl -fsSL --retry 5 -o "$work/kernel.deb" "$base/$deb_path"
    # A .deb is a plain ar(1) archive of debian-binary/control.tar.*/
    # data.tar.*, not a tar itself -- bsdtar reads the outer ar
    # container directly but (unlike the RPM case above) won't reach
    # into the inner data.tar member's own contents in one pass,
    # confirmed empirically. Two extractions: unpack the ar member,
    # then unpack that. The inner member's own compression varies by
    # package (control/data.tar.zst for the signed package above,
    # plain uncompressed control/data.tar for this unsigned one,
    # confirmed directly) -- try zst first, fall back to uncompressed.
    if bsdtar -tf "$work/kernel.deb" | grep -qx data.tar.zst; then
        bsdtar -xf "$work/kernel.deb" -C "$work" data.tar.zst
        data_member="data.tar.zst"
    else
        bsdtar -xf "$work/kernel.deb" -C "$work" data.tar
        data_member="data.tar"
    fi
    bsdtar -xf "$work/$data_member" -C "$work" --include='./boot/vmlinuz-*'
    found="$(find "$work/boot" -maxdepth 1 -name 'vmlinuz-*' | head -1)"
    if [[ -z "$found" ]]; then
        echo "fetch-aarch64-kernel: $deb_path has no boot/vmlinuz-*" >&2
        exit 1
    fi
    cp "$found" "$out"
    ;;
*)
    echo "fetch-aarch64-kernel: unknown distro '$distro' (want centos-stream10 or ubuntu-26.04)" >&2
    exit 1
    ;;
esac

echo "fetch-aarch64-kernel: wrote $distro's real, current kernel package to $out"
