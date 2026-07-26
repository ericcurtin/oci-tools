#!/usr/bin/env bash
# Prepare a Debian/Ubuntu host (GitHub Actions runner) to run the ocivmm
# VM harness: widen /dev/kvm permissions, fetch a current passt build
# (the userspace network backend every guest's virtio-net device
# connects to -- see the comment further down for why it's fetched
# directly rather than installed from the distro's own repos), and
# install e2fsprogs (`mkfs.ext4`, which builds and `ocivmm cp` loop-
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
    e2fsprogs

# NOT `apt-get install passt`: ubuntu-24.04 (this runner's own OS, not
# the guest's) ships passt 0.0~git20240220 -- a build over a year and
# a half old. Confirmed the hard way: with that package, DHCPv4 over
# passt's `--socket` (qemu stream) backend never completes at all (the
# guest gets no more than its own IPv6 link-local address, forever),
# on both the ubuntu-26.04 and centos-stream10 guest cells alike, even
# though the exact same oci-vmm/passt invocation works immediately and
# reliably on real bare-metal hardware running a *current* passt
# (Fedora 42's 20250919 build) -- strongly indicating a long-since-
# fixed bug in that specific old build rather than anything
# environment- or oci-vmm-specific. passt's own project publishes
# static, dependency-free x86_64 builds directly; fetch current passt
# from there instead of trusting whatever an LTS distro's own repos
# happen to have. Installed to /usr/local/bin, which is earlier on
# PATH than /usr/bin, so it's found first regardless of whether some
# other, older passt is also present.
sudo curl -fsSL -o /usr/local/bin/passt https://passt.top/builds/latest/x86_64/passt
sudo chmod +x /usr/local/bin/passt
passt --version

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
