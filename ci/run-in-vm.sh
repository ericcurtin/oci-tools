#!/usr/bin/env bash
# Host-side CI orchestration, ocivmm edition: boot the (base, arch) guest
# as a microVM using this repo's own `ocivmm` binary (dogfooding it on
# every push/PR), build and test the workspace inside it, and pull the
# release binaries back out -- the guest *is* the distro's own OCI
# image, extracted once into a persistent pet-VM disk image, provisioned
# once by ocivmm with the distro's own kernel, initramfs, and systemd
# (installed with the distro's own dnf/apt), and booted straight into
# that systemd on every run after that -- the workspace tests therefore
# run under the real CentOS Stream 10 / Ubuntu 26.04 kernels, exactly
# like the cloud images did. The command below runs as a generated
# oneshot systemd unit whose exit status ocivmm forwards as its own.
#
# ocivmm's VMM (`oci-vmm`, this workspace's own KVM/virtio-pci monitor)
# has no filesystem-sharing device at all -- unlike the earlier
# virtiofs-based design this replaced, the pet VM's root filesystem is
# a plain ext4 disk image, and `ocivmm cp` (loop-mount based) is how
# source goes in and artifacts come out, both while the VM is stopped.
#
# What replaced what (vs. the retired qemu+cloud-init harness):
#   cloud image download (~700MB)  -> OCI image pull (~30-60MB, cached)
#   UEFI boot + cloud-init (min.)  -> direct-kernel boot to systemd (~1-2s)
#   ssh + tar push/pull            -> `ocivmm cp` (loop-mount, VM stopped)
#   qcow2 cache disk               -> the pet VM's own disk image, cached
#
# ocivmm runs as root: real distro package managers (both in the
# provisioning container and, via `ocivmm cp`'s loop-mount, whenever the
# host writes into the image) need real chown, which loop-mounted ext4
# only honors for a real root process. That's also why the cached VM
# state travels as a root-created tarball rather than a plain directory:
# the actions/cache step runs as the runner user, which couldn't read a
# root-owned ext4 image file's contents directly anyway (nor would it
# need to -- the whole image is one opaque file to actions/cache).
#
# Environment:
#   OCI_CI_BASE     centos-stream10 | ubuntu-26.04 (required)
#   OCI_CI_ARCH     x86_64 | aarch64 (default: host arch; must equal it)
#   OCI_CI_IMAGE    Override the guest OCI image reference for the cell.
#   OCI_CI_MEM_MIB  Guest RAM in MiB (default 8192).
#   OCI_CI_STATE_TAR  Path of the cached VM-state tarball.
#
# Usage: OCI_CI_BASE=ubuntu-26.04 ci/run-in-vm.sh
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/.." && pwd)

base=${OCI_CI_BASE:?OCI_CI_BASE is required (centos-stream10 | ubuntu-26.04)}
host_arch=$(uname -m)
arch=${OCI_CI_ARCH:-$host_arch}
if [ "$arch" != "$host_arch" ]; then
    echo "run-in-vm: requested arch '$arch' but host is '$host_arch';" \
        "the CI matrix always builds natively (no cross-arch emulation)" >&2
    exit 1
fi

case "$base" in
    centos-stream10)
        # CentOS's own OCI images moved to quay.io years ago; the
        # docker.io library/centos repository stops at centos:8.
        default_image="quay.io/centos/centos:stream10"
        ;;
    ubuntu-26.04)
        default_image="docker.io/library/ubuntu:26.04"
        ;;
    *)
        echo "run-in-vm: unsupported base: $base" >&2
        exit 1
        ;;
esac
image=${OCI_CI_IMAGE:-$default_image}

storage="$HOME/.cache/oci-tools-ci/storage"
state_tar=${OCI_CI_STATE_TAR:-"$HOME/.cache/oci-tools-ci/vm-state.tar"}
mem_mib=${OCI_CI_MEM_MIB:-8192}
vm_name="oci-ci-$base"

# The harness's own ocivmm (release build; the whole point is dogfooding
# it). The workflow builds this beforehand with its own cargo cache; the
# fallback here keeps the script runnable standalone.
ocivmm="$repo/target/release/ocivmm"
if [ ! -x "$ocivmm" ]; then
    echo "run-in-vm: building ocivmm"
    (cd "$repo" && cargo build --release --locked -p ocivmm)
fi

# The tree is pushed without .git; hand the hash through so --version
# output built inside the VM still embeds it.
git_hash=$(git -C "$repo" rev-parse HEAD 2>/dev/null || echo unknown)
git_hash=${git_hash:0:12}

# Restore the cached pet-VM state (image blobs + the disk image with
# everything vm-ci.sh installed/built on previous runs) -- root-owned,
# hence the tarball indirection described in the header comment.
mkdir -p "$(dirname "$state_tar")"
if [ -f "$state_tar" ]; then
    echo "run-in-vm: restoring cached VM state"
    sudo mkdir -p "$storage"
    sudo tar -C "$storage" -xf "$state_tar"
    rm -f "$state_tar"
fi

if ! sudo env OCI_TOOLS_STORAGE_ROOT="$storage" "$ocivmm" create \
    --name "$vm_name" -i "$image" 2>/dev/null; then
    echo "run-in-vm: $vm_name already exists (reusing the pet VM)"
fi

echo "run-in-vm: pushing source (excluding .git/target/artifacts)"
push_dir=$(mktemp -d)
trap 'rm -rf "$push_dir"' EXIT
tar -C "$repo" \
    --exclude=./.git --exclude=./target \
    --exclude=./artifacts --exclude=./artifacts-rpm --exclude=./artifacts-deb \
    -cf - . | tar -C "$push_dir" -xf -
sudo env OCI_TOOLS_STORAGE_ROOT="$storage" "$ocivmm" cp "$push_dir" "$vm_name:/root/oci-tools"

echo "run-in-vm: booting $vm_name"
rc=0
rm -f /tmp/passt-strace.log
sudo aa-status 2>&1 | grep -i passt || echo "run-in-vm: no passt AppArmor entries in aa-status"
sudo env OCI_TOOLS_STORAGE_ROOT="$storage" OCIVMM_PASST=ocivmm-passt-strace "$ocivmm" run \
    --mem "$mem_mib" \
    -e "OCI_TOOLS_GIT_HASH=$git_hash" \
    "$vm_name" \
    bash /root/oci-tools/ci/vm-ci.sh || rc=$?

# TEMPORARY diagnostic: see ci/setup-host.sh's ocivmm-passt-strace
# wrapper. Remove both once passt's real failure mode here is found.
if [ "$rc" -ne 0 ] && [ -f /tmp/passt-strace.log ]; then
    echo "run-in-vm: passt strace log follows"
    sudo cat /tmp/passt-strace.log
fi

echo "run-in-vm: pulling artifacts"
mkdir -p "$repo/artifacts"
sudo env OCI_TOOLS_STORAGE_ROOT="$storage" "$ocivmm" cp \
    "$vm_name:/root/oci-tools/artifacts" "$repo/artifacts" || true
sudo chown -R "$(id -u):$(id -g)" "$repo/artifacts"

# Only the centos-stream10 cell's own vm-ci.sh ever creates this.
if [ "$base" = centos-stream10 ]; then
    mkdir -p "$repo/artifacts-rpm"
    sudo env OCI_TOOLS_STORAGE_ROOT="$storage" "$ocivmm" cp \
        "$vm_name:/root/oci-tools/artifacts-rpm" "$repo/artifacts-rpm" || true
    sudo chown -R "$(id -u):$(id -g)" "$repo/artifacts-rpm"
fi

if [ "$rc" -ne 0 ]; then
    echo "run-in-vm: guest CI failed (exit $rc)" >&2
    exit "$rc"
fi

# Pack the (root-owned) VM state for actions/cache to save: pet-VM reuse
# is exactly what makes warm runs fast (no pull, no dnf/apt, no rustup,
# incremental cargo target -- all of it lives inside the disk image).
echo "run-in-vm: packing VM state for the cache"
sudo tar -C "$storage" -cf "$state_tar.tmp" .
sudo chown "$(id -u):$(id -g)" "$state_tar.tmp"
mv "$state_tar.tmp" "$state_tar"

ls -l "$repo/artifacts"
echo "run-in-vm: success ($base/$arch)"
