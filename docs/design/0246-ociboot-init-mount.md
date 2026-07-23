# Design note 0246: `ociboot-init mount` — the boot-time mount, first slice

Status: implemented
Scope: `bin/ociboot-init/src/main.rs`, `bin/ociboot-init/Cargo.toml`,
`tests/tests/ociboot_init_mount.rs` (new).

## Milestone 5's actual core, started

`ociboot-init` had been a milestone-1 skeleton since the beginning:
`--version`/`--help` and a promise. Milestone 5's real remaining core
is "actually mounting a verified image at boot" — this increment
lands its first real slice as a `mount` operation, covering the
verify-and-mount half of the boot sequence:

1. Read the kernel command line (`--cmdline`, default
   `/proc/cmdline`) and parse the `ociboot.*` kargs via the same
   shared `oci_bls::cmdline` parser `ociboot`'s own kargs handling
   uses (real quoting rules, ported from real bootc — 0137).
2. `ociboot.image=<file>` names the deployment under `--image-dir` —
   a plain file name, never a path: an
   `ociboot.image=../../etc/shadow`-shaped value must not escape the
   image directory (the same traversal rule `ocibox`'s own name
   validation established, applied for the same reason).
3. `ociboot.verity=<64-hex>`: when present, the image's real
   fs-verity digest (the same shared `oci_erofs::verity::measure`
   ioctl everything else uses) must match byte for byte, *before*
   anything is attached or mounted — an unsealed or mismatching image
   is a hard boot error. When absent, the image mounts unverified
   with a real warning: `ociboot build-image --seal` is opt-in
   (0201), and a boot must not fail for a configuration the installer
   legitimately produced. (The detached dm-verity fallback — 0202's
   `<image>.verity` sidecar — is a documented later increment; this
   slice enforces fs-verity only.)
4. Loop-attach read-only (`LO_FLAGS_READ_ONLY|DIRECT_IO`, shared
   `oci_mount::loop_device`) and mount erofs read-only at `--target`
   — detaching again if the mount itself fails, so a failed boot
   attempt never leaks a loop device.

Still ahead, in the skeleton's own original words: mounting the state
partition, the writable view (/etc overlay, /var bind, tmpfs
/run+/tmp), binding /ociboot into the target, switch-root, and the
`90ociboot` dracut module that installs this binary — plus wiring
`ociboot` itself to *emit* these kargs into a real BLS entry.

## Dependency discipline

The binary stays clap/tracing/anyhow-free (manual argv handling, two
exit-code classes: 2 usage with help, 1 real boot failure) — but the
"dependency-free" comment was always about *external* weight, so the
shared workspace crates it now uses (`oci-bls`, `oci-erofs`,
`oci-mount`, whose own dependencies are just libc/rustix/tempfile)
are exactly the right reuse: zero duplicated cmdline/ioctl/mount
code, per this project's own share-everything pillar. The Cargo.toml
comment now says precisely that.

## Verified

- Unit tests (pure parsing, no privileges): the karg contract —
  required image, empty/path-shaped rejections (traversal), verity
  digest shape validation and lowercase normalization, verity-less
  boots allowed.
- Integration, against a real deployment built by the actual
  `ociboot build-image` binary: every unprivileged failure surface
  (missing karg, traversal value, verity-expected-but-unsealed,
  missing image file, usage-vs-boot-failure exit codes) — and the
  real privileged happy path under passwordless sudo (the workspace's
  established opportunistic gate): a real loop attach, a real erofs
  read-only mount whose tree contains the seeded rootfs, a write
  attempt genuinely rejected, then unmount and loop-device release,
  leftover-checked. On this dev host the privileged path runs for
  real (erofs + sudo available); hosts without either skip cleanly.
- Full workspace: `cargo build`, `cargo test --workspace`,
  `cargo fmt --all --check`, `cargo clippy --workspace --all-targets
  -- -D warnings`, `python3 ci/guards.py`, `cargo deny check`,
  `bash ci/native-ci.sh`, `ci/build-deb.sh` (all six binaries still
  package cleanly), `ci/bench.sh` sanity (nothing on any container
  path changed).
