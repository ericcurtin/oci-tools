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
`RUN`/`COPY`/`ADD` execute for real and commit real layers (`ADD`'s own
local-archive-auto-extraction matches real docker's documented
behavior; `ADD` from a remote URL is supported too, never
auto-extracted, matching real BuildKit's own `noDecompress` for that
source kind); multi-stage builds
work via both `FROM <earlier-stage>` and `COPY --from=<earlier-stage>`,
and `COPY --from=<external-image>` (a `--from` naming neither) pulls and
extracts that image just for the one `COPY`; `--build-arg` and
`--target` (building only up to a named stage, pruning everything else
— including a stage that would otherwise fail to build at all) both
work; multiple explicit sources and glob patterns in one `COPY`/`ADD`
both work too (each landing under the destination by its own basename; glob
matching verified byte-for-byte against Go's own `path/filepath.Match`,
the exact matcher real BuildKit itself uses)). Milestone 3 also grew
real
`--memory-swap`/
`--cpuset-cpus`/`--cpuset-mems`/`--security-opt seccomp=`/a real
`podman`-default capability set/`--cap-add`/`--cap-drop`/`--privileged`/
`--read-only`/`-e`/`--env`/`--hostname`/`-w`/`--workdir`/
`ocirun features` (real, checked support-surface introspection,
independently verified byte-for-byte identical to real installed
`runc features` for namespaces/capabilities) beyond its
own original scope.
Milestone 5 also now has real pieces: `oci-erofs` builds
verified-deterministic erofs images via `mkfs.erofs`, seals/verifies
them with real fs-verity ioctls, and has a detached dm-verity fallback
via `veritysetup` for state filesystems without fs-verity support;
`oci-bls` reads/writes the real GRUB environment block
(`saved_entry`/boot-counting), verified byte-for-byte against the real
`grub-editenv`, reads/writes Type #1 BLS entries and scans
`/loader/entries/` as a directory, implements the real spec's own
`+tries_left-tries_done` boot-counting filename convention, and sorts
entries per the real spec's own "Sorting" section (full UAPI.10
version comparison included), all verified against the real
uapi-group specification's own text and worked examples, some
cross-checked against the real `systemd-analyze compare-versions`
tool too; `oci-mount` attaches/detaches real loop devices
(read-only, direct-io), verified
against the real `losetup`. See [docs/design/](docs/design/) for
design notes per
milestone.

| milestone | scope | status |
|-----------|-------|--------|
| 1 | workspace skeleton, `oci-cli-common`, 4-VM CI matrix | **done** |
| 2 | `oci-spec-types`/`oci-registry`/`oci-store`; `ociman pull/images/inspect` | **done** |
| 3 | `oci-runtime-core` + `ocirun`; `ociman run/exec/ps/logs` rootless | **done** (plus systemd cgroups, hooks, seccomp, resource limits, `--security-opt seccomp=`, a real `podman`-default capability set, `--cap-add`/`--cap-drop`, `--privileged`, `--read-only`, `-e`/`--env`, `--hostname`, `-w`/`--workdir`, `ocirun features`, beyond the original scope) |
| 4 | `oci-dockerfile`; `ociman build` (multi-stage, cache) | in progress — `RUN`/`COPY`/`ADD`/`--build-arg`/`--target` work end to end and commit real layers, including multiple explicit sources, glob patterns, `COPY --from=<external-image>`, `ADD` from a remote URL, multiple `ARG` names on one line, and `COPY`/`ADD --chmod` (the build cache is not yet implemented) (see `docs/design/0050`-`0060`, `0068`, `0072`-`0079`) |
| 5 | erofs/mount/BLS; `ociboot install to-disk`; dracut module; QEMU boot test | in progress — `oci-erofs` builds real, verified-deterministic erofs images via `mkfs.erofs`, seals/verifies them with real fs-verity ioctls, and has a detached dm-verity fallback via `veritysetup`; `oci-bls` reads/writes the real grubenv block and Type #1 BLS entries, scans `/loader/entries/` as a directory, implements the real spec's own boot-counting filename convention, and sorts entries per the real spec's own "Sorting" section including full UAPI.10 version comparison (all verified against the real uapi-group spec/tools); `oci-mount` attaches/detaches real loop devices (see `docs/design/0061`-`0066`, `0070`-`0071`); the `boot_success` grubenv protocol, actually mounting a verified image at boot, `ociboot`'s own subcommands, and the dracut module are not started yet |
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
