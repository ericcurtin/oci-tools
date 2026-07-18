#!/usr/bin/env bash
# Host-side CI orchestration: boot the (base, arch) guest VM, build and test
# the workspace inside it, and pull the release binaries back out.
#
# Environment:
#   OCI_CI_BASE        centos-stream10 | ubuntu-26.04 (required)
#   OCI_CI_ARCH        x86_64 | aarch64 (default: host arch; must equal it —
#                      builds are always native, never cross/emulated-arch)
#   OCI_CI_IMAGE_URL   Override the cloud image URL for the cell.
#   OCI_CI_CACHE_DISK  Path of the persistent build-cache qcow2.
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

case "$base/$arch" in
    centos-stream10/x86_64)
        default_url="https://cloud.centos.org/centos/10-stream/x86_64/images/CentOS-Stream-GenericCloud-10-latest.x86_64.qcow2"
        ;;
    centos-stream10/aarch64)
        default_url="https://cloud.centos.org/centos/10-stream/aarch64/images/CentOS-Stream-GenericCloud-10-latest.aarch64.qcow2"
        ;;
    ubuntu-26.04/x86_64)
        default_url="https://cloud-images.ubuntu.com/releases/26.04/release/ubuntu-26.04-server-cloudimg-amd64.img"
        ;;
    ubuntu-26.04/aarch64)
        default_url="https://cloud-images.ubuntu.com/releases/26.04/release/ubuntu-26.04-server-cloudimg-arm64.img"
        ;;
    *)
        echo "run-in-vm: unsupported base/arch combination: $base/$arch" >&2
        exit 1
        ;;
esac

export VM_IMAGE_URL=${OCI_CI_IMAGE_URL:-$default_url}
export VM_DIR=${VM_DIR:-"$HOME/.cache/oci-tools-ci/vm"}
export VM_NAME="oci-ci-$base"
export VM_CACHE_DISK=${OCI_CI_CACHE_DISK:-"$HOME/.cache/oci-tools-ci/cache-disk.qcow2"}

vm="$here/vm.sh"

# The tree is pushed without .git; hand the hash through so --version output
# built inside the VM still embeds it.
git_hash=$(git -C "$repo" rev-parse HEAD 2>/dev/null || echo unknown)
git_hash=${git_hash:0:12}

cleanup() {
    local rc=$?
    if [ "$rc" -ne 0 ]; then
        echo "::group::VM serial console (tail)"
        tail -n 300 "$VM_DIR/console.log" 2>/dev/null || true
        echo "::endgroup::"
    fi
    "$vm" down || true
    exit "$rc"
}
trap cleanup EXIT

"$vm" up

echo "run-in-vm: preparing guest (distro packages)"
"$vm" run -- bash -s <"$here/vm-prepare.sh"

echo "run-in-vm: pushing source tree"
VM_PUSH_EXCLUDE="./.git ./target ./artifacts ./.vm-scratch" \
    "$vm" push "$repo" oci-tools

echo "run-in-vm: building and testing"
"$vm" run -- "OCI_TOOLS_GIT_HASH=$git_hash bash oci-tools/ci/vm-ci.sh"

echo "run-in-vm: pulling artifacts"
"$vm" pull artifacts "$repo/artifacts"
ls -l "$repo/artifacts"

echo "run-in-vm: success ($base/$arch)"
