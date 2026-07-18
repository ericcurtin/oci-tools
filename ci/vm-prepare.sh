#!/usr/bin/env bash
# Runs INSIDE the CI guest VM, streamed over ssh stdin *before* the source
# tree is pushed (so it must not assume anything beyond a stock cloud image;
# in particular it installs tar, which `vm.sh push` needs).
#
# Installs the build toolchain packages for either supported base:
#   - CentOS Stream 10 (dnf)
#   - Ubuntu 26.04 (apt)
# Distro differences are data (package lists), not logic.
set -euxo pipefail

if command -v dnf >/dev/null 2>&1; then
    sudo dnf -y -q install \
        gcc \
        glibc-devel \
        make \
        tar \
        xz \
        e2fsprogs
elif command -v apt-get >/dev/null 2>&1; then
    sudo apt-get update -qq
    sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
        build-essential \
        ca-certificates \
        curl \
        tar \
        xz-utils \
        e2fsprogs
else
    echo "vm-prepare: no supported package manager (need dnf or apt-get)" >&2
    exit 1
fi

echo "vm-prepare: done"
