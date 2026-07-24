#!/usr/bin/env bash
# Two callers, same script, because neither cares whether it's inside a VM:
# `ci/vm-ci.sh`, run as *root* inside the ocivmm guest (a stock distro
# OCI base image, so it must assume very little; the shim below papers
# over `sudo` not existing there yet, and the lists install it for
# everything that runs afterward, e.g. tests that spawn `sudo` and
# `ci/build-rpm.sh`'s own `sudo rpm -i`) -- and, directly,
# `.github/workflows/ci.yml`'s own `native-test` job, on the bare
# aarch64 runner `ci/native-ci.sh` builds/tests on next (a real
# `sudo`-capable Ubuntu host, where the shim never activates and every
# package below is already present or a cheap no-op).
#
# Installs the build toolchain packages for either supported guest base:
#   - CentOS Stream 10 (dnf) -- quay.io/centos/centos:stream10
#   - Ubuntu 26.04 (apt) -- docker.io/library/ubuntu:26.04
# Distro differences are data (package lists), not logic. The bare aarch64
# runner is itself always Ubuntu (whatever `ubuntu-24.04-arm` ships), so it
# always takes the `apt-get` branch below.
set -euxo pipefail

# Already root but no sudo binary yet (stock OCI base images ship none):
# make the `sudo` invocations below plain command invocations.
if [ "$(id -u)" = 0 ] && ! command -v sudo >/dev/null 2>&1; then
    sudo() { "$@"; }
fi

if command -v dnf >/dev/null 2>&1; then
    sudo dnf -y -q install \
        gcc \
        glibc-devel \
        make \
        sudo \
        tar \
        xz \
        cpio \
        findutils \
        e2fsprogs \
        erofs-utils \
        cryptsetup \
        grub2-tools \
        rpm-build
elif command -v apt-get >/dev/null 2>&1; then
    sudo apt-get update -qq
    # `apparmor` (the userspace tools, notably apparmor_parser) is
    # spelled out because the ocivmm guest starts from the bare ubuntu
    # OCI image: its own provisioned distro kernel enforces
    # `apparmor_restrict_unprivileged_userns` exactly like the old
    # cloud image's kernel did, but the cloud image shipped
    # apparmor_parser preinstalled and the OCI image doesn't — without
    # it the profile workaround below would silently skip and every
    # rootless-userns test would fail.
    sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
        apparmor \
        build-essential \
        ca-certificates \
        curl \
        sudo \
        tar \
        xz-utils \
        e2fsprogs \
        erofs-utils \
        cryptsetup-bin \
        grub-common

    # Ubuntu 24.04+ auto-confines any unconfined process that creates an
    # unprivileged user namespace into a restrictive built-in AppArmor
    # profile (`kernel.apparmor_restrict_unprivileged_userns`), which
    # denies the CAP_SYS_ADMIN check the kernel does before accepting a
    # write to the new namespace's own /proc/<pid>/uid_map — so even
    # `unshare --user --map-root-user` fails with EPERM out of the box.
    # This is a real, deliberate hardening default (not a bug) that
    # affects every rootless container runtime alike (crun, runc,
    # bubblewrap, rootless podman/docker...); real packages work around
    # it by shipping an AppArmor profile that grants their own binary
    # `userns,` under an `unconfined` flag. Do the same here, scoped to
    # the binary names this workspace actually builds, so CI exercises
    # the same rootless namespace path a real install needs to as well.
    if [ -e /proc/sys/kernel/apparmor_restrict_unprivileged_userns ] &&
        command -v apparmor_parser >/dev/null 2>&1; then
        profile=/etc/apparmor.d/oci-tools-ci-userns
        sudo tee "$profile" >/dev/null <<'EOF'
abi <abi/4.0>,
include <tunables/global>

profile oci-tools-ci-userns
    /**/target/{debug,release}/{ocirun,ociman,ocicri,ocibox,ociboot,ociboot-init}
    flags=(unconfined) {
  userns,
}
EOF
        sudo apparmor_parser -r "$profile"
    fi
else
    echo "vm-prepare: no supported package manager (need dnf or apt-get)" >&2
    exit 1
fi

echo "vm-prepare: done"
