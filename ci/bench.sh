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
commit_tag=localhost/oci-tools-bench-commit:latest
cleanup() {
    # The ociman half of the commit comparison lives entirely inside
    # $workdir (a scratch storage root), so removing $workdir is its
    # whole cleanup -- the default ociman store is never touched.
    rm -rf "$workdir"
    "$ociman" rm -f benchbox >/dev/null 2>&1 || true
    if need podman; then
        podman rm -f benchbox >/dev/null 2>&1 || true
        podman rm -f benchcommit >/dev/null 2>&1 || true
        podman rmi "$commit_tag" >/dev/null 2>&1 || true
        # Deliberately no `podman image prune` here: it would sweep
        # dangling images this script didn't create. The re-commits
        # leave a few tiny dangling configs behind in podman's own
        # store (the committed *layer* is content-identical every
        # sample so it deduplicates; only each commit's own
        # `created`-timestamped config differs); `podman image prune`
        # reclaims them whenever the host wants.
    fi
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

echo
echo "### ociman vs podman vs docker: run -d (detached create+start, returns at running) ###"
# The isolated create+start half of the startup story (the combined
# run --rm cycle above includes destroy) -- the same "ociman run -d
# (create-only) vs podman run -d" figure every performance-
# reverification note since 0161 measured by hand (see
# docs/design/0170's own "Method" section): each sample starts a real
# detached container and returns once it's running; the previous
# sample's own container is removed in --prepare, outside the timed
# region.
"$ociman" rm -f benchd >/dev/null 2>&1 || true
if need podman; then podman rm -f benchd >/dev/null 2>&1 || true; fi
if need docker; then docker rm -f benchd >/dev/null 2>&1 || true; fi
hf_args=()
if "$ociman" images 2>/dev/null | grep -q "^$image "; then
    hf_args+=(
        --prepare "$ociman rm -f benchd >/dev/null 2>&1 || true"
        --command-name "ociman run -d" "$ociman run -d --name benchd $image sleep 60"
    )
fi
if need podman && podman image exists "$image" 2>/dev/null; then
    hf_args+=(
        --prepare "podman rm -f benchd >/dev/null 2>&1 || true"
        --command-name "podman run -d" "podman run -d --name benchd $image sleep 60"
    )
fi
if need docker && docker image inspect "$image" >/dev/null 2>&1; then
    hf_args+=(
        --prepare "docker rm -f benchd >/dev/null 2>&1 || true"
        --command-name "docker run -d" "docker run -d --name benchd $image sleep 60"
    )
fi
if [ "${#hf_args[@]}" -gt 0 ]; then
    hyperfine --warmup 3 "${hf_args[@]}"
fi
"$ociman" rm -f benchd >/dev/null 2>&1 || true
if need podman; then podman rm -f benchd >/dev/null 2>&1 || true; fi
if need docker; then docker rm -f benchd >/dev/null 2>&1 || true; fi

echo
echo "### ociman vs podman: commit (an already-stopped container, re-committed over the same tag) ###"
# The exact methodology every performance-reverification note since
# 0161 has used by hand (see docs/design/0176's own "Method" section):
# one real, already-stopped container per tool (`sh -c "echo hi >
# /f.txt"`, a real, nonempty diff layer), reused every sample, each
# sample re-committing over the same tag -- a real, no-error operation
# for both tools. The committed layer is content-identical every
# sample so it deduplicates in both stores.
#
# "Forcing plain-Extract rootfs setup" (those notes' own words) needs
# one real step this script has to encode so it doesn't get
# re-discovered by hand again (it was, wiring this up): on a host
# where the rootless-overlay rootfs optimization (0108/0146) is
# supported -- this project's own dev hosts included -- a container in
# the *default* store gets an overlay rootfs, and `ociman commit`
# rejects exactly that with a clear "not supported yet" error
# (docs/design/0146). So the ociman half runs against a scratch
# storage root under $workdir (cleaned up with it) whose
# `.rootless-overlay-supported` marker (see ociman's own
# rootfs_setup.rs) is pre-seeded `false`, forcing the same
# plain-Extract container every hand-run measurement used. The image
# is copied into that scratch store offline via `ociman save`/`load`
# (no network), from the same already-pulled default-store image every
# other section already requires.
commit_store="$workdir/commit-store"
hf_args=()
if "$ociman" images 2>/dev/null | grep -q "^$image "; then
    mkdir -p "$commit_store"
    echo false >"$commit_store/.rootless-overlay-supported"
    "$ociman" save -o "$workdir/commit-image.tar" "$image" >/dev/null
    OCI_TOOLS_STORAGE_ROOT="$commit_store" "$ociman" load -i "$workdir/commit-image.tar" >/dev/null
    OCI_TOOLS_STORAGE_ROOT="$commit_store" \
        "$ociman" run --name benchcommit "$image" sh -c 'echo hi > /f.txt' >/dev/null
    hf_args+=(--command-name "ociman commit" \
        "OCI_TOOLS_STORAGE_ROOT=$commit_store $ociman commit benchcommit $commit_tag")
else
    echo "bench: $image not already pulled into ociman's own store, skipping (run 'ociman pull $image' first)" >&2
fi
if need podman && podman image exists "$image" 2>/dev/null; then
    podman rm -f benchcommit >/dev/null 2>&1 || true
    podman run --name benchcommit "$image" sh -c 'echo hi > /f.txt' >/dev/null
    hf_args+=(--command-name "podman commit" "podman commit benchcommit $commit_tag")
else
    need podman && echo "bench: $image not already pulled into podman's own store, skipping" >&2
fi
if [ "${#hf_args[@]}" -gt 0 ]; then
    hyperfine --warmup 3 "${hf_args[@]}"
fi

echo "bench: done"
