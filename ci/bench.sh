#!/usr/bin/env bash
# Runs this project's own real, direct hyperfine comparisons against
# real crun/runc/podman/docker -- consolidating into one reusable,
# runnable script the exact ad hoc methodology every performance-
# sensitive design doc has used individually and by hand since
# 0012/0018 (git-stash-based A/B, `hyperfine -N`/plain `hyperfine`,
# a real rootless busybox bundle, a real already-pulled image), so
# this project's own "beat the equivalents on all the benchmarks,
# especially startup time and destroy time" claim stays easy to
# re-verify on demand rather than only reproducible by re-reading
# prose. See docs/benchmarks.md for the full narrative and historical
# results this consolidates.
#
# Every comparison here is opportunistic: any one real equivalent
# (crun/runc/podman/docker) or prerequisite (busybox, an
# already-pulled image) that isn't actually available on this host is
# skipped with a clear message rather than failing the whole run --
# this project's own binaries are still benchmarked alone in that
# case. `hyperfine` itself and a real workspace release build are the
# only hard requirements.
#
# Local/manual use only (like ci/build-rpm.sh/ci/build-deb.sh, this is
# not wired into .github/workflows/ci.yml -- a shared CI runner is a
# poor host for a benchmark whose whole point is wall-clock timing
# relative to other real tools that may or may not even be installed
# there).
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/.." && pwd)
cd "$repo"

need() { command -v "$1" >/dev/null 2>&1; }

if ! need hyperfine; then
    echo "bench: hyperfine is not installed, nothing to do" >&2
    exit 1
fi

cargo build --workspace --release --locked --offline

ocirun="$repo/target/release/ocirun"
ociman="$repo/target/release/ociman"

workdir=$(mktemp -d "${TMPDIR:-/tmp}/oci-tools-bench.XXXXXX")
cleanup() {
    rm -rf "$workdir"
    "$ociman" rm -f benchbox >/dev/null 2>&1 || true
    if need podman; then podman rm -f benchbox >/dev/null 2>&1 || true; fi
    if need docker; then docker rm -f benchbox >/dev/null 2>&1 || true; fi
}
trap cleanup EXIT

echo "### ocirun vs crun vs runc: run (create+start+wait+destroy), rootless busybox bundle ###"
if need busybox; then
    bundle="$workdir/bundle"
    mkdir -p "$bundle/rootfs/bin"
    cp "$(command -v busybox)" "$bundle/rootfs/bin/busybox"
    for applet in sh echo true false; do
        ln -sf busybox "$bundle/rootfs/bin/$applet"
    done
    "$ocirun" spec --rootless --bundle "$bundle" >/dev/null

    # crun rejects a bundle whose `ociVersion` is 1.2.x outright
    # ("unknown version specified") -- already documented in
    # docs/design/0105 (patched by hand there to "1.0.2-dev" for the
    # exact same reason); 1.1.0 works just as well and is what this
    # script actually uses. Purely about a fair, all-three-accept-it
    # bundle, not which runtime advertises the newest spec version.
    python3 - "$bundle/config.json" <<'PY'
import json
import sys

path = sys.argv[1]
with open(path) as f:
    config = json.load(f)
config["process"]["terminal"] = False
config["process"]["args"] = ["/bin/true"]
config["ociVersion"] = "1.1.0"
with open(path, "w") as f:
    json.dump(config, f)
PY

    hf_args=(--warmup 5 --command-name "ocirun run" \
        "$ocirun run --log-level error --bundle $bundle bench-ocirun")
    need crun && hf_args+=(--command-name "crun run" "crun run --bundle $bundle bench-crun")
    need runc && hf_args+=(--command-name "runc run" "runc run --bundle $bundle bench-runc")
    hyperfine -N "${hf_args[@]}"
else
    echo "bench: busybox not on \$PATH, skipping (needed for a real rootfs)" >&2
fi

echo
echo "### ociman vs podman vs docker: run --rm (full startup+destroy cycle) ###"
image=docker.io/library/busybox:latest
hf_args=()
if "$ociman" images 2>/dev/null | grep -q "^$image "; then
    hf_args+=(--command-name "ociman run --rm" "$ociman run --rm $image true")
else
    echo "bench: $image not already pulled into ociman's own store, skipping (run 'ociman pull $image' first)" >&2
fi
if need podman && podman image exists "$image" 2>/dev/null; then
    hf_args+=(--command-name "podman run --rm" "podman run --rm $image true")
else
    need podman && echo "bench: $image not already pulled into podman's own store, skipping" >&2
fi
if need docker && docker image inspect "$image" >/dev/null 2>&1; then
    hf_args+=(--command-name "docker run --rm" "docker run --rm $image true")
else
    need docker && echo "bench: $image not already pulled into docker's own store, skipping" >&2
fi
if [ "${#hf_args[@]}" -gt 0 ]; then
    hyperfine --warmup 3 "${hf_args[@]}"
fi

echo
echo "### ociman vs podman: rm (destroy-only, an already-created stopped container) ###"
"$ociman" rm -f benchbox >/dev/null 2>&1 || true
if need podman; then podman rm -f benchbox >/dev/null 2>&1 || true; fi
hf_args=()
if "$ociman" images 2>/dev/null | grep -q "^$image "; then
    hf_args+=(
        --prepare "$ociman create --name benchbox $image true >/dev/null"
        --command-name "ociman rm" "$ociman rm --force benchbox"
    )
fi
if need podman && podman image exists "$image" 2>/dev/null; then
    hf_args+=(
        --prepare "podman create --name benchbox $image true >/dev/null"
        --command-name "podman rm" "podman rm --force benchbox"
    )
fi
if [ "${#hf_args[@]}" -gt 0 ]; then
    hyperfine --warmup 3 "${hf_args[@]}"
fi

echo "bench: done"
