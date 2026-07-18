#!/usr/bin/env bash
# Prepare a Debian/Ubuntu host (GitHub Actions runner) to run the VM harness:
# install QEMU + UEFI firmware + cloud-image tooling and widen /dev/kvm
# permissions when present. The harness itself degrades to TCG without KVM.
set -euo pipefail

arch=$(uname -m)

pkgs=(qemu-utils cloud-image-utils openssh-client curl ca-certificates)
case "$arch" in
    x86_64) pkgs+=(qemu-system-x86 ovmf) ;;
    aarch64) pkgs+=(qemu-system-arm qemu-efi-aarch64) ;;
    *)
        echo "setup-host: unsupported host architecture: $arch" >&2
        exit 1
        ;;
esac

sudo apt-get update -qq
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends "${pkgs[@]}"

# GitHub runners ship /dev/kvm restricted to the kvm group; make it usable
# without re-logging by widening the node (standard approach for CI runners).
if [ -e /dev/kvm ]; then
    echo 'KERNEL=="kvm", GROUP="kvm", MODE="0666", OPTIONS+="static_node=kvm"' |
        sudo tee /etc/udev/rules.d/99-kvm4all.rules >/dev/null
    sudo udevadm control --reload-rules
    sudo udevadm trigger --name-match=kvm || true
    if [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
        echo "setup-host: KVM available"
    else
        echo "setup-host: /dev/kvm present but not accessible; harness will fall back to TCG"
    fi
else
    echo "setup-host: no /dev/kvm; harness will fall back to TCG"
fi
