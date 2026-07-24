#!/usr/bin/env bash
# Runs directly on a *native* CI runner (bare metal, no nested VM at all):
# installs the pinned toolchain, builds and tests the whole workspace, and
# stages release binaries in ./artifacts -- the aarch64 counterpart to
# ci/run-in-vm.sh's own nested-VM harness, used instead of it for exactly
# one reason: GitHub's own hosted aarch64 runners have no /dev/kvm at all
# (see ci/setup-host.sh's own comment), so that harness's guest VMs would
# only ever boot under TCG there -- slow, and (found the hard way, see the
# 2026-07-20 CI investigation this script's own commit is part of) prone to
# surfacing real per-architecture bugs' *symptoms* (namely, tests timing
# out or racing that would otherwise pass) alongside the real bugs
# themselves, muddying which is which. This project's own code doesn't
# need a *different guest distro* to get real aarch64 architecture
# coverage, just a real aarch64 CPU -- which the runner itself already is,
# no VM required. The x86_64 cells (ci/run-in-vm.sh, real KVM acceleration
# when the runner's own host happens to expose it) still cover both
# centos-stream10 and ubuntu-26.04 as distinct guest distros; this only
# ever runs as whatever distro GitHub's own aarch64 runner image ships.
#
# Expects ci/vm-prepare.sh to have installed the distro packages already
# (it's plain package installation plus the rootless-userns AppArmor
# profile fix, neither of which cares whether it's running inside a VM or
# directly on the runner).
#
# Persistent caching (cargo registry/git, target/) is the calling
# workflow's own job, via actions/cache -- unlike ci/vm-ci.sh, which
# carries its own virtio cache-disk mounting logic because a fresh VM has
# nothing else to persist state across runs with, this script runs
# directly on the already-persistent-per-job runner filesystem and needs
# no cache-disk equivalent of its own.
set -euxo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/.." && pwd)
cd "$repo"

# GitHub's own runner images ship rustup preinstalled; this is only a
# fallback for anywhere that isn't true (matches ci/vm-ci.sh's identical
# check, needed there since a bare cloud image never has it).
if ! command -v rustup >/dev/null 2>&1; then
    curl -fsSL --retry 5 https://sh.rustup.rs |
        sh -s -- -y --default-toolchain none --profile minimal --no-modify-path
    export PATH="$HOME/.cargo/bin:$PATH"
fi

# Install the toolchain pinned by rust-toolchain.toml (components included).
# Older rustup needs the channel spelled out, hence the fallback.
if ! rustup toolchain install; then
    channel=$(sed -n 's/^channel *= *"\(.*\)"/\1/p' rust-toolchain.toml)
    rustup toolchain install "$channel" --profile minimal -c rustfmt -c clippy
fi
rustup show
cargo --version
rustc --version

cargo build --workspace --locked
cargo test --workspace --locked
cargo build --workspace --release --locked

mkdir -p "$repo/artifacts"
for bin in ocirun ociman ocicri ocibox ociboot ociboot-init ocivmm; do
    cp "target/release/$bin" "$repo/artifacts/"
done
"$repo/artifacts/ociman" --version

echo "native-ci: done"
