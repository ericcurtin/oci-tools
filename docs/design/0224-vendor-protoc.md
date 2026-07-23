# Design note 0224: vendoring `protoc` instead of requiring a system package

Status: implemented
Scope: `crates/oci-cri-types/build.rs`, `crates/oci-cri-types/Cargo.toml`,
`Cargo.toml` (workspace).

## Found while actually verifying RPM packaging in a real CentOS Stream 10 VM

Milestone 8's RPM packaging (0216) had, until now, only ever been
verified on this project's own Ubuntu development host — where
`protoc` happens to already be installed (Ubuntu's own
`protobuf-compiler` package). This session finally exercised
`ci/build-rpm.sh` for real inside a genuine, freshly-provisioned
CentOS Stream 10 aarch64 guest (this project's own existing VM
harness, `ci/vm.sh`, with the already-cached cloud image from
`~/.cache/oci-tools-ci/images/`) — closing the long-standing "still
ahead" gap named repeatedly in 0216/`packaging/README.md` ("a real
`rpm -i` install verification ... only a real CentOS Stream 10 ... runner
could do that meaningfully").

That real verification surfaced a genuine, previously-undiscovered
blocker: CentOS Stream 10 ships **no dnf-installable `protoc` binary
at all**, not even via EPEL — confirmed directly (`dnf provides '*/
protoc'`, `dnf search protobuf-compiler`, both empty; the `protobuf`
package itself only ships the runtime `libprotobuf.so`, no compiler
binary). `oci-cri-types`'s own `build.rs` (0212) required a real,
host-installed `protoc` — this had silently worked everywhere so far
purely because every host it had actually been built on (this dev
host, GitHub's own `ubuntu-24.04`/`ubuntu-24.04-arm` runners) happened
to already have one.

## The fix: `protoc-bin-vendored`, not a new host-package requirement

Rather than teach `ci/vm-prepare.sh` (or a future CentOS-specific
packaging step) to somehow obtain a `protoc` binary on a distro that
doesn't package one via its normal package manager at all, `oci-cri-
types/build.rs` now uses `protoc-bin-vendored` (a real, MIT-licensed,
widely used crate — 14M+ downloads — that bundles Google's own
prebuilt `protoc` binaries for every common platform, including
`linux-aarch_64`/`linux-x86_64`, the two this project's own CI matrix
actually needs) to supply `$PROTOC` itself when nothing's already set.
This removes the external host dependency **everywhere**, not just for
CentOS — this dev host's own reliance on Ubuntu's `protobuf-compiler`
package was itself silently accidental, never a deliberate design
choice, and is now gone too.

An already-set `$PROTOC` (a caller's own, deliberately chosen system
`protoc`) still wins — this only fills the gap when nothing's set at
all, matching `prost-build`'s own upstream-documented precedence.

## `#[allow(unsafe_code)]` on `build.rs`'s own `main`

`std::env::set_var` is `unsafe` (a real, current Rust soundness rule:
mutating the environment is only safe when nothing else could be
reading/writing it concurrently). Build scripts genuinely run single-
threaded, before any of this crate's own code exists yet, so this is
safe in practice — the same, already-established per-function
`#[allow(unsafe_code)]` pattern this project already uses throughout
`oci-runtime-core` for its own real, justified raw syscalls (`exec.rs`/
`hooks.rs`/`identity.rs`), not a blanket module-wide allowance.

## Verified

- `cargo build`/`clippy --all-targets -- -D warnings`/`cargo deny
  check` all clean locally (only the pre-existing benign `deny`
  warning) — `protoc-bin-vendored` and its platform-specific sub-crate
  are new, but MIT-licensed and already covered by `deny.toml`'s
  existing allow-list.
- **The actual point of this whole investigation**: verified directly,
  inside a real, freshly-provisioned CentOS Stream 10 aarch64 VM (no
  system `protoc`, confirmed absent both before and after this fix —
  the vendored binary is bundled in the crate itself, no new host
  package needed): `cargo build --release -p oci-cri-types --offline`
  now succeeds with zero `protoc`-related setup at all, where it
  previously failed with prost-build's own "Could not find `protoc`"
  error.
- Full workspace: `cargo build --workspace`, `cargo test --workspace`
  (95/95 result blocks, 0 failures — no test behavior changed, this is
  a build-time-only dependency), `cargo fmt --check`, `cargo clippy
  --all-targets -- -D warnings`, `python3 ci/guards.py` (18 capability
  groups, unaffected — a build-dependency confined to `oci-cri-types`,
  no new runtime capability), `bash ci/native-ci.sh`, hyperfine perf
  sanity on `ociman run --rm` (no regression, as expected for a
  build-time-only change nowhere near any binary's own runtime code).

## What this unblocks (still ahead, not done this session)

With `protoc` no longer a real, host-dependent requirement anywhere,
a real RPM-native CI verification (building `ci/build-rpm.sh`'s own
package inside a genuine CentOS Stream 10 guest, reusing this
project's own existing `ci/vm.sh`/`ci/run-in-vm.sh` VM harness) is now
actually possible for the first time — the one real blocker this
session found and fixed. Wiring that into the harness/CI workflow for
real (rather than the one, ad hoc, manual verification run this
session) is real, separate, still-ahead follow-up work.
