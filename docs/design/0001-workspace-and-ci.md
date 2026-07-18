# Design note 0001: workspace skeleton and CI (milestone 1)

Status: implemented
Scope: workspace layout, `oci-cli-common`, the 4-VM CI matrix, repo guards.

## Goals

Milestone 1 delivers a compiling, tested, linted workspace with the final crate
topology in place, plus CI that already runs the full 4-cell VM matrix
(CentOS Stream 10 / Ubuntu 26.04 x x86_64 / aarch64) so every later milestone
inherits working infrastructure.

## Workspace layout

One Cargo workspace, resolver v3, edition 2024, pinned stable toolchain in
`rust-toolchain.toml` (MSRV == pinned stable; bumped freely).

* `crates/*` - shared libraries only. All real logic lives here. Milestone 1
  ships `oci-cli-common` (real) and `oci-build-info` (real, build-script
  helper), plus documented stubs for the other crates so the dependency
  topology and CI guards are exercised from day one.
* `bin/*` - six thin binaries. In milestone 1 they only parse a CLI
  (`--log-level`, `--json`, `--version` with embedded git hash) and exit.
  `bin/*` crates may depend on `crates/*` only, never on each other
  (enforced by `ci/guards.py`).
* `tests/` - the `oci-tools-tests` package: cross-binary integration tests.
  Requires a prior `cargo build --workspace` (CI does this); tests locate
  sibling binaries via the test executable's target directory.

Shared dependency versions live in `[workspace.dependencies]`; crates opt into
`[workspace.lints]`. Warnings are allowed locally, denied in CI
(`RUSTFLAGS=-Dwarnings`, `clippy -- -D warnings`).

### `oci-cli-common`

* `GlobalArgs` (clap `Args`): `--log-level <FILTER>` (tracing `EnvFilter`
  syntax, env `OCI_TOOLS_LOG`, default `warn`), `--json`.
* `logging::init` - tracing-subscriber to stderr; logs never pollute stdout,
  which is reserved for command output (JSON mode included).
* `output` - `print_json` / `print_json_compact` for `--json` mode.
* `error::run_main` - uniform `error: ...` + `caused by: ...` chain rendering
  and exit-code mapping, used by every binary's `main`.
* `version` - `long(pkg_version)` returns `X.Y.Z (git <hash>)`.
* `progress` - shared indicatif styles (hidden automatically off-tty).

### `oci-build-info`

Build-dependency crate: emits `cargo:rustc-env=OCI_TOOLS_GIT_HASH=...` by
reading `.git/HEAD` (+ ref file / `packed-refs`) directly, falling back to the
`OCI_TOOLS_GIT_HASH` env var (source tarball / packaging builds) and finally
`unknown`. Shared by `oci-cli-common` and `ociboot-init` (which must stay tiny
and cannot depend on `oci-cli-common`'s clap/tracing stack).

## CI

Workflow `.github/workflows/ci.yml`:

* **lint** (ubuntu-24.04): rustfmt check, clippy `-D warnings` on all targets,
  `cargo-deny check`, `ci/guards.py`.
* **vm-test** - exactly 4 parallel jobs, no cross-compilation; the VM arch
  always equals the runner arch:

  | base            | arch    | runner            |
  |-----------------|---------|-------------------|
  | centos-stream10 | x86_64  | ubuntu-24.04      |
  | centos-stream10 | aarch64 | ubuntu-24.04-arm  |
  | ubuntu-26.04    | x86_64  | ubuntu-24.04      |
  | ubuntu-26.04    | aarch64 | ubuntu-24.04-arm  |

### VM harness (`ci/`)

Reusable outside GitHub Actions; plain bash + QEMU:

* `ci/setup-host.sh` - installs qemu/UEFI firmware/cloud-image-utils on the
  runner, widens `/dev/kvm` permissions.
* `ci/vm.sh` - generic cloud-image VM driver: `up | run | push | pull | down`.
  Downloads the cloud image, makes a qcow2 overlay (base stays pristine),
  builds a NoCloud seed ISO (fresh ed25519 key, `ci` user, passwordless sudo),
  boots QEMU with UEFI firmware (OVMF on x86_64, QEMU_EFI on aarch64),
  user-mode networking with an ssh hostfwd on 127.0.0.1. KVM when `/dev/kvm`
  is usable, TCG fallback otherwise (`-cpu host` vs `-cpu max`). `push`/`pull`
  are tar-over-ssh so the guest needs no rsync. `down` powers off via ssh and
  waits for QEMU to exit so the cache disk is flushed.
* `ci/run-in-vm.sh` - maps (base, arch) to an image URL (overridable via
  `OCI_CI_IMAGE_URL`), boots the VM with a persistent **cache disk**, pushes
  the working tree, runs `ci/vm-ci.sh` inside, pulls `artifacts/`, dumps the
  serial console on failure.
* `ci/vm-ci.sh` (runs inside the guest) - formats/mounts the cache disk
  (ext4, label `ocicache`) at `/mnt/cache`, installs build deps via dnf or
  apt, installs rustup with `RUSTUP_HOME`/`CARGO_HOME`/`CARGO_TARGET_DIR` on
  the cache disk, then `cargo build`, `cargo test`, `cargo build --release`
  (all `--workspace --locked`) and exports the release binaries.

Caching: the cache disk qcow2 (cargo registry, rustup, target dir) is stored
with `actions/cache`, keyed `(base, arch, hash(rust-toolchain.toml,
Cargo.lock))` with a prefix restore key. The base cloud image is re-downloaded
each run ("latest" images mutate in place; correctness over speed - revisit if
it becomes a bottleneck).

Artifacts: the six release binaries per matrix cell.

### Guards (`ci/guards.py`)

1. **Forbidden filesystems**: `(btrfs|zfs)` (word-bounded, case-insensitive)
   must not appear in any tracked file outside `docs/`, `*.md`, `LICENSE`, and
   the guard itself. Includes `Cargo.lock`, so a stray dependency trips it.
2. **No bin->bin dependencies**: via `cargo metadata`, no `bin/*` package may
   depend on another `bin/*` package (direct or via re-export path); shared
   code must live in `crates/*`.
3. **One crate per function**: exactly one workspace-declared dependency per
   curated capability group (tar, http client, CLI parser, error derive,
   digest impl, ...). The table lives in the guard and grows as crates are
   adopted. This checks *direct* dependencies of workspace members - what we
   choose, not what transitive deps drag in (cargo-deny's `multiple-versions`
   watches those).

## Decisions and risks

* **Firmware, corrected by inspection**: the CentOS Stream 10 GenericCloud
  x86_64 image is **BIOS-boot-only** (GPT with a 1M BIOS-boot partition, no
  ESP — under OVMF it PXE-loops into the EFI shell), while Ubuntu amd64
  images are hybrid. So x86_64 defaults to SeaBIOS (`VM_FIRMWARE=uefi`
  switches to OVMF for UEFI-only disks, e.g. the milestone-5 ociboot boot
  test); aarch64 has no BIOS and always uses `QEMU_EFI.fd`. NIC PXE option
  ROMs are disabled (`romfile=`): we always boot from disk, and the ROM
  package (`ipxe-qemu`) is only a Recommends of `qemu-system-arm` — its
  absence otherwise aborts QEMU startup on the arm runners (found the hard
  way).
* **CentOS Stream 10 requires x86-64-v3**: fine under KVM (`-cpu host`,
  runners have AVX2) and under TCG with `-cpu max` (QEMU >= 7.2 implements
  AVX2; ubuntu-24.04 ships 8.2).
* **GitHub arm64 hosted runners have no /dev/kvm** (confirmed on
  ubuntu-24.04-arm): the aarch64 cells run under multi-threaded TCG with
  `tb-size=1024` and `pauth-impdef=on`, get a 2400 s boot timeout and a
  300-minute job timeout, and lean hard on the cache disk. If GitHub ever
  enables KVM there, the harness picks it up automatically.
  `VM_FORCE_TCG=1` reproduces the runner behavior locally.
* **Cache keys end in `run_id`**: `actions/cache` never overwrites an
  existing key, so an exact-hash key would freeze the first (cold or failed)
  cache forever; unique keys + prefix restore-keys keep the newest warm disk.
* **Image URLs are pinned to "latest"** symlinks (CentOS) / release paths
  (Ubuntu 26.04) and validated at time of writing; `OCI_CI_IMAGE_URL`
  overrides without a workflow change.
* `ociboot-init` prints its version without clap and stays dependency-free so
  it can later be built as a tiny static (musl) initramfs binary.
