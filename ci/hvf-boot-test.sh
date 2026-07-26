#!/usr/bin/env bash
# For **local** use on real Apple Silicon hardware only -- despite the
# `ci/` location (matching this project's existing convention of
# putting reusable scripts there regardless of caller), no CI job
# actually runs this: GitHub's own hosted macOS runners don't support
# Hypervisor.framework at all, on any macOS version (confirmed
# directly, see .github/workflows/ci.yml's own `hvf-build` job
# comment and docs/design/0249's phase 7 section) -- there is no
# GitHub-hosted way to run this at all right now.
#
# Builds and runs crates/oci-vmm/tests/hvf_boot.rs (the phase 3
# milestone from docs/design/0249: a real, unmodified distro aarch64
# kernel boots through the hvf backend to its own console banner and
# panics cleanly for lack of a root filesystem) against a real kernel
# image -- codesigning the compiled test binary in between build and
# run, since Hypervisor.framework denies hv_vm_create() to a binary
# that isn't (see ci/codesign-ocivmm.sh), which `cargo test` alone has
# no way to do (it builds and runs in one step).
#
# Usage: ci/hvf-boot-test.sh <path-to-real-kernel-image>
# (pair with ci/fetch-aarch64-kernel.sh to get one)
set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "usage: $0 <path-to-real-kernel-image>" >&2
    exit 1
fi
kernel="$1"

# `cargo test --no-run` alone doesn't print the compiled binary's own
# path anywhere stable enough to rely on (its filename is content-
# hashed) -- `--message-format=json`'s own `compiler-artifact` message
# for this crate's test profile names it exactly.
bin="$(
    cargo test -p oci-vmm --test hvf_boot --no-run --locked --message-format=json |
        jq -r 'select(.profile.test == true and .target.name == "hvf_boot") | .filenames[0]' |
        tail -1
)"
if [[ -z "$bin" ]]; then
    echo "hvf-boot-test: couldn't find the compiled hvf_boot test binary" >&2
    exit 1
fi

"$(dirname "${BASH_SOURCE[0]}")/codesign-ocivmm.sh" "$bin"

OCIVMM_TEST_KERNEL_IMAGE="$kernel" "$bin" --test-threads=1
