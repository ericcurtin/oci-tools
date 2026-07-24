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

## The CI VM harness (ocivmm, dogfooded)

CI builds and tests natively inside VMs of the two supported bases
(CentOS Stream 10, Ubuntu 26.04) on x86_64 — booted by this repo's own
`ocivmm` binary straight from the distros' OCI images (no qemu, no
cloud images, no cloud-init, no ssh; the VMM is libkrun's crates
statically linked into `ocivmm`, and the checkout is shared with the
guest over virtiofs). At create time `ocivmm` provisions the pet VM
with the distro's *own* kernel, initramfs, and systemd (running the
distro's own dnf/apt as a container on the fresh rootfs), so the
tests run under the real distro kernel with the real distro init —
the same fidelity the cloud images had. Works locally on any Linux
with KVM (plus the `passt` package for guest networking):

```sh
ci/setup-host.sh                       # once: /dev/kvm perms + passt
OCI_CI_BASE=ubuntu-26.04 ci/run-in-vm.sh
OCI_CI_BASE=centos-stream10 ci/run-in-vm.sh
```

`run-in-vm.sh` builds `ocivmm`, boots the pet VM (creating and
provisioning it from the OCI image on first use), runs `ci/vm-ci.sh`
inside as a oneshot systemd unit, and the release binaries appear in
`./artifacts/` directly (written through the virtiofs mount).
Lower-level control is `ocivmm` itself:

```sh
sudo target/release/ocivmm run -v "$PWD:/src" ubuntu:26.04 uname -a
sudo target/release/ocivmm run ubuntu-26.04    # root console, poweroff to leave
```

State lives under `~/.cache/oci-tools-ci/` (`vm-state.tar`, the packed
pet-VM storage carrying the provisioned kernel+systemd, installed
packages, rustup/cargo, and the target dir between runs — delete it
for a cold build).

## Benchmarking

```sh
ci/bench.sh   # ocirun vs crun/runc, ociman vs podman/docker, real hyperfine
```

Real, direct `hyperfine` comparisons against whatever of crun/runc/podman/
docker/busybox is actually installed (skips what isn't, rather than
failing). See `docs/benchmarks.md` for the full methodology and historical
results. Local/manual use only, like `ci/build-rpm.sh`/`ci/build-deb.sh` —
not wired into CI (a shared runner is a poor host for wall-clock timing).

## Version embedding

`--version` embeds the short git hash via `oci-build-info` (a tiny
build-dependency crate). Tarball builds without `.git` can set
`OCI_TOOLS_GIT_HASH` in the environment; otherwise the hash degrades to
`unknown`.
