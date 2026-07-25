#!/usr/bin/env bash
# Runs INSIDE the ocivmm CI guest, as guest root, as the command of a
# generated oneshot systemd unit (see ci/run-in-vm.sh): installs the
# distro packages (once per pet VM -- both the rootfs and this stamp
# persist inside the disk image across runs, see below), builds and
# tests the whole workspace, and stages the release binaries in
# ~/oci-tools/artifacts for the host to pull out via `ocivmm cp`.
#
# `ci/run-in-vm.sh` has already pushed a fresh checkout straight onto
# `~/oci-tools` via `ocivmm cp` (loop-mount, VM stopped) before this
# unit ever runs -- overlaid onto whatever was there from the pet VM's
# previous run, so `~/oci-tools/target` (this script's own build
# output) survives untouched between runs: the disk image itself *is*
# the build cache now, no separate cache disk needed.
set -euxo pipefail

WORK=$HOME/oci-tools
cd "$WORK"

# --- Distro packages (once per pet VM) ----------------------------------
# Stamped with the prepare script's own hash so editing it re-runs the
# preparation in an otherwise-reused pet VM.
stamp=/var/lib/oci-tools-ci.prepared
want=$(sha256sum ci/vm-prepare.sh | cut -d' ' -f1)
if [ ! -e "$stamp" ] || [ "$(cat "$stamp")" != "$want" ]; then
    bash ci/vm-prepare.sh
    echo "$want" >"$stamp"
fi

export RUSTUP_HOME=$HOME/.rustup
export CARGO_HOME=$HOME/.cargo
export CARGO_TARGET_DIR=$WORK/target
export PATH="$CARGO_HOME/bin:$PATH"

# --- Toolchain -----------------------------------------------------------
if ! command -v rustup >/dev/null 2>&1; then
    curl -fsSL --retry 5 https://sh.rustup.rs |
        sh -s -- -y --default-toolchain none --profile minimal --no-modify-path
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

# --- Build + test --------------------------------------------------------
cargo build --workspace --locked
cargo test --workspace --locked
cargo build --workspace --release --locked

# --- Artifacts (staged inside the image; the host pulls them via `ocivmm cp`)
rm -rf artifacts
mkdir -p artifacts
for bin in ocirun ociman ocicri ocibox ociboot ociboot-init ocivmm; do
    cp "$CARGO_TARGET_DIR/release/$bin" artifacts/
done
artifacts/ociman --version

# --- RPM packaging verification (CentOS Stream 10 only) ------------------
# A real, RPM-native distro -- the one guest base where ci/build-rpm.sh's
# own OCI_RPM_VERIFY_INSTALL=1 (a genuine rpm -i/--version/rpm -e round
# trip, not just extract-and-run) is both meaningful and safe; see
# docs/design/0224/0225/0227.
if [ -r /etc/os-release ] && (. /etc/os-release && [ "$ID" = "centos" ]); then
    echo "vm-ci: CentOS guest, also verifying RPM packaging"
    rm -rf artifacts-rpm
    OCI_RPM_VERIFY_INSTALL=1 bash ci/build-rpm.sh
fi

echo "vm-ci: done"
