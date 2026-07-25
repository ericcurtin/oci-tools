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
# make the `sudo` invocations below plain command invocations. Unlike
# real sudo, this shim does *not* understand a leading `VAR=value`
# prefix as an environment assignment (found the hard way: `sudo
# DEBIAN_FRONTEND=noninteractive apt-get ...` under this shim tried to
# execute the literal string "DEBIAN_FRONTEND=noninteractive" as a
# command and failed) -- every invocation below that needs one uses
# `sudo env VAR=value cmd` instead, which is a single well-formed
# command either way.
if [ "$(id -u)" = 0 ] && ! command -v sudo >/dev/null 2>&1; then
    sudo() { "$@"; }
fi

# In the ocivmm guest specifically, this is the very first thing that
# needs DNS: reaching network-online.target (interface has an address)
# and even nss-lookup.target (a passive target nothing here actually
# gates on) both turned out insufficient in practice -- the package
# manager's very first mirror lookup failed with "Could not resolve
# host" before DHCP/DNS had actually converged (confirmed via CI:
# NetworkManager's own device state was still "connecting (getting IP
# configuration)" a full 10 seconds in). Poll for real resolution
# instead of trusting either target, for up to a minute; a no-op after
# the first `getent` success everywhere else (native aarch64,
# already-up hosts).
for _ in $(seq 1 300); do
    getent hosts mirrors.centos.org >/dev/null 2>&1 && break
    getent hosts archive.ubuntu.com >/dev/null 2>&1 && break
    sleep 0.2
done
# TEMPORARY diagnostic: if the loop above still gave up, show exactly
# what DNS configuration was actually in place, instead of guessing
# further from the package manager's own opaque curl error alone.
cat /etc/resolv.conf 2>&1 || true

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
    sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
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
