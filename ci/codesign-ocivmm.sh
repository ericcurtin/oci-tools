#!/usr/bin/env bash
# Ad-hoc codesign a just-built ocivmm (or any oci-vmm test/example
# binary) with the com.apple.security.hypervisor entitlement.
#
# Hypervisor.framework on Apple Silicon denies hv_vm_create() with
# HV_DENIED to any process that isn't codesigned with this entitlement
# -- true even running as root -- see docs/design/0249. Ad-hoc signing
# (`codesign -s -`, no paid Apple Developer account, no keychain
# identity) is sufficient for this entitlement specifically; it is not
# sufficient for distribution/notarization, which is out of scope
# here.
#
# Usage: ci/codesign-ocivmm.sh <path-to-binary>...
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "codesign-ocivmm.sh: not macOS, nothing to do" >&2
    exit 0
fi

if [[ $# -eq 0 ]]; then
    echo "usage: $0 <path-to-binary>..." >&2
    exit 1
fi

entitlements="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/packaging/macos/ocivmm.entitlements"

for bin in "$@"; do
    codesign --force --sign - --entitlements "$entitlements" "$bin"
    echo "codesign-ocivmm.sh: signed $bin with com.apple.security.hypervisor"
done
