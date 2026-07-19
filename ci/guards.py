#!/usr/bin/env python3
"""Project guards that cargo-deny cannot express. Run from anywhere; CI runs
this in the lint job. Exits non-zero with an explanation on any violation.

Guards:
  1. forbidden-fs: the words btrfs/zfs must not appear in any tracked file
     outside documentation (docs/, *.md, LICENSE). oci-tools supports erofs
     for immutable images and ext4/xfs for writable state -- nothing else.
     Cargo.lock is included on purpose: a stray dependency trips this too.
  2. bin-deps: bin/* crates are thin frontends; they must never depend on
     each other (shared logic belongs in crates/*).
  3. one-crate-per-function: exactly one direct dependency per capability
     (one tar implementation, one HTTP client, ...) across the whole
     workspace, per the curated groups below. Transitive duplicates are
     cargo-deny's job ([bans] multiple-versions); this guard is about the
     crates *we* choose.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

FORBIDDEN_RE = re.compile(r"\b(btrfs|zfs)\b", re.IGNORECASE)

# Paths exempt from the forbidden-fs scan: documentation may (and should)
# explain *why* those filesystems are excluded; deny.toml names the crates it
# bans; this guard contains its own pattern.
def is_exempt_path(path: str) -> bool:
    return (
        path.startswith("docs/")
        or path.endswith(".md")
        or path == "LICENSE"
        or path == "ci/guards.py"
        or path == "deny.toml"
    )


# Capability groups: at most one of each may be a direct dependency of any
# workspace crate. Grow each list when you notice an alternative, and add new
# groups as capabilities are adopted.
CAPABILITY_GROUPS: dict[str, list[str]] = {
    "tar archive handling": ["tar", "tokio-tar", "async-tar", "krata-tokio-tar"],
    "HTTP client": ["reqwest", "ureq", "isahc", "attohttpc", "minreq", "curl"],
    "CLI argument parsing": ["clap", "structopt", "argh", "gumdrop", "pico-args", "lexopt", "bpaf"],
    "error derive": ["thiserror", "snafu", "displaydoc"],
    "error context/reporting": ["anyhow", "eyre", "color-eyre", "miette"],
    "logging/tracing facade": ["tracing", "log", "slog", "defmt"],
    "JSON serialization": ["serde_json", "json", "simd-json", "tinyjson"],
    "SHA-2 digests": ["sha2", "sha256", "hmac-sha256", "openssl"],
    "progress bars": ["indicatif", "pbr", "linya", "kdam"],
    "temporary files": ["tempfile", "tempdir", "mktemp"],
    "low-level unix syscalls": ["rustix", "nix"],
    "seccomp-bpf filtering": ["seccompiler", "libseccomp", "libseccomp-sys", "syscallz"],
    "gzip decompression": ["flate2", "libflate", "zune-inflate"],
    "zstd decompression": ["ruzstd", "zstd", "zstd-safe"],
}


def fail(title: str, lines: list[str]) -> None:
    print(f"guards: FAIL: {title}", file=sys.stderr)
    for line in lines:
        print(f"  {line}", file=sys.stderr)
    sys.exit(1)


def git_tracked_files() -> list[str]:
    # Tracked plus untracked-but-not-ignored, so the guard also catches
    # violations before they are ever committed.
    out = subprocess.run(
        ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    return [f for f in out.split("\0") if f]


def check_forbidden_fs() -> None:
    violations = []
    for rel in git_tracked_files():
        if is_exempt_path(rel):
            continue
        path = os.path.join(ROOT, rel)
        if not os.path.isfile(path):
            continue
        try:
            with open(path, encoding="utf-8", errors="ignore") as fh:
                for lineno, line in enumerate(fh, 1):
                    match = FORBIDDEN_RE.search(line)
                    if match:
                        violations.append(f"{rel}:{lineno}: {line.strip()!r}")
        except OSError as err:
            violations.append(f"{rel}: unreadable: {err}")
    if violations:
        fail(
            "forbidden filesystem reference (btrfs/zfs) outside docs",
            violations
            + ["policy: erofs for immutable images, ext4/xfs for writable state"],
        )
    print("guards: ok: no forbidden filesystem references")


def cargo_metadata() -> dict:
    out = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps", "--locked"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    return json.loads(out)


def workspace_packages(meta: dict) -> list[dict]:
    members = set(meta["workspace_members"])
    return [p for p in meta["packages"] if p["id"] in members]


def is_bin_package(pkg: dict) -> bool:
    rel = os.path.relpath(pkg["manifest_path"], ROOT)
    return rel.replace(os.sep, "/").startswith("bin/")


def check_bin_interdependencies(packages: list[dict]) -> None:
    bin_names = {p["name"] for p in packages if is_bin_package(p)}
    violations = []
    for pkg in packages:
        if pkg["name"] not in bin_names:
            continue
        for dep in pkg["dependencies"]:
            if dep["name"] in bin_names:
                violations.append(
                    f"{pkg['name']} depends on {dep['name']} "
                    f"(bin/* crates must only depend on crates/*)"
                )
    if violations:
        fail("bin/* crates must not depend on each other", violations)
    print(f"guards: ok: no bin->bin dependencies ({len(bin_names)} binaries checked)")


def check_capability_groups(packages: list[dict]) -> None:
    # crate name -> set of workspace packages that declare it (any dep kind).
    declared: dict[str, set[str]] = {}
    for pkg in packages:
        for dep in pkg["dependencies"]:
            declared.setdefault(dep["name"], set()).add(pkg["name"])

    violations = []
    for capability, alternatives in CAPABILITY_GROUPS.items():
        present = [c for c in alternatives if c in declared]
        if len(present) > 1:
            detail = "; ".join(
                f"{crate} (used by {', '.join(sorted(declared[crate]))})"
                for crate in present
            )
            violations.append(f"{capability}: {detail}")
    if violations:
        fail(
            "multiple crates providing the same capability",
            violations + ["pick one implementation per capability (see ci/guards.py)"],
        )
    print(f"guards: ok: one crate per capability ({len(CAPABILITY_GROUPS)} groups checked)")


def main() -> None:
    check_forbidden_fs()
    meta = cargo_metadata()
    packages = workspace_packages(meta)
    check_bin_interdependencies(packages)
    check_capability_groups(packages)
    print("guards: all checks passed")


if __name__ == "__main__":
    main()
