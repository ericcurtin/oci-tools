#!/usr/bin/env bash
# Prepare a Debian/Ubuntu host (GitHub Actions runner) to run the ocivmm
# VM harness: widen /dev/kvm permissions and install passt (the
# userspace network backend every guest's virtio-net device connects
# to) and e2fsprogs (`mkfs.ext4`, which builds and `ocivmm cp` loop-
# mounts a pet VM's own disk image). Nothing else: ocivmm's VMM
# (`oci-vmm`, this workspace's own KVM/virtio-pci monitor) is
# statically linked, built like any other Rust dependency by the
# ordinary cargo build; the guests run their own distro kernels, and
# provisioning is containerized -- so no qemu, no firmware, no
# cloud-image tooling, no shared libraries, and no kernel build
# toolchain.
set -euo pipefail

sudo apt-get update -qq
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
    build-essential \
    passt \
    e2fsprogs

# Ubuntu 24.04+ GitHub runners auto-confine any unconfined process
# that creates an unprivileged user namespace into a restrictive
# built-in AppArmor profile
# (`kernel.apparmor_restrict_unprivileged_userns`). passt's own
# unshare(CLONE_NEWUSER) hits this wall reproducibly (confirmed via
# strace: its socket bind() then fails EACCES -- passt doesn't check
# that return value at all and prints "socket bound" regardless --
# and a second, later unshare() for its own further sandboxing fails
# outright with EPERM). A per-binary AppArmor profile exception
# (`ci/vm-prepare.sh`'s approach for this workspace's own rootless
# binaries *inside* the guest) turned out fragile to get right here:
# passt re-execs itself into a CPU-feature-specific build
# (passt -> passt.avx2) before it ever calls unshare(), and even a
# profile correctly scoped to both names and loaded successfully
# (confirmed via aa-status) did not actually stop the kernel's
# auto-confinement from kicking in. Disable the restriction directly
# instead: this is a fresh, single-tenant, ephemeral CI VM with no
# other untrusted userns-creating workload to protect against.
if [ -e /proc/sys/kernel/apparmor_restrict_unprivileged_userns ]; then
    sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0
fi

# GitHub runners ship /dev/kvm restricted to the kvm group; make it usable
# without re-logging by widening the node (standard approach for CI
# runners). Unlike the old qemu harness there is no TCG fallback: the
# VMM is KVM-only, so a missing /dev/kvm is a hard, clearly-reported
# error rather than a silent 20x slowdown.
if [ ! -e /dev/kvm ]; then
    echo "setup-host: no /dev/kvm; ocivmm microVMs cannot run on this host" >&2
    exit 1
fi
echo 'KERNEL=="kvm", GROUP="kvm", MODE="0666", OPTIONS+="static_node=kvm"' |
    sudo tee /etc/udev/rules.d/99-kvm4all.rules >/dev/null
sudo udevadm control --reload-rules
sudo udevadm trigger --name-match=kvm || true
if [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
    echo "setup-host: KVM available"
else
    # The harness runs ocivmm under sudo anyway (see ci/run-in-vm.sh),
    # so root-only /dev/kvm access is still fine; this is informational.
    echo "setup-host: /dev/kvm present but not user-accessible (harness runs as root)"
fi
