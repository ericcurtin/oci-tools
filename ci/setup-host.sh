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
    strace \
    e2fsprogs

# TEMPORARY diagnostic: passt is still dying at startup even after
# widening vm_dir; wrap it in strace so the next failed run's log
# shows exactly which syscall/event is responsible now.
# ci/run-in-vm.sh points OCIVMM_PASST at this wrapper and cats the
# trace on failure.
sudo tee /usr/local/bin/ocivmm-passt-strace >/dev/null <<'EOF'
#!/bin/sh
exec strace -f -tt -s 200 -o /tmp/passt-strace.log /usr/bin/passt "$@"
EOF
sudo chmod +x /usr/local/bin/ocivmm-passt-strace

# Ubuntu 24.04+ GitHub runners auto-confine any unconfined process that
# creates an unprivileged user namespace into a restrictive built-in
# AppArmor profile (`kernel.apparmor_restrict_unprivileged_userns`) --
# the same hardening default `ci/vm-prepare.sh` already works around
# for this workspace's own rootless-userns binaries *inside* the guest,
# but this one is on the *host* side: passt's own privilege-drop (to
# "nobody") plus internal namespace isolation hits this exact wall,
# reproducibly, before it can even accept its first connection. Grant
# passt the same `userns,` exception.
if [ -e /proc/sys/kernel/apparmor_restrict_unprivileged_userns ]; then
    profile=/etc/apparmor.d/oci-tools-ci-passt-userns
    sudo tee "$profile" >/dev/null <<'EOF'
abi <abi/4.0>,
include <tunables/global>

profile oci-tools-ci-passt-userns /usr/bin/passt flags=(unconfined) {
  userns,
}
EOF
    sudo apparmor_parser -r "$profile"
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
