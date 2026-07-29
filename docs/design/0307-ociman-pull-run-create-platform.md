# Design note 0307: `ociman pull`/`run`/`create --platform`

Status: implemented
Scope: `crates/oci-registry/src/pull.rs`, `bin/ociman/src/main.rs`,
`bin/ociman/src/build.rs`, `bin/ocivmm/src/main.rs`,
`bin/ocibox/src/main.rs`, `bin/ocicri/src/image_service.rs`,
`tests/tests/ociman_platform.rs`.

## The gap: an already-built capability, unreachable from the CLI

`oci_spec_types::image::{Platform, ImageIndex}` and `oci_registry`'s
own `pull()`/`resolve_manifest()` already fully implement following a
real multi-platform manifest index down to a matching child manifest
(`ImageIndex::select`, already unit-tested at the `oci-spec-types`
level). But every real caller — `pull_unconditionally`, and the
`Newer`-policy digest check inside `resolve_or_pull` — hardcoded
`Platform::host()`, and **no CLI surface existed to override it at
all**: `ociman pull`/`run`/`create --help` had zero `--platform` flag.
So a multi-arch image could only ever resolve to the host's own
platform — correct for the overwhelmingly common case, but with no way
to reach a foreign-arch manifest-list entry at all, unlike real
`docker pull --platform`/`podman pull --platform`/`podman run
--platform`.

Notably, `ociman build --platform` already existed — a real,
previously-unnoticed asymmetry between `build` and `pull`/`run`/
`create`, not a whole-new-concept gap.

## Deliberately different scope than `ociman build --platform`

`ociman build --platform` (0193) genuinely asserts the requested
platform matches the host and errors otherwise, because a build
actually executes `RUN` steps synchronously using the host's own
kernel — there is no way around requiring a match there.

`pull`/`run`/`create --platform` is different, and matches real
podman's own documented behavior exactly (`--platform`: "Specify the
platform for selecting the image"): it's purely an image-*selection*
mechanism, with **no host-match assertion at all**. A mismatched pull
is completely ordinary (useful for inspecting/re-pushing a
foreign-arch image); only actually *running* a foreign-architecture
binary would fail, and it fails naturally at the kernel's own
`execve(2)` (`ENOEXEC`) — the same honest, non-fabricated outcome real
podman/docker give without `qemu-user-static`/`binfmt_misc`
registered, needing no special-casing here either.

## Implementation

`oci_registry::pull_unconditionally`/`resolve_or_pull` both gained a
`platform: &Platform` parameter (threaded to the existing, unchanged
`pull()`/`has_different_digest()` internals — `resolve_manifest`'s own
index-selection logic needed no changes at all). Every non-`ociman`
caller (`ocivmm`, `ocibox`, `ocicri`'s `PullImage`) passes
`&Platform::host()` unchanged — no CLI flag added there, out of scope
for this turn, matching this project's own established narrow-slice
convention.

`ociman build`'s own `parse_platform_spec` (already Go-BuildKit-
compatible `os/arch[/variant]` parsing) is now `pub(crate)` and takes
a `command: &str` for its error message prefix, reused by `ociman
pull`/`run`/`create` rather than duplicated a second time. `Command::
Pull` gained `--platform`; the shared `RunArgs` struct (covering both
`run`/`create`) gained the identical flag, parsed eagerly in
`prepare_container` (defaulting to `Platform::host()` when omitted,
exactly as before this flag existed) and threaded into the existing
`resolve_or_pull` call.

## Verified

Manual, end-to-end against a real registry (`docker.io/library/
busybox`, a genuine multi-arch image) on this aarch64 host: a bare
`ociman pull` resolves `arm64` (this host's own architecture,
confirmed via `ociman inspect`'s own `architecture` field); `ociman
pull --platform linux/amd64` resolves a genuinely different manifest
digest, declaring `amd64` instead — cross-checked directly against an
installed `podman pull --platform linux/amd64`, which resolves the
identical `amd64` image. `ociman create --platform linux/amd64`
followed by `ociman inspect` on the resolved image confirms the same
selection through the `run`/`create` path. An invalid `--platform`
value is a clear, immediate error (`missing an architecture`),
matching `ociman build --platform`'s own identical parser.

Integration (new `tests/tests/ociman_platform.rs`, 5 tests): a real,
fully offline plain-HTTP mock registry serves a genuine two-platform
`ImageIndex` (`linux/arm64`/`linux/amd64`, each a real, independently
fetchable child manifest with its own real gzip+tar layer) —
`--platform linux/amd64`/`linux/arm64` each fetch the correct, distinct
child manifest (verified by comparing the returned digest against
each real, independently-computed one, not just success/failure); no
`--platform` at all still resolves to a real platform (this test
host's own, whichever it is) rather than erroring; an invalid value is
a clear error; `ociman create --platform` threads the same selection
through `prepare_container`'s own pull path, verified via the created
image's own resolved `architecture` field.

Regression: full `cargo test --workspace --locked` (112 test result
blocks — one more than before, the new `ociman_platform.rs` binary —
0 failures after two known, pre-existing, unrelated `ocicri_
container.rs` flakes under full parallel `ci/native-ci.sh` load were
each confirmed non-regressing via isolated re-run plus a full clean
re-run, matching this project's own established ritual for this known
flake class).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).

Performance: no impact on any benchmarked hot path — the common,
no-`--platform`-given case still resolves to `Platform::host()`
exactly as before, with the identical number of operations; a real
`--platform` value only ever changes *which* manifest gets selected
out of an index already being parsed, not how many network round
trips or how much work happens. No re-benchmark needed.

## Still ahead

No further `ociman pull`/`run`/`create --platform` gap is known
against real `podman`/`docker`. `ocivmm`/`ocibox`/`ocicri` don't
expose an equivalent `--platform` flag of their own — a real,
separately-scoped candidate if any of those binaries' own users ever
need to select a non-host platform.
