# Hacking on oci-tools

## Prerequisites

* rustup (the pinned toolchain in `rust-toolchain.toml` installs itself on
  first `cargo` invocation, or run `rustup toolchain install`)
* a C toolchain (`gcc`/`clang`) for `-sys` crates in later milestones
* python3 for `ci/guards.py`

## Everyday commands

```sh
cargo build --workspace          # keep the whole tree compiling
cargo test  --workspace          # unit + cross-binary smoke tests
cargo fmt --all                  # format
cargo clippy --workspace --all-targets -- -D warnings
python3 ci/guards.py             # repo policy guards (fast, no network)
cargo deny check                 # advisories/licenses/bans (needs cargo-deny)
```

CI denies warnings; locally they are allowed so you can iterate.

## Repository rules (enforced by CI)

* All shared logic lives in `crates/`; `bin/*` crates are thin frontends and
  must not depend on each other (`ci/guards.py`).
* One crate per capability (one tar impl, one HTTP client, ...): the curated
  table lives in `ci/guards.py`. If you need a new capability, add the group.
* Never reference btrfs/zfs outside documentation. Immutable images are
  erofs; writable state is ext4/xfs.
* Shelling out is only allowed for: `mkfs.erofs`, `mkfs.ext4`/`mkfs.xfs`,
  `veritysetup`, `grub2-mkconfig`/`grub-install`, `dracut`, `sfdisk`/`blkid`
  — each wrapped behind a trait in its owning crate.
* Every milestone starts with a design note in `docs/design/`; keep it
  updated as the implementation evolves.

## The CI VM harness

CI builds and tests natively inside VMs of the two supported bases
(CentOS Stream 10, Ubuntu 26.04) on both x86_64 and aarch64. The harness in
`ci/` is plain bash + QEMU and works locally on any Linux with KVM (or
without — it falls back to TCG, slowly):

```sh
ci/setup-host.sh                       # once: qemu + firmware (+ /dev/kvm perms)
OCI_CI_BASE=ubuntu-26.04 ci/run-in-vm.sh
OCI_CI_BASE=centos-stream10 ci/run-in-vm.sh
```

`run-in-vm.sh` boots the cloud image, pushes the tree, runs `ci/vm-ci.sh`
inside, and pulls release binaries into `./artifacts/`. Lower-level control:

```sh
export VM_IMAGE_URL=https://cloud-images.ubuntu.com/releases/26.04/release/ubuntu-26.04-server-cloudimg-amd64.img
ci/vm.sh up
ci/vm.sh run -- uname -a
ci/vm.sh push . oci-tools
ci/vm.sh pull artifacts ./artifacts
ci/vm.sh down
```

State lives under `~/.cache/oci-tools-ci/` (base images, VM overlay disk,
build-cache disk `cache-disk.qcow2` carrying rustup/cargo/target between
runs — delete it for a cold build).

## Version embedding

`--version` embeds the short git hash via `oci-build-info` (a tiny
build-dependency crate). Tarball builds without `.git` can set
`OCI_TOOLS_GIT_HASH` in the environment; otherwise the hash degrades to
`unknown`.
