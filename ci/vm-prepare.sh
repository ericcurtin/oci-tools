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
# needs DNS -- and both systemd-resolved (networkd path) and
# NetworkManager itself (its own dns=default/rc-manager=file, tried
# and confirmed insufficient over several real CI runs) turned out
# unreliable at actually getting a DHCP-provided nameserver into
# /etc/resolv.conf at all: sometimes it stays empty for a full minute,
# sometimes (confirmed on ubuntu-26.04) it works just long enough for
# `apt-get update` to succeed and then goes empty again before
# `apt-get install`'s own package download phase. Stop depending on
# either service's own DNS-writing behavior and write it ourselves,
# directly, from the one thing we already know is reliably true by
# this point (our own oneshot unit's `After=network-online.target`):
# a real default route exists, and passt always serves its own DNS
# proxy at that same gateway address. A no-op everywhere a working
# /etc/resolv.conf already exists (native aarch64, already-up hosts).
if ! getent hosts archive.ubuntu.com >/dev/null 2>&1 &&
    ! getent hosts mirrors.centos.org >/dev/null 2>&1; then
    gateway=""
    for _ in $(seq 1 300); do
        # `ip` doesn't even exist on the stock CentOS Stream OCI image
        # (confirmed via CI: "ip: command not found" -- iproute2 isn't
        # part of its minimal base, and we can't install it yet
        # without DNS already working). Parse /proc/net/route
        # directly instead -- always present, no external tool needed:
        # its own Destination field is "00000000" for the default
        # route, and Gateway is the IP address in hex, byte-reversed
        # (confirmed against a real route: "010011AC" -> AC.11.00.01
        # -> 172.17.0.1).
        hex=$(awk '$2 == "00000000" {print $3; exit}' /proc/net/route) || true
        if [ -n "$hex" ]; then
            gateway="$((16#${hex:6:2})).$((16#${hex:4:2})).$((16#${hex:2:2})).$((16#${hex:0:2}))"
            break
        fi
        sleep 0.2
    done
    if [ -n "$gateway" ]; then
        # /etc/resolv.conf may still be a symlink into resolved's own
        # managed stub file at this point; a plain `>` redirection
        # would write *through* that symlink instead of replacing it,
        # only for resolved's own next update cycle to silently
        # overwrite it again later. Remove it first so our own write
        # lands in a real, independent file nothing else can reclaim.
        rm -f /etc/resolv.conf
        echo "nameserver $gateway" >/etc/resolv.conf
    else
        echo "vm-prepare: no default route found; can't set up DNS" >&2
    fi
fi

if command -v dnf >/dev/null 2>&1; then
    # --setopt=retries=10: package *download* (as opposed to the
    # single mirror-metadata lookup `getent`/our own resolv.conf fix
    # already confirmed works) has shown occasional transient
    # "Could not resolve host"/timeout failures under real CI even
    # with a known-good DNS config already in place -- automatic
    # retries paper over that instead of chasing a possible packet-
    # loss-under-load issue in the underlying virtio-net path.
    sudo dnf -y -q --setopt=retries=10 install \
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
    #
    # -o Acquire::Retries=10: confirmed via CI that the package
    # *download* phase specifically (not `apt-get update`'s own single
    # mirror-metadata fetch, run moments earlier against the exact
    # same resolv.conf) can hit transient "Temporary failure resolving"
    # errors on nearly every package at once, even with a known-good
    # DNS config already in place and already working a moment before
    # -- automatic retries paper over that instead of chasing a
    # possible packet-loss-under-load issue in the underlying
    # virtio-net path.
    sudo env DEBIAN_FRONTEND=noninteractive apt-get -o Acquire::Retries=10 \
        install -y -qq --no-install-recommends \
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
