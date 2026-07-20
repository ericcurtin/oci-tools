#!/usr/bin/env bash
# Two callers, same script, because neither cares whether it's inside a VM:
# `ci/run-in-vm.sh`, streamed over ssh stdin *before* the source tree is
# pushed into a guest VM (so it must not assume anything beyond a stock
# cloud image there; in particular it installs tar, which `vm.sh push`
# needs) -- and, directly, `.github/workflows/ci.yml`'s own `native-test`
# job, on the bare aarch64 runner `ci/native-ci.sh` builds/tests on next
# (a real `sudo`-capable Ubuntu host already has `tar`; installing it again
# is simply a no-op there).
#
# Installs the build toolchain packages for either supported guest-VM base:
#   - CentOS Stream 10 (dnf)
#   - Ubuntu 26.04 (apt)
# Distro differences are data (package lists), not logic. The bare aarch64
# runner is itself always Ubuntu (whatever `ubuntu-24.04-arm` ships), so it
# always takes the `apt-get` branch below.
set -euxo pipefail

if command -v dnf >/dev/null 2>&1; then
    sudo dnf -y -q install \
        gcc \
        glibc-devel \
        make \
        tar \
        xz \
        e2fsprogs \
        erofs-utils \
        cryptsetup \
        grub2-tools
elif command -v apt-get >/dev/null 2>&1; then
    sudo apt-get update -qq
    sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
        build-essential \
        ca-certificates \
        curl \
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
