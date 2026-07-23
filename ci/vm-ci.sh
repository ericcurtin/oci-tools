#!/usr/bin/env bash
# Runs INSIDE the CI guest VM from the pushed source tree (~/oci-tools):
# mounts the persistent cache disk, installs rustup + the pinned toolchain,
# builds and tests the whole workspace natively, and stages release binaries
# in ~/artifacts for the host to pull.
#
# Expects ci/vm-prepare.sh to have installed the distro packages already.
set -euxo pipefail

CACHE_DEV=/dev/disk/by-id/virtio-ocicache
CACHE_MNT=/mnt/cache

# --- Cache disk -------------------------------------------------------------
# The qcow2 behind this device is preserved across CI runs (actions/cache).
# It carries rustup, the cargo home, and the target dir. Best effort: any
# failure falls back to uncached paths rather than failing the job.
use_cache=0
if [ -e "$CACHE_DEV" ]; then
    if ! sudo blkid "$CACHE_DEV" >/dev/null 2>&1; then
        sudo mkfs.ext4 -q -L ocicache "$CACHE_DEV"
    fi
    sudo mkdir -p "$CACHE_MNT"
    if sudo mount "$CACHE_DEV" "$CACHE_MNT" 2>/dev/null; then
        use_cache=1
    else
        echo "vm-ci: cache disk unmountable; reformatting" >&2
        if sudo mkfs.ext4 -F -q -L ocicache "$CACHE_DEV" &&
            sudo mount "$CACHE_DEV" "$CACHE_MNT"; then
            use_cache=1
        else
            echo "vm-ci: cache disk unusable; continuing without cache" >&2
        fi
    fi
fi

if [ "$use_cache" = 1 ]; then
    sudo chown "$(id -u):$(id -g)" "$CACHE_MNT"
    export RUSTUP_HOME=$CACHE_MNT/rustup
    export CARGO_HOME=$CACHE_MNT/cargo
    export CARGO_TARGET_DIR=$CACHE_MNT/target
else
    export RUSTUP_HOME=$HOME/.rustup
    export CARGO_HOME=$HOME/.cargo
    export CARGO_TARGET_DIR=$HOME/oci-tools/target
fi
export PATH="$CARGO_HOME/bin:$PATH"

# --- Toolchain ---------------------------------------------------------------
if ! command -v rustup >/dev/null 2>&1; then
    curl -fsSL --retry 5 https://sh.rustup.rs |
        sh -s -- -y --default-toolchain none --profile minimal --no-modify-path
fi

cd "$HOME/oci-tools"

# Install the toolchain pinned by rust-toolchain.toml (components included).
# Older rustup needs the channel spelled out, hence the fallback.
if ! rustup toolchain install; then
    channel=$(sed -n 's/^channel *= *"\(.*\)"/\1/p' rust-toolchain.toml)
    rustup toolchain install "$channel" --profile minimal -c rustfmt -c clippy
fi
rustup show
cargo --version
rustc --version

# --- Build + test ------------------------------------------------------------
cargo build --workspace --locked
cargo test --workspace --locked
cargo build --workspace --release --locked

# --- Artifacts ---------------------------------------------------------------
mkdir -p "$HOME/artifacts"
for bin in ocirun ociman ocicri ocibox ociboot ociboot-init; do
    cp "$CARGO_TARGET_DIR/release/$bin" "$HOME/artifacts/"
done
"$HOME/artifacts/ociman" --version

# --- RPM packaging verification (CentOS Stream 10 only) ---------------------
# A real, RPM-native distro -- the one guest base where `ci/build-rpm.sh`'s
# own `OCI_RPM_VERIFY_INSTALL=1` (a genuine `rpm -i`/`--version`/`rpm -e`
# round trip, not just extract-and-run) is both meaningful and safe (that
# flag's own real, automatic `rpm -q rpm` safety check -- see
# ci/build-rpm.sh -- would refuse outright on the Ubuntu 26.04 cell anyway,
# but there's no reason to even attempt it there). Found and fixed two
# real, distro-specific blockers getting this far in the first place
# (`docs/design/0224`/`0225`) -- this is that same verification, now run on
# every CI push/PR rather than only by hand.
if [ -r /etc/os-release ] && (. /etc/os-release && [ "$ID" = "centos" ]); then
    echo "vm-ci: CentOS guest, also verifying RPM packaging"
    OCI_RPM_VERIFY_INSTALL=1 bash ci/build-rpm.sh
    mkdir -p "$HOME/artifacts-rpm"
    cp artifacts-rpm/*.rpm "$HOME/artifacts-rpm/"
fi

echo "vm-ci: done"
