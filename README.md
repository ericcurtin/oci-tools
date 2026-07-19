# oci-tools

A pure-Rust, monorepo reimplementation of the container and bootable-container
stack — with a bootc-like OS updater that has **no dependency on ostree or
composefs** (deployments are flattened erofs images sealed with fsverity).

| binary         | role                                                        | equivalent |
|----------------|-------------------------------------------------------------|------------|
| `ocirun`       | OCI runtime (runc-CLI-compatible)                           | crun       |
| `ociman`       | daemonless container engine incl. `build`                   | podman     |
| `ocicri`       | Kubernetes CRI server                                       | cri-o      |
| `ocibox`       | pet containers with home/user/host integration              | distrobox  |
| `ociboot`      | bootable-container OS manager (install/upgrade/rollback)    | bootc      |
| `ociboot-init` | tiny initramfs binary that mounts deployments (dracut)      | —          |

Design pillars:

* **One implementation per function.** One registry client, one blob store,
  one tar/whiteout applier, one erofs builder, one mount library, one runtime
  core — all in `crates/`, shared by every binary in `bin/` (which are thin
  frontends and never depend on each other). CI enforces this.
* **Filesystem policy.** erofs for immutable OS/deployment images; ext4/xfs
  for all writable state. btrfs and zfs are forbidden — CI greps for them.
* **ostree's concepts without ostree.** Immutable images, transactional
  deployments, rollback, boot counting, `/etc` three-way merge, persistent
  `/var` — reimplemented directly on OCI + erofs + fsverity + BLS entries.
* **First-class distros:** CentOS Stream 10 and Ubuntu 26.04, as OS image
  bases and as CI targets (x86_64 + aarch64, tested in VMs).

## Status

Early development, milestone 4 of 8 in progress (`ociman build` works
end to end for a real, deliberately narrow subset of Dockerfiles —
single-stage; `RUN` executes for real; `COPY` from the build context
works for a single file/directory source; `ADD` and multi-source/glob
`COPY` are not implemented yet). See [docs/design/](docs/design/) for
design notes per milestone.

| milestone | scope | status |
|-----------|-------|--------|
| 1 | workspace skeleton, `oci-cli-common`, 4-VM CI matrix | **done** |
| 2 | `oci-spec-types`/`oci-registry`/`oci-store`; `ociman pull/images/inspect` | **done** |
| 3 | `oci-runtime-core` + `ocirun`; `ociman run/exec/ps/logs` rootless | **done** (plus systemd cgroups, hooks, seccomp, resource limits beyond the original scope) |
| 4 | `oci-dockerfile`; `ociman build` (multi-stage, cache) | in progress — single-stage builds with `RUN`/`COPY` work end to end (metadata instructions + real `RUN` execution/`COPY` from the build context, both committing real layers); `ADD`, multi-stage, and the build cache are not yet implemented (see `docs/design/0050`-`0052`) |
| 5 | erofs/mount/BLS; `ociboot install to-disk`; dracut module; QEMU boot test | — |
| 6 | upgrade/switch/rollback/status/gc; /etc merge; boot counting; layered mode | — |
| 7 | `ocicri` (critest subset), `ocibox` | — |
| 8 | packaging (rpm/deb), docs polish, release workflow | — |

## Layout

```
crates/   shared libraries (all real logic lives here)
bin/      the six binaries (thin frontends)
tests/    cross-binary integration tests
ci/       reusable VM harness + repo guards (used by GitHub Actions)
docs/     architecture + design notes
dracut/   90ociboot dracut module            (milestone 5)
examples/ bootable OS Containerfiles          (milestone 4/5)
packaging/ rpm (CentOS Stream 10) + deb (Ubuntu 26.04)   (milestone 8)
```

## Building

Needs the pinned stable toolchain from `rust-toolchain.toml` (rustup picks it
up automatically):

```sh
cargo build --workspace
cargo test  --workspace
```

Lints as CI runs them:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
python3 ci/guards.py
```

See [docs/HACKING.md](docs/HACKING.md) for the CI VM harness and development
workflow.

## License

Apache-2.0, see [LICENSE](LICENSE).
