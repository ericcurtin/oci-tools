#!/usr/bin/env bash
# Runs INSIDE the ocivmm CI guest, as guest root, with the host checkout
# mounted read-write at /src (virtiofs): installs the distro packages
# (once per pet VM -- the rootfs persists across runs via the cached VM
# state, see ci/run-in-vm.sh), syncs the source into a guest-local work
# tree, installs rustup + the pinned toolchain, builds and tests the
# whole workspace natively, and drops the release binaries straight into
# /src/artifacts for the host to upload -- no ssh, no tar-over-a-socket,
# no cache disk: the pet VM's own rootfs (rustup, cargo home, target
# dir, installed packages and all) *is* the cache.
set -euxo pipefail

SRC=${OCI_CI_SRC:-/src}
WORK=$HOME/oci-tools

# --- Distro packages (once per pet VM) ----------------------------------
# Stamped with the prepare script's own hash so editing it re-runs the
# preparation in an otherwise-reused pet VM.
stamp=/var/lib/oci-tools-ci.prepared
want=$(sha256sum "$SRC/ci/vm-prepare.sh" | cut -d' ' -f1)
if [ ! -e "$stamp" ] || [ "$(cat "$stamp")" != "$want" ]; then
    bash "$SRC/ci/vm-prepare.sh"
    echo "$want" >"$stamp"
fi

# --- Source sync ---------------------------------------------------------
# Same exclusions the old ssh-push used; $WORK/target is deliberately
# left alone so incremental builds survive across runs of the pet VM.
mkdir -p "$WORK"
tar -C "$SRC" \
    --exclude=./.git \
    --exclude=./target \
    --exclude=./artifacts \
    --exclude=./artifacts-rpm \
    --exclude=./artifacts-deb \
    --exclude=./.vm-scratch \
    -cf - . | tar -C "$WORK" -xf -

export RUSTUP_HOME=$HOME/.rustup
export CARGO_HOME=$HOME/.cargo
export CARGO_TARGET_DIR=$WORK/target
export PATH="$CARGO_HOME/bin:$PATH"

# --- Toolchain -----------------------------------------------------------
if ! command -v rustup >/dev/null 2>&1; then
    curl -fsSL --retry 5 https://sh.rustup.rs |
        sh -s -- -y --default-toolchain none --profile minimal --no-modify-path
fi

cd "$WORK"

# Install the toolchain pinned by rust-toolchain.toml (components included).
# Older rustup needs the channel spelled out, hence the fallback.
if ! rustup toolchain install; then
    channel=$(sed -n 's/^channel *= *"\(.*\)"/\1/p' rust-toolchain.toml)
    rustup toolchain install "$channel" --profile minimal -c rustfmt -c clippy
fi
rustup show
cargo --version
rustc --version

# --- Build + test --------------------------------------------------------
cargo build --workspace --locked
cargo test --workspace --locked
cargo build --workspace --release --locked

# --- Artifacts (straight onto the host checkout via virtiofs) ------------
mkdir -p "$SRC/artifacts"
for bin in ocirun ociman ocicri ocibox ociboot ociboot-init ocivmm; do
    cp "$CARGO_TARGET_DIR/release/$bin" "$SRC/artifacts/"
done
"$SRC/artifacts/ociman" --version

# --- RPM packaging verification (CentOS Stream 10 only) ------------------
# A real, RPM-native distro -- the one guest base where ci/build-rpm.sh's
# own OCI_RPM_VERIFY_INSTALL=1 (a genuine rpm -i/--version/rpm -e round
# trip, not just extract-and-run) is both meaningful and safe; see
# docs/design/0224/0225/0227.
if [ -r /etc/os-release ] && (. /etc/os-release && [ "$ID" = "centos" ]); then
    echo "vm-ci: CentOS guest, also verifying RPM packaging"
    OCI_RPM_VERIFY_INSTALL=1 bash ci/build-rpm.sh
    mkdir -p "$SRC/artifacts-rpm"
    cp artifacts-rpm/*.rpm "$SRC/artifacts-rpm/"
fi

echo "vm-ci: done"
