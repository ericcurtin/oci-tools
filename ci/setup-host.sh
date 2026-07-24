#!/usr/bin/env bash
# Prepare a Debian/Ubuntu host (GitHub Actions runner) to run the ocivmm
# VM harness: widen /dev/kvm permissions and install passt (the
# userspace network backend every guest's virtio-net device connects
# to). Nothing else: ocivmm's VMM is statically linked (libkrun's
# crates, built like any other Rust dependency by the ordinary cargo
# build), the guests run their own distro kernels, and provisioning is
# containerized -- so no qemu, no firmware, no cloud-image tooling, no
# shared libraries, and no kernel build toolchain.
set -euo pipefail

sudo apt-get update -qq
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
    build-essential \
    passt

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
