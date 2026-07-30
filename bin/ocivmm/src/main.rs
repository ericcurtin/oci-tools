//! `ocivmm` — pet microVMs from OCI images.
//!
//! Creates long-lived pet *virtual machines* the same way `ocibox`
//! creates pet containers: `ocivmm run ubuntu:26.04` resolves/pulls
//! the image, provisions it with the distro's own kernel and systemd,
//! and boots it — everything installed or written inside the guest
//! persists across runs, the "pet" model.
//!
//! The VMM is `oci-vmm`, this workspace's own KVM/virtio-pci monitor
//! (ported from Firecracker; see `crates/oci-vmm`) — statically
//! linked, nothing dynamically loaded, nothing dlopen'd. It has no
//! virtio-fs device at all (a deliberate scope cut: virtio-pci +
//! virtio-blk + virtio-net covers everything a stock distro kernel
//! needs to boot and get online), so a pet VM's root filesystem is a
//! plain **ext4 disk image**, not a shared host directory:
//!
//! * `create` extracts the OCI image into a scratch directory, runs
//!   the distro's own package manager in it *as a container*
//!   (`oci_runtime_core::launch`, the same machinery `ocibox enter`
//!   uses) to install its own kernel, dracut, and systemd, then builds
//!   `rootfs.img` from that directory with `mkfs.ext4 -d` and deletes
//!   the scratch directory — the image *is* the pet VM from then on.
//! * `run` loop-mounts the image read-only just long enough to copy
//!   out the guest's own `/boot/vmlinuz-*` + initramfs (the *host*
//!   VMM loads these directly; `linux-loader` needs plain files, not
//!   a mounted filesystem) into a small cache, then boots with the
//!   image itself as the virtio-blk root disk. No command → an
//!   autologin root console; a command → a generated oneshot systemd
//!   unit whose exit status is written back into the image and read
//!   back by loop-mounting it again once the guest has powered off.
//! * `cp` copies files into or out of a (stopped) pet VM's image by
//!   loop-mounting it — the replacement for live directory sharing
//!   (`--volume`), which virtio-blk-only has no equivalent of; see
//!   its own doc comment for exactly what this trades away.
//!
//! Networking is a passt-backed virtio-net device; the guest's own
//! DHCP client does the negotiating against passt (`systemd-networkd`
//! where the distro ships it, `NetworkManager` where it doesn't — see
//! `PROVISION_CONFIGURE`), and `--publish` maps host ports via
//! passt's own `-t` forwarding.
//!
//! Host requirements (run time only — building `ocivmm` needs
//! nothing): `/dev/kvm`, the `passt` package, and `mkfs.ext4`
//! (e2fsprogs — already a dependency of every distro this project
//! packages for). Guest uid/gid fidelity (package managers chown what
//! they install, both in the provisioning container and in the
//! finished image) wants real root — run `ocivmm` as root for full
//! pet-distro behavior, the way CI does.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clap::Parser;
use oci_spec_types::Reference;
use oci_store::Store;
use serde::{Deserialize, Serialize};

mod disk;

/// Command-line interface.
#[derive(Debug, Parser)]
#[command(
    name = "ocivmm",
    about = "Pet microVMs from OCI images",
    version = oci_cli_common::version::long(env!("CARGO_PKG_VERSION")),
)]
struct Cli {
    #[command(flatten)]
    global: oci_cli_common::GlobalArgs,

    #[command(subcommand)]
    command: Option<Command>,
}

/// Default guest RAM in MiB when `--mem` is not given.
const DEFAULT_MEM_MIB: u32 = 4096;

/// Default disk image size in MiB when creating a pet VM — generous
/// headroom for a full `dnf`/`apt` toolchain plus whatever the guest
/// installs later; the file is sparse, so this costs nothing upfront.
const DEFAULT_DISK_MIB: u64 = 20_480;

/// Where the guest command's exit status lands inside the image
/// (written by the generated oneshot unit's `ExecStopPost`, read back
/// by the host via a loop-mount once the guest has powered off).
const EXIT_STATUS_FILE: &str = ".ocivmm-exit-status";

/// The generated per-run oneshot unit's own name.
const RUN_UNIT: &str = "ocivmm-run.service";

/// Subcommands shipped so far.
#[derive(Debug, clap::Subcommand)]
enum Command {
    /// Boot a pet VM. `TARGET` is either the name of an existing VM or
    /// an image reference: `ocivmm run ubuntu:26.04` creates (pulling
    /// the image and provisioning its own kernel + systemd) a VM named
    /// `ubuntu-26.04` on first use and simply boots the same,
    /// persistent disk image on every use after that. With no
    /// `COMMAND`, boots to a root login on the console; with one, runs
    /// it as a oneshot systemd unit, powers off, and exits with its
    /// status.
    Run {
        /// Existing VM name, or an image reference to create one from.
        target: String,
        /// The command to run inside the VM, and its arguments —
        /// omitted, the VM boots to an interactive root console.
        /// Everything after `TARGET` belongs to the command (docker
        /// `run`'s own convention), so no `--` separator is needed.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
        /// Name for the VM (defaults to a name derived from the image
        /// reference, e.g. `centos-stream10` for `centos:stream10`).
        #[arg(long = "name", short = 'n', value_name = "NAME")]
        name: Option<String>,
        /// Number of vCPUs (defaults to every host CPU).
        #[arg(long)]
        cpus: Option<u8>,
        /// Guest RAM in MiB.
        #[arg(long, value_name = "MIB")]
        mem: Option<u32>,
        /// Map a host port to a guest port (`HOST:GUEST`, repeatable).
        #[arg(long = "publish", short = 'p', value_name = "HOST:GUEST")]
        publish: Vec<String>,
        /// Extra `NAME=value` guest environment entries for `COMMAND`
        /// (repeatable); override the image's own declared environment.
        #[arg(long = "env", short = 'e', value_name = "NAME=VALUE")]
        env: Vec<String>,
        /// Working directory for `COMMAND` (defaults to /root).
        #[arg(long, value_name = "DIR")]
        workdir: Option<String>,
        /// Pull the image even if a local copy already exists (only
        /// meaningful when the VM doesn't exist yet — an existing pet
        /// VM's disk image is never silently replaced).
        #[arg(long)]
        pull: bool,
    },
    /// Create (and provision) a pet VM without leaving it running:
    /// resolves `--image`, extracts it, runs the distro's own package
    /// manager in it as a container to install its own kernel and
    /// systemd, then builds the pet VM's `rootfs.img`.
    Create {
        /// Image reference to base the VM on.
        #[arg(long = "image", short = 'i', value_name = "REFERENCE")]
        image: String,
        /// Name for the VM (defaults to a name derived from the image
        /// reference, e.g. `ubuntu-26.04` for `ubuntu:26.04`).
        #[arg(long = "name", short = 'n', value_name = "NAME")]
        name: Option<String>,
        /// Pull `--image` even if a local copy already exists.
        #[arg(long, short = 'p')]
        pull: bool,
        /// The disk image size in MiB.
        #[arg(long, value_name = "MIB")]
        disk_mib: Option<u64>,
    },
    /// List created VMs (name, image, creation time), sorted by name.
    #[command(alias = "ls")]
    List,
    /// Remove one or more VMs entirely (each one's disk image and
    /// persisted record).
    Rm {
        /// The VM name(s), exactly as shown by `ocivmm list`. At least
        /// one, or `--all`, is required. Every given name must resolve
        /// to a real, existing VM before anything is actually removed
        /// -- an unresolvable one aborts the whole call rather than
        /// removing only some of the given names, the same "resolve
        /// everything first" convention this workspace's own `ociman
        /// rm`/`kill`/`stop` multi-target support already established
        /// (`docs/design/0310`-`0318`).
        names: Vec<String>,
        /// Remove every existing VM. Mutually exclusive with any
        /// positional `names`.
        #[arg(long, short = 'a')]
        all: bool,
    },
    /// Copy a file or directory into or out of a pet VM's disk image
    /// by loop-mounting it — the replacement for live directory
    /// sharing (a virtio-blk-only VMM has no virtiofs `--volume`
    /// equivalent; this trades a live, shared view for an explicit,
    /// one-shot copy, docker-`cp`-style).
    /// Exactly one of `SRC`/`DST` must be `VMNAME:PATH`; the VM must
    /// not currently be running.
    Cp {
        /// Source: a host path, or `VMNAME:PATH` inside the image.
        src: String,
        /// Destination: a host path, or `VMNAME:PATH` inside the image.
        dst: String,
    },
    /// Hidden: become the VMM for a spec prepared by the parent
    /// `ocivmm` process. [`oci_vmm::run`] turns its caller into the
    /// VMM (it never returns; the process exits when the guest powers
    /// off), so `run` — which must keep running to loop-mount the
    /// image and read the exit-status file back — boots through this
    /// re-exec'd child instead. The same self-re-exec technique
    /// `ocicri`'s own `__launch` uses.
    #[command(name = "__boot", hide = true)]
    Boot {
        /// Path of the serialized [`BootSpec`] JSON.
        spec: PathBuf,
    },
}

fn main() -> std::process::ExitCode {
    oci_cli_common::run_main(|| {
        let cli = Cli::parse();
        oci_cli_common::logging::init(&cli.global)?;
        tracing::debug!(
            git_hash = oci_cli_common::version::GIT_HASH,
            "ocivmm starting"
        );
        match cli.command {
            Some(Command::Run {
                target,
                command,
                name,
                cpus,
                mem,
                publish,
                env,
                workdir,
                pull,
            }) => cmd_run(&RunRequest {
                target,
                command,
                name,
                cpus,
                mem,
                publish,
                env,
                workdir,
                pull,
            }),
            Some(Command::Create {
                image,
                name,
                pull,
                disk_mib,
            }) => cmd_create(
                &image,
                name.as_deref(),
                pull,
                disk_mib.unwrap_or(DEFAULT_DISK_MIB),
            ),
            Some(Command::List) => cmd_list(cli.global.json),
            Some(Command::Rm { names, all }) => cmd_rm(&names, all),
            Some(Command::Cp { src, dst }) => cmd_cp(&src, &dst),
            Some(Command::Boot { spec }) => cmd_boot(&spec),
            None => anyhow::bail!("no subcommand given (try `ocivmm run ubuntu:26.04`)"),
        }
    })
}

/// Where every VM's own on-disk state lives.
fn vms_root() -> PathBuf {
    oci_cli_common::storage::default_root().join("vms")
}

/// A conservative charset check matching real `docker`/`podman`'s own
/// `--name` convention.
fn validate_vm_name(name: &str) -> anyhow::Result<()> {
    let valid = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    if !valid {
        anyhow::bail!(
            "invalid VM name {name:?}: must start with a letter or digit and contain only \
             letters, digits, '_', '.', or '-' afterward"
        );
    }
    Ok(())
}

/// Derive a friendly default VM name from an image reference:
/// `ubuntu:26.04` -> `ubuntu-26.04`, `quay.io/centos/centos:stream10`
/// -> `centos-stream10`.
fn derive_vm_name(reference: &Reference) -> String {
    let base = reference
        .repository()
        .rsplit('/')
        .next()
        .unwrap_or("vm")
        .to_string();
    let name = match reference.tag() {
        Some(tag) if tag != "latest" => format!("{base}-{tag}"),
        _ => base,
    };
    let mut sanitized: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    if !sanitized
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric())
    {
        sanitized = format!("vm-{sanitized}");
    }
    sanitized
}

/// A VM's own persisted metadata (`<vms_root>/<name>/vm.json`). The
/// kernel/initramfs are never recorded here: they belong to the guest
/// (its own package manager installs and upgrades them), so every
/// boot re-detects the newest ones from the image instead of trusting
/// a stale record.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct VmRecord {
    name: String,
    image: String,
    manifest_digest: String,
    created: String,
    /// The source image's own declared default environment (used for
    /// the generated oneshot unit's Environment= lines).
    #[serde(default)]
    env: Vec<String>,
}

/// Fallback `PATH` for a VM whose source image declared no default
/// `env` at all — matching real `podman`'s identical fallback.
const DEFAULT_ENV_WHEN_VM_DECLARES_NONE: &str =
    "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// The real create logic `cmd_create` and `cmd_run`'s create-on-first-
/// use path share: resolve/pull the image, extract it to a scratch
/// directory, provision the distro's own kernel and systemd into it
/// (as a container), then image it into `rootfs.img`. Any failure
/// removes the half-created VM directory.
fn create_vm(image: &str, name: &str, pull: bool, disk_mib: u64) -> anyhow::Result<VmRecord> {
    validate_vm_name(name)?;

    let vm_dir = vms_root().join(name);
    anyhow::ensure!(
        !vm_dir.exists(),
        "{name}: a VM with this name already exists"
    );

    let reference =
        Reference::parse(image).with_context(|| format!("parsing image reference {image:?}"))?;
    let store =
        Store::open(oci_cli_common::storage::default_root()).context("opening image storage")?;

    let pull_policy = if pull {
        oci_registry::PullPolicy::Always
    } else {
        oci_registry::PullPolicy::Missing
    };
    let record = oci_registry::resolve_or_pull(
        &store,
        &reference,
        pull_policy,
        true,
        &oci_spec_types::image::Platform::host(),
        || {
            oci_registry::pull_unconditionally(
                &store,
                &reference,
                true,
                &oci_spec_types::image::Platform::host(),
            )
        },
    )
    .with_context(|| format!("resolving {reference}"))?;

    let manifest = store
        .image_manifest(&record)
        .with_context(|| format!("reading manifest for {reference}"))?;
    let config = store
        .image_config(&record)
        .with_context(|| format!("reading config for {reference}"))?;
    let container_config = config.config.unwrap_or_default();

    std::fs::create_dir_all(&vm_dir).with_context(|| format!("creating {}", vm_dir.display()))?;
    // Named "rootfs" (not "scratch"), even though it's deleted right
    // after imaging: `oci_spec_types::runtime::Spec::example()`'s own
    // `root.path` is the literal string "rootfs", relative to the
    // bundle directory (`vm_dir`) `provision_vm` loads as a `Bundle`.
    let scratch = vm_dir.join("rootfs");
    std::fs::create_dir_all(&scratch).with_context(|| format!("creating {}", scratch.display()))?;

    let vm_record = VmRecord {
        name: name.to_string(),
        image: reference.to_string(),
        manifest_digest: record.manifest_digest.to_string(),
        created: oci_spec_types::time::format_rfc3339_utc(std::time::SystemTime::now()),
        env: container_config.env,
    };
    let result = extract_rootfs(&store, &manifest, &scratch)
        .and_then(|()| ensure_guest_files(&scratch, name))
        .and_then(|()| reset_resolv_conf(&scratch))
        .and_then(|()| provision_vm(&scratch, name))
        .and_then(|()| disk::build_ext4_image(&scratch, &vm_dir.join("rootfs.img"), disk_mib))
        .and_then(|()| {
            let vm_json_path = vm_dir.join("vm.json");
            std::fs::write(
                &vm_json_path,
                serde_json::to_vec_pretty(&vm_record).context("serializing VM record")?,
            )
            .with_context(|| format!("writing {}", vm_json_path.display()))
        });
    let _ = std::fs::remove_dir_all(&scratch);
    if result.is_err() {
        // Never leave a half-created VM directory lying around for a
        // later create of the same name to trip over — best effort,
        // the original error is what gets reported either way.
        let _ = std::fs::remove_dir_all(&vm_dir);
    }
    result?;

    Ok(vm_record)
}

/// Extract every one of `manifest`'s layers, bottom-first, into
/// `dest`.
fn extract_rootfs(
    store: &Store,
    manifest: &oci_spec_types::image::ImageManifest,
    dest: &Path,
) -> anyhow::Result<()> {
    for layer in &manifest.layers {
        let compression = oci_layer::compression_for_media_type(&layer.media_type)
            .with_context(|| format!("unsupported layer media type {:?}", layer.media_type))?;
        let blob = store
            .open_blob(&layer.digest)
            .with_context(|| format!("opening layer blob {}", layer.digest))?;
        oci_layer::apply(blob, compression, dest)
            .with_context(|| format!("extracting layer {}", layer.digest))?;
    }
    Ok(())
}

/// The distro-specific half of the provisioning script: install the
/// distro's own kernel, dracut, kmod, and systemd with the distro's
/// own package manager. Distro differences are data, not logic. On
/// the apt side dracut is installed *before* the kernel so it both
/// satisfies `linux-image-*`'s `initramfs-tools | linux-initramfs-tool`
/// alternative and owns the kernel's initramfs hooks.
///
/// `NetworkManager` on the dnf side, `systemd-networkd` on the apt
/// side, not the same tool on both: CentOS Stream doesn't ship a
/// `systemd-networkd` package at all (confirmed directly — `dnf list
/// systemd-networkd` reports no matching package; RHEL-family systems
/// use NetworkManager as their native DHCP client instead), so
/// `PROVISION_CONFIGURE` below branches on which one is actually
/// available rather than assuming either universally.
#[cfg(target_os = "linux")]
const PROVISION_PACKAGES: &str = r#"
if command -v dnf >/dev/null 2>&1; then
    dnf -y --setopt=install_weak_deps=False install \
        kernel dracut kmod systemd systemd-resolved dbus-broker util-linux NetworkManager
elif command -v apt-get >/dev/null 2>&1; then
    export DEBIAN_FRONTEND=noninteractive
    apt-get update
    apt-get install -y --no-install-recommends \
        systemd systemd-sysv systemd-resolved dbus kmod dracut
    apt-get install -y --no-install-recommends \
        linux-image-virtual linux-image-extra-virtual
else
    echo 'ocivmm provision: no supported package manager (need dnf or apt-get)' >&2
    exit 1
fi
"#;

/// The distro-independent half: a dracut initramfs able to mount the
/// virtio-blk root device directly (`root=/dev/vda`, no virtiofs — the
/// VMM has no filesystem-sharing device at all), DHCP for the
/// passt-backed virtio-net device (`systemd-networkd` where it
/// exists, `NetworkManager` where it doesn't — see `PROVISION_PACKAGES`),
/// and a root autologin on the serial console systemd's getty-generator
/// spawns for `console=ttyS0`.
#[cfg(target_os = "linux")]
const PROVISION_CONFIGURE: &str = r#"
kver=$(ls /lib/modules | sort -V | tail -n 1)
[ -n "$kver" ] || { echo 'ocivmm provision: no kernel modules installed' >&2; exit 1; }
dracut --force --no-hostonly "/boot/ocivmm-initrd-$kver.img" "$kver"

mkdir -p /etc/systemd/network \
    /etc/systemd/system/multi-user.target.wants \
    /etc/systemd/system/network-online.target.wants \
    '/etc/systemd/system/serial-getty@ttyS0.service.d'

if [ -e /usr/lib/systemd/system/systemd-networkd.service ]; then
    cat > /etc/systemd/network/20-ocivmm.network <<'EOF'
[Match]
Name=e*

[Network]
DHCP=yes
EOF
    ln -sf /usr/lib/systemd/system/systemd-networkd.service \
        /etc/systemd/system/multi-user.target.wants/systemd-networkd.service
    ln -sf /usr/lib/systemd/system/systemd-networkd-wait-online.service \
        /etc/systemd/system/network-online.target.wants/systemd-networkd-wait-online.service
    # networkd itself doesn't manage DNS; hand that to resolved, whose
    # own stub /etc/resolv.conf picks up the DHCP-provided nameserver.
    ln -sf /usr/lib/systemd/system/systemd-resolved.service \
        /etc/systemd/system/multi-user.target.wants/systemd-resolved.service
    ln -sfn ../run/systemd/resolve/resolv.conf /etc/resolv.conf
else
    ln -sf /usr/lib/systemd/system/NetworkManager.service \
        /etc/systemd/system/multi-user.target.wants/NetworkManager.service
    ln -sf /usr/lib/systemd/system/NetworkManager-wait-online.service \
        /etc/systemd/system/network-online.target.wants/NetworkManager-wait-online.service
    # Neither NetworkManager's own DNS management (dns=default,
    # rc-manager=file: tried, confirmed over several real CI runs to
    # leave /etc/resolv.conf empty anyway) nor systemd-resolved proved
    # reliable at actually getting a DHCP-provided nameserver into
    # /etc/resolv.conf at all -- `ci/vm-prepare.sh` writes it directly
    # instead, from the DHCP-assigned default gateway (passt always
    # serves its own DNS proxy there), the one thing that reliably is
    # correct by the time our own unit's `After=network-online.target`
    # is satisfied. dns=none here so NetworkManager never touches the
    # file at all and can't reclaim it later.
    mkdir -p /etc/NetworkManager/conf.d
    cat > /etc/NetworkManager/conf.d/10-ocivmm-dns.conf <<'EOF'
[main]
dns=none
EOF
fi

cat > '/etc/systemd/system/serial-getty@ttyS0.service.d/autologin.conf' <<'EOF'
[Service]
ExecStart=
ExecStart=-/sbin/agetty --autologin root --keep-baud 115200,57600,38400,9600 %I $TERM
EOF

echo 'ocivmm provision: done'
"#;

/// Provision a freshly extracted rootfs (a plain directory, not yet
/// imaged) with the distro's own kernel and systemd, by running the
/// provisioning script *as a container on it* — the same shared
/// `oci_runtime_core::launch`/`Bundle`/`validate` lifecycle `ocibox
/// enter` uses (host network kept: the package manager needs the
/// registry mirrors). Images with no `dnf`/`apt-get` (alpine,
/// distroless) are a clear, upfront error.
#[cfg(target_os = "linux")]
fn provision_vm(rootfs: &Path, name: &str) -> anyhow::Result<()> {
    let has_pkg_manager = ["usr/bin/dnf", "usr/bin/apt-get", "bin/apt-get"]
        .iter()
        .any(|p| rootfs.join(p).exists());
    anyhow::ensure!(
        has_pkg_manager,
        "the image has neither dnf nor apt-get, so it cannot install its own kernel and \
         systemd — only real distro images (e.g. centos:stream10, ubuntu:26.04) can \
         become ocivmm VMs"
    );
    let (euid, egid) = oci_cli_common::identity::effective_uid_gid();
    if euid != 0 {
        eprintln!(
            "ocivmm: warning: provisioning rootless; the distro's package manager may fail \
             to chown what it installs (run as root for full fidelity)"
        );
    }

    eprintln!("ocivmm: provisioning distro kernel + systemd (containerized package install)");
    let script = format!("set -e\n{PROVISION_PACKAGES}\n{PROVISION_CONFIGURE}");

    let mut spec = oci_spec_types::runtime::Spec::example();
    if euid != 0 {
        spec = spec.into_rootless(euid, egid);
    }
    // The package manager needs host network (into_rootless already
    // drops the network namespace for the rootless case). No seccomp
    // filter: this is our own trusted provisioning script.
    if let Some(linux) = spec.linux.as_mut() {
        linux
            .namespaces
            .retain(|ns| !matches!(ns.kind, oci_spec_types::runtime::NamespaceType::Network));
        linux.seccomp = None;
    }
    spec.root
        .as_mut()
        .expect("Spec::example always sets root")
        .readonly = false;
    let process = spec
        .process
        .as_mut()
        .expect("Spec::example always sets process");
    process.args = vec!["/bin/sh".to_string(), "-c".to_string(), script];
    process.terminal = false;
    process.cwd = "/".to_string();
    process.env = vec![
        DEFAULT_ENV_WHEN_VM_DECLARES_NONE.to_string(),
        "HOME=/root".to_string(),
    ];
    if let Some(capabilities) = process.capabilities.as_mut() {
        let podman_caps = oci_spec_types::runtime::podman_default_capabilities();
        capabilities.bounding = podman_caps.clone();
        capabilities.effective = podman_caps.clone();
        capabilities.permitted = podman_caps;
    }

    let config_path = rootfs
        .parent()
        .expect("scratch dir has a parent")
        .join(oci_runtime_core::bundle::CONFIG_FILENAME);
    std::fs::write(&config_path, serde_json::to_vec_pretty(&spec)?)
        .with_context(|| format!("writing {}", config_path.display()))?;
    let bundle_dir = rootfs.parent().expect("scratch dir has a parent");
    let result = (|| -> anyhow::Result<()> {
        let bundle = oci_runtime_core::Bundle::load(bundle_dir)
            .with_context(|| format!("loading bundle from {}", bundle_dir.display()))?;
        let validated_rootfs = oci_runtime_core::validate::validate(&bundle)
            .context("provisioning config.json failed validation")?;
        // SAFETY: `ocivmm create` has not spawned any additional
        // threads by this point, matching `ocibox enter`'s identical
        // safety note for this same entry point. Stdin is closed (a
        // package install is never interactive); output passes
        // through for progress.
        #[allow(unsafe_code)]
        let exit_code = unsafe {
            // `preserve_fds: 0` -- `ocivmm` has no `--preserve-fds`
            // flag of its own. `no_pivot: false` -- `ocivmm` has no
            // `--no-pivot` flag either.
            oci_runtime_core::launch::run(
                &format!("ocivmm-provision-{name}"),
                &bundle,
                &validated_rootfs,
                true,
                false,
                0,
                false,
            )
        }
        .context("running the provisioning container")?;
        anyhow::ensure!(
            exit_code == 0,
            "provisioning the distro kernel + systemd failed (exit code {exit_code})"
        );
        Ok(())
    })();
    let _ = std::fs::remove_file(&config_path);
    result
}

/// Containers (and KVM) are Linux-only; everywhere else `create` fails
/// clearly before leaving half-provisioned state around.
#[cfg(not(target_os = "linux"))]
fn provision_vm(_rootfs: &Path, _name: &str) -> anyhow::Result<()> {
    anyhow::bail!("ocivmm can only provision and run VMs on Linux (KVM + containers)");
}

/// A boot spec handed to the re-exec'd `ocivmm __boot` child as JSON —
/// see [`Command::Boot`] for why a child.
#[derive(Debug, Serialize, Deserialize)]
struct BootSpec {
    cpus: u8,
    mem_mib: u32,
    kernel_path: PathBuf,
    initrd_path: Option<PathBuf>,
    cmdline: String,
    disk_path: PathBuf,
    disk_read_only: bool,
    net_mac: [u8; 6],
    /// passt's unix-stream socket path; the `__boot` child connects to
    /// it itself (already listening by the time this spec is written).
    passt_socket: PathBuf,
}

/// Boot `spec` in a re-exec'd `ocivmm __boot` child (stdio inherited)
/// and wait for it.
fn boot_in_child(vm_dir: &Path, spec: &BootSpec) -> anyhow::Result<std::process::ExitStatus> {
    let spec_path = vm_dir.join("boot-spec.json");
    std::fs::write(&spec_path, serde_json::to_vec_pretty(spec)?)
        .with_context(|| format!("writing {}", spec_path.display()))?;
    let exe = std::env::current_exe().context("resolving our own executable path")?;
    let status = std::process::Command::new(exe)
        .arg("__boot")
        .arg(&spec_path)
        .status()
        .context("spawning the ocivmm __boot child")?;
    let _ = std::fs::remove_file(&spec_path);
    Ok(status)
}

/// `ocivmm __boot`: the hidden VMM half — never returns on success.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn cmd_boot(spec_path: &Path) -> anyhow::Result<()> {
    let bytes = std::fs::read(spec_path)
        .with_context(|| format!("reading boot spec {}", spec_path.display()))?;
    let spec: BootSpec = serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing boot spec {}", spec_path.display()))?;
    let passt_socket = std::os::unix::net::UnixStream::connect(&spec.passt_socket)
        .with_context(|| format!("connecting to passt at {}", spec.passt_socket.display()))?;
    let config = oci_vmm::VmmConfig {
        vcpu_count: spec.cpus,
        mem_mib: spec.mem_mib,
        kernel_path: spec.kernel_path,
        initrd_path: spec.initrd_path,
        cmdline: spec.cmdline,
        disk_path: spec.disk_path,
        disk_read_only: spec.disk_read_only,
        net_mac: spec.net_mac,
        passt_socket,
    };
    match oci_vmm::run(config) {
        Ok(never) => match never {},
        Err(err) => anyhow::bail!("{err}"),
    }
}

/// KVM is Linux-only, and `oci-vmm` is x86_64-specific besides.
#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
fn cmd_boot(_spec_path: &Path) -> anyhow::Result<()> {
    anyhow::bail!("ocivmm can only run VMs on Linux/x86_64 (KVM)");
}

/// Read one VM's persisted [`VmRecord`] back from `vm.json`.
fn load_vm(name: &str) -> anyhow::Result<VmRecord> {
    let vm_json_path = vms_root().join(name).join("vm.json");
    let bytes = std::fs::read(&vm_json_path)
        .with_context(|| format!("reading {}", vm_json_path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", vm_json_path.display()))
}

fn cmd_create(image: &str, name: Option<&str>, pull: bool, disk_mib: u64) -> anyhow::Result<()> {
    let reference =
        Reference::parse(image).with_context(|| format!("parsing image reference {image:?}"))?;
    let name = match name {
        Some(name) => name.to_string(),
        None => derive_vm_name(&reference),
    };
    create_vm(image, &name, pull, disk_mib)?;
    println!("{name}");
    Ok(())
}

/// Every VM's persisted record, sorted by name.
fn list_vms() -> anyhow::Result<Vec<VmRecord>> {
    let root = vms_root();
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", root.display())),
    };
    let mut records = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let Ok(bytes) = std::fs::read(entry.path().join("vm.json")) else {
            continue;
        };
        if let Ok(record) = serde_json::from_slice::<VmRecord>(&bytes) {
            records.push(record);
        }
    }
    records.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(records)
}

fn cmd_list(json: bool) -> anyhow::Result<()> {
    let records = list_vms()?;
    if json {
        oci_cli_common::output::print_json(&records)?;
        return Ok(());
    }
    if records.is_empty() {
        println!("no VMs");
        return Ok(());
    }
    println!("{:<24} {:<50} {:<20}", "NAME", "IMAGE", "CREATED");
    for record in &records {
        println!(
            "{:<24} {:<50} {:<20}",
            record.name, record.image, record.created
        );
    }
    Ok(())
}

/// Remove exactly one VM's directory and print its name.
fn remove_one_vm(name: &str) -> anyhow::Result<()> {
    validate_vm_name(name)?;
    let vm_dir = vms_root().join(name);
    anyhow::ensure!(vm_dir.is_dir(), "{name}: no such VM");
    std::fs::remove_dir_all(&vm_dir).with_context(|| format!("removing {}", vm_dir.display()))?;
    println!("{name}");
    Ok(())
}

/// `ocivmm rm <NAME>` / `ocivmm rm --all`.
fn cmd_rm(names: &[String], all: bool) -> anyhow::Result<()> {
    match (names.is_empty(), all) {
        (false, true) => anyhow::bail!("cannot give both a VM name and --all"),
        (true, false) => anyhow::bail!("no VM name given (try `ocivmm rm <NAME>` or `--all`)"),
        (false, false) => remove_named_vms(names),
        (true, true) => {
            let mut first_error = None;
            for record in list_vms()? {
                if let Err(e) = remove_one_vm(&record.name) {
                    eprintln!("error removing {}: {e:#}", record.name);
                    first_error.get_or_insert(e);
                }
            }
            match first_error {
                Some(e) => Err(e.context("removing every VM")),
                None => Ok(()),
            }
        }
    }
}

/// Remove every one of `names` (`ocivmm rm` with one or more explicit
/// names, no `--all`): every name must resolve to a real, existing VM
/// first -- an unresolvable one aborts the whole call before removing
/// anything at all -- but once every one resolves, each is still
/// genuinely attempted regardless of an earlier one's own removal
/// failure (matching `--all`'s own identical resilience just above).
fn remove_named_vms(names: &[String]) -> anyhow::Result<()> {
    for name in names {
        validate_vm_name(name)?;
        anyhow::ensure!(vms_root().join(name).is_dir(), "{name}: no such VM");
    }
    let mut first_error = None;
    for name in names {
        if let Err(e) = remove_one_vm(name) {
            eprintln!("error removing {name}: {e:#}");
            first_error.get_or_insert(e);
        }
    }
    match first_error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// `ocivmm cp`: `SRC`/`DST` where exactly one side is `VMNAME:PATH`.
fn cmd_cp(src: &str, dst: &str) -> anyhow::Result<()> {
    match (parse_vm_path(src)?, parse_vm_path(dst)?) {
        (Some((name, inner)), None) => {
            let image = vms_root().join(&name).join("rootfs.img");
            disk::with_loop_mount(&image, true, |mountpoint| {
                copy_path(
                    &mountpoint.join(inner.trim_start_matches('/')),
                    Path::new(dst),
                )
            })
        }
        (None, Some((name, inner))) => {
            let image = vms_root().join(&name).join("rootfs.img");
            disk::with_loop_mount(&image, false, |mountpoint| {
                copy_path(
                    Path::new(src),
                    &mountpoint.join(inner.trim_start_matches('/')),
                )
            })
        }
        (Some(_), Some(_)) => anyhow::bail!("ocivmm cp: only one side may be VMNAME:PATH"),
        (None, None) => anyhow::bail!("ocivmm cp: one side must be VMNAME:PATH"),
    }
}

/// Parse a `cp` argument as `VMNAME:PATH`, if it looks like one (a
/// bare host path never contains `:` in practice on Linux).
fn parse_vm_path(arg: &str) -> anyhow::Result<Option<(String, String)>> {
    let Some((name, path)) = arg.split_once(':') else {
        return Ok(None);
    };
    if validate_vm_name(name).is_err() {
        return Ok(None);
    }
    anyhow::ensure!(
        vms_root().join(name).join("rootfs.img").is_file(),
        "{name}: no such VM"
    );
    Ok(Some((name.to_string(), path.to_string())))
}

/// Recursively copy `src` to `dst` (files or directories).
fn copy_path(src: &Path, dst: &Path) -> anyhow::Result<()> {
    if src.is_dir() {
        std::fs::create_dir_all(dst).with_context(|| format!("creating {}", dst.display()))?;
        for entry in std::fs::read_dir(src).with_context(|| format!("reading {}", src.display()))? {
            let entry = entry?;
            copy_path(&entry.path(), &dst.join(entry.file_name()))?;
        }
    } else {
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::copy(src, dst)
            .with_context(|| format!("copying {} to {}", src.display(), dst.display()))?;
    }
    Ok(())
}

/// Everything `ocivmm run` was asked for.
struct RunRequest {
    target: String,
    command: Vec<String>,
    name: Option<String>,
    cpus: Option<u8>,
    mem: Option<u32>,
    publish: Vec<String>,
    env: Vec<String>,
    workdir: Option<String>,
    pull: bool,
}

/// `ocivmm run`: resolve `TARGET` to a (possibly freshly created and
/// provisioned) pet VM, then boot it — exiting the process with the
/// guest command's own exit status.
fn cmd_run(request: &RunRequest) -> anyhow::Result<()> {
    let record = resolve_or_create_vm(request)?;
    let vm_dir = vms_root().join(&record.name);
    let image = vm_dir.join("rootfs.img");
    anyhow::ensure!(
        image.is_file(),
        "{}: VM record exists but its disk image is missing (remove it with `ocivmm rm`)",
        record.name
    );

    // Checked here rather than left to the VMM: krun-vmm's Kvm setup
    // panics (not errors) on a missing /dev/kvm.
    anyhow::ensure!(
        Path::new("/dev/kvm").exists(),
        "/dev/kvm not found; ocivmm microVMs need KVM"
    );

    let kernel = disk::find_guest_kernel(&vm_dir, &image)?.with_context(|| {
        format!(
            "{}: no bootable kernel found in this VM's image",
            record.name
        )
    })?;

    let ports = parse_ports(&request.publish)?;
    let passt_socket = spawn_passt(&vm_dir, &ports)?;

    let interactive = request.command.is_empty();
    let exit_file_rel = EXIT_STATUS_FILE;
    if !interactive {
        let env = unit_env(request, &record);
        let workdir = request.workdir.clone().unwrap_or_else(|| "/root".into());
        let unit = systemd_unit(&request.command, &env, workdir.as_str())?;
        disk::with_loop_mount(&image, false, |mountpoint| {
            let _ = std::fs::remove_file(mountpoint.join(exit_file_rel));
            write_run_unit(mountpoint, &unit)
        })?;
    } else {
        disk::with_loop_mount(&image, false, remove_run_unit)?;
    }

    let spec = BootSpec {
        cpus: request.cpus.unwrap_or_else(default_cpus),
        mem_mib: request.mem.unwrap_or(DEFAULT_MEM_MIB),
        kernel_path: kernel.vmlinuz,
        initrd_path: kernel.initramfs,
        cmdline: kernel_cmdline(),
        disk_path: image.clone(),
        disk_read_only: false,
        net_mac: [0x5a, 0x94, 0xef, 0xe4, 0x0c, 0xee],
        passt_socket,
    };

    eprintln!(
        "ocivmm: booting {} (image {}, kernel {}, {} vcpu(s), {} MiB)",
        record.name,
        record.image,
        spec.kernel_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        spec.cpus,
        spec.mem_mib
    );
    let status = boot_in_child(&vm_dir, &spec)?;
    let code = if interactive {
        status.code().unwrap_or(1)
    } else {
        anyhow::ensure!(status.success(), "the VM exited abnormally ({status})");
        let raw = disk::with_loop_mount(&image, true, |mountpoint| {
            std::fs::read_to_string(mountpoint.join(exit_file_rel)).with_context(|| {
                "the guest powered off without reporting a command status".to_string()
            })
        })?;
        disk::with_loop_mount(&image, false, |mountpoint| {
            let _ = std::fs::remove_file(mountpoint.join(exit_file_rel));
            Ok(())
        })?;
        parse_exit_status(&raw)
    };
    std::process::exit(code);
}

/// Resolve `run`'s `TARGET`: an existing VM name wins; anything else
/// is treated as an image reference whose derived (or `--name`d) VM is
/// reused if it already exists and created otherwise.
fn resolve_or_create_vm(request: &RunRequest) -> anyhow::Result<VmRecord> {
    let target = &request.target;
    if request.name.is_none()
        && validate_vm_name(target).is_ok()
        && vms_root().join(target).join("vm.json").is_file()
    {
        return load_vm(target);
    }

    let reference = Reference::parse(target).with_context(|| {
        format!("{target:?} is neither an existing VM name nor a valid image reference")
    })?;
    let name = match &request.name {
        Some(name) => name.clone(),
        None => derive_vm_name(&reference),
    };
    if vms_root().join(&name).join("vm.json").is_file() {
        tracing::debug!(name, "reusing existing pet VM");
        return load_vm(&name);
    }
    eprintln!("ocivmm: creating VM {name} from {reference}");
    create_vm(target, &name, request.pull, DEFAULT_DISK_MIB)
}

/// Default vCPU count: every host CPU (saturating into the `u8` the
/// VM config carries).
fn default_cpus() -> u8 {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(u8::MAX as usize) as u8
}

/// The kernel command line for distro-kernel boots: dracut mounts the
/// virtio-blk root device (`root=/dev/vda`) and switches into the
/// distro's own systemd (`/sbin/init`, the kernel default — no
/// `init=`). `console=ttyS0` (the VMM's only console — see
/// `oci-vmm`'s own docs) makes systemd's getty-generator spawn the
/// autologin console the provisioning step configured.
/// `rd.shell=0`/`rd.emergency=reboot`: a root-mount failure must end
/// the VM (which reports it), not park at an interactive dracut
/// prompt nothing is attached to — `reboot`, not `poweroff`: `oci-vmm`
/// has no ACPI at all, so the only way the guest can ever signal "end
/// the VM" is `reboot=k`'s i8042 keyboard-controller reset write,
/// which its own reset-only i8042 device is built to detect; a real
/// ACPI poweroff has nothing to negotiate with and the kernel just
/// halts forever instead (found via a full real CI boot: the guest
/// completed its actual workload and systemd shutdown target cleanly,
/// then printed `reboot: Power off not available: System halted
/// instead` and hung there until the job's own timeout killed it).
/// `selinux=0` because the container image ships no policy to load.
fn kernel_cmdline() -> String {
    // TEMPORARY diagnostic: ignore_loglevel, to rule out virtio_net's
    // own probe messages being filtered by a distro-specific default
    // console loglevel -- the ubuntu-26.04 cell's own guest kernel
    // shows no virtio_net driver messages at all (no probe success,
    // no failure, nothing), unlike centos-stream10's, which does.
    "reboot=k panic=-1 console=ttyS0 root=/dev/vda rw selinux=0 systemd.firstboot=off \
     rd.shell=0 rd.emergency=reboot ignore_loglevel"
        .to_string()
}

/// The guest environment for the generated oneshot unit: the image's
/// declared env (or the standard `PATH` fallback), `HOME`, the host's
/// `TERM`, then `-e` overrides.
fn unit_env(request: &RunRequest, record: &VmRecord) -> Vec<String> {
    let mut env = if record.env.is_empty() {
        vec![DEFAULT_ENV_WHEN_VM_DECLARES_NONE.to_string()]
    } else {
        record.env.clone()
    };
    if !env.iter().any(|e| e.starts_with("HOME=")) {
        env.push("HOME=/root".to_string());
    }
    if let Ok(term) = std::env::var("TERM") {
        env = merge_env(env, &[format!("TERM={term}")]);
    }
    merge_env(env, &request.env)
}

/// Map a unit's `$EXIT_STATUS` content to our own exit code.
fn parse_exit_status(raw: &str) -> i32 {
    raw.trim().parse().unwrap_or(1)
}

/// Render the per-run oneshot unit.
///
/// `SuccessAction`/`FailureAction` are `reboot`, not `poweroff`: see
/// `kernel_cmdline`'s own doc comment for why `oci-vmm` (no ACPI at
/// all) can only ever be told to end the VM via the i8042 reset path
/// that `reboot` triggers.
fn systemd_unit(command: &[String], env: &[String], workdir: &str) -> anyhow::Result<String> {
    anyhow::ensure!(!command.is_empty(), "empty command");
    let exec_start = command
        .iter()
        .map(|arg| unit_escape_word(arg))
        .collect::<Result<Vec<_>, _>>()?
        .join(" ");
    let mut unit = String::from(
        "# Generated by ocivmm for a single `ocivmm run` invocation; removed afterward.\n\
         [Unit]\n\
         Description=ocivmm one-shot command\n\
         Wants=network-online.target nss-lookup.target\n\
         After=network-online.target nss-lookup.target\n\
         SuccessAction=reboot\n\
         FailureAction=reboot\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         StandardOutput=tty\n\
         StandardError=tty\n\
         TTYPath=/dev/ttyS0\n",
    );
    unit.push_str(&format!("WorkingDirectory={workdir}\n"));
    for entry in env {
        anyhow::ensure!(
            !entry.contains('\n') && !entry.contains('"'),
            "environment entry {entry:?} contains characters a unit file cannot carry"
        );
        unit.push_str(&format!("Environment=\"{entry}\"\n"));
    }
    unit.push_str(&format!("ExecStart={exec_start}\n"));
    unit.push_str(&format!(
        "ExecStopPost=/bin/sh -c 'echo \"$EXIT_STATUS\" > /{EXIT_STATUS_FILE}'\n"
    ));
    Ok(unit)
}

/// Quote one `ExecStart` argument per systemd.service syntax.
fn unit_escape_word(arg: &str) -> anyhow::Result<String> {
    anyhow::ensure!(
        !arg.contains('\n'),
        "command argument {arg:?} contains a newline"
    );
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    for c in arg.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '%' => out.push_str("%%"),
            '$' => out.push_str("$$"),
            _ => out.push(c),
        }
    }
    out.push('"');
    Ok(out)
}

/// Install the per-run unit (mounted image root at `mountpoint`): the
/// unit file, a `multi-user.target.wants` symlink, and a mask for
/// `serial-getty@ttyS0` (the autologin console would fight the unit
/// for the same tty).
fn write_run_unit(mountpoint: &Path, unit: &str) -> anyhow::Result<()> {
    let system = mountpoint.join("etc/systemd/system");
    let wants = system.join("multi-user.target.wants");
    std::fs::create_dir_all(&wants).with_context(|| format!("creating {}", wants.display()))?;
    std::fs::write(system.join(RUN_UNIT), unit).with_context(|| format!("writing {RUN_UNIT}"))?;
    let link = wants.join(RUN_UNIT);
    let _ = std::fs::remove_file(&link);
    symlink(&format!("/etc/systemd/system/{RUN_UNIT}"), &link)?;
    let mask = system.join("serial-getty@ttyS0.service");
    let _ = std::fs::remove_file(&mask);
    symlink("/dev/null", &mask)?;
    Ok(())
}

/// Remove everything [`write_run_unit`] installed (idempotent).
fn remove_run_unit(mountpoint: &Path) -> anyhow::Result<()> {
    let system = mountpoint.join("etc/systemd/system");
    let _ = std::fs::remove_file(system.join("multi-user.target.wants").join(RUN_UNIT));
    let _ = std::fs::remove_file(system.join(RUN_UNIT));
    let _ = std::fs::remove_file(system.join("serial-getty@ttyS0.service"));
    Ok(())
}

fn symlink(target: &str, link: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link)
        .with_context(|| format!("creating symlink {}", link.display()))?;
    #[cfg(not(unix))]
    anyhow::bail!("symlinks unsupported on this platform");
    #[cfg(unix)]
    Ok(())
}

/// Start passt for one VM boot and return its unix-stream socket path
/// (the `__boot` child connects to it itself). passt runs
/// `--foreground` as our own child (self-daemonization has been known
/// to unlink the socket file on some passt builds) and the socket is
/// polled into existence before returning; `--one-off` ends passt once
/// the VMM's one connection closes. `--publish` mappings become
/// passt's own `-t host:guest` TCP forwards.
///
/// The socket itself lives directly under `/tmp` (sticky-bit 1777,
/// universally writable), not inside `vm_dir` (root-owned since the
/// whole harness runs under sudo): passt always creates its own,
/// unmapped user namespace (`unshare(CLONE_NEWUSER)`) before binding,
/// and — confirmed via strace — a process inside an unmapped user
/// namespace has its filesystem permission checks against anything
/// outside that namespace (i.e. every pre-existing file, from any
/// original UID) degrade to the overflow UID, regardless of AppArmor
/// settings or `--runas`. `/tmp`'s own permissions already handle
/// this for every UID, sidestepping the whole question. `vm_dir`
/// itself is used only to name the temp socket uniquely and to
/// remove any stale one from a previous boot.
fn spawn_passt(vm_dir: &Path, ports: &[String]) -> anyhow::Result<PathBuf> {
    let vm_name = vm_dir
        .file_name()
        .expect("vm_dir always has a final component")
        .to_string_lossy();
    let socket = std::env::temp_dir().join(format!("ocivmm-{vm_name}-passt.sock"));
    let _ = std::fs::remove_file(&socket);
    let passt = std::env::var("OCIVMM_PASST").unwrap_or_else(|_| "passt".to_string());
    let mut command = std::process::Command::new(&passt);
    command
        .arg("--foreground")
        .arg("--one-off")
        // NOT --ipv4-only: tried it (every guest packet is NAT'd
        // through passt anyway, so IPv6 seemed to buy nothing here),
        // but CI showed the guest's DHCPv4 client then never
        // completing at all (only ever gaining its own IPv6
        // link-local address) -- reverted rather than dig into why a
        // flag that per passt's own docs should only affect IPv6
        // somehow broke IPv4 DHCP too.
        //
        // --mtu 1500: passt defaults to assigning an MTU of 65520 to
        // the guest via DHCP/NDP. Our virtio-net device doesn't
        // negotiate VIRTIO_NET_F_MTU or any GSO/TSO offload feature,
        // and its RX buffers are sized for a fixed, standard 1500-byte
        // Ethernet MTU (see MAX_FRAME_LEN/VNET_HDR_LEN in
        // crates/oci-vmm/src/virtio/net.rs). Left at its default,
        // passt's oversized MTU announcement led it to forward frames
        // far larger than our device can receive, which our own
        // bounds checks then had to drop -- silently stalling
        // reassembly-dependent traffic (e.g. package-manager
        // downloads) instead of ever failing loudly. Pin it to match.
        .arg("--mtu")
        .arg("1500")
        .arg("--socket")
        .arg(&socket);
    for port in ports {
        command.arg("-t").arg(port);
    }
    let mut child = command.spawn().with_context(|| {
        format!("running {passt:?} (passt provides guest networking; install the passt package)")
    })?;

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !socket.exists() {
        if let Some(status) = child.try_wait().context("checking on passt")? {
            anyhow::bail!("passt exited during startup ({status})");
        }
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "passt did not create {} within 10s",
            socket.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    Ok(socket)
}

/// Validate every `--publish HOST:GUEST` port mapping; passt's `-t`
/// forwards take the same `"host:guest"` form.
fn parse_ports(publish: &[String]) -> anyhow::Result<Vec<String>> {
    publish
        .iter()
        .map(|mapping| {
            let (host, guest) = mapping
                .split_once(':')
                .with_context(|| format!("--publish {mapping}: expected HOST_PORT:GUEST_PORT"))?;
            host.parse::<u16>()
                .ok()
                .with_context(|| format!("--publish {mapping}: invalid host port {host:?}"))?;
            guest
                .parse::<u16>()
                .ok()
                .with_context(|| format!("--publish {mapping}: invalid guest port {guest:?}"))?;
            Ok(mapping.clone())
        })
        .collect()
}

/// Merge `extra` `NAME=value` entries into `base`, replacing any entry
/// with the same `NAME` in place and appending the rest.
fn merge_env(mut base: Vec<String>, extra: &[String]) -> Vec<String> {
    for entry in extra {
        let key = entry.split('=').next().unwrap_or(entry);
        match base
            .iter_mut()
            .find(|existing| existing.split('=').next() == Some(key))
        {
            Some(existing) => *existing = entry.clone(),
            None => base.push(entry.clone()),
        }
    }
    base
}

/// Make the guest's `/etc/resolv.conf`, `/etc/hosts`, and
/// `/etc/hostname` usable, applied to the scratch directory before
/// imaging. OCI base images ship these absent, empty, or as dangling
/// symlinks.
fn ensure_guest_files(rootfs: &Path, name: &str) -> anyhow::Result<()> {
    let etc = rootfs.join("etc");
    std::fs::create_dir_all(&etc).with_context(|| format!("creating {}", etc.display()))?;

    let hosts = etc.join("hosts");
    if !hosts.exists() {
        std::fs::write(
            &hosts,
            format!("127.0.0.1 localhost\n::1 localhost\n127.0.1.1 {name}\n"),
        )
        .with_context(|| format!("writing {}", hosts.display()))?;
    }

    let hostname = etc.join("hostname");
    if !hostname.exists() {
        std::fs::write(&hostname, format!("{name}\n"))
            .with_context(|| format!("writing {}", hostname.display()))?;
    }
    Ok(())
}

/// Unconditionally point the scratch rootfs's resolv.conf at the
/// host's (the provisioning container needs it; provisioning later
/// hands DNS over to systemd-resolved for the booted VM itself). Base
/// images routinely leak a meaningless resolv.conf from their own
/// build environment.
fn reset_resolv_conf(rootfs: &Path) -> anyhow::Result<()> {
    let resolv = rootfs.join("etc/resolv.conf");
    let _ = std::fs::remove_file(&resolv);
    std::fs::write(&resolv, host_resolv_conf())
        .with_context(|| format!("writing {}", resolv.display()))
}

/// The host's own resolv.conf, verbatim, with a public-resolver
/// fallback only when the host has nothing usable to offer at all.
fn host_resolv_conf() -> String {
    match std::fs::read_to_string("/etc/resolv.conf") {
        Ok(content) if !nameservers_from(&content).is_empty() => content,
        _ => "nameserver 1.1.1.1\nnameserver 8.8.8.8\n".to_string(),
    }
}

/// Parse `nameserver` addresses out of resolv.conf content.
fn nameservers_from(content: &str) -> Vec<String> {
    content
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            (fields.next() == Some("nameserver"))
                .then(|| fields.next())
                .flatten()
        })
        .map(ToString::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_vm_name_matches_ocibox_convention() {
        assert!(validate_vm_name("ubuntu-26.04").is_ok());
        assert!(validate_vm_name("centos-stream10").is_ok());
        assert!(validate_vm_name("-leading").is_err());
        assert!(validate_vm_name("has space").is_err());
        assert!(validate_vm_name("").is_err());
    }

    #[test]
    fn derive_vm_name_uses_repo_basename_and_tag() {
        let reference = Reference::parse("ubuntu:26.04").unwrap();
        assert_eq!(derive_vm_name(&reference), "ubuntu-26.04");
        let reference = Reference::parse("quay.io/centos/centos:stream10").unwrap();
        assert_eq!(derive_vm_name(&reference), "centos-stream10");
    }

    #[test]
    fn derive_vm_name_drops_latest() {
        let reference = Reference::parse("debian").unwrap();
        assert_eq!(derive_vm_name(&reference), "debian");
    }

    #[test]
    fn parse_ports_validates_both_sides() {
        assert!(parse_ports(&["8080:80".into()]).is_ok());
        assert!(parse_ports(&["notaport:80".into()]).is_err());
        assert!(parse_ports(&["8080:99999".into()]).is_err());
        assert!(parse_ports(&["8080".into()]).is_err());
    }

    #[test]
    fn merge_env_overrides_by_key() {
        let merged = merge_env(
            vec!["PATH=/usr/bin".into(), "LANG=C".into()],
            &["PATH=/opt/bin".into(), "TERM=xterm".into()],
        );
        assert_eq!(
            merged,
            vec![
                "PATH=/opt/bin".to_string(),
                "LANG=C".to_string(),
                "TERM=xterm".to_string()
            ]
        );
    }

    #[test]
    fn nameservers_parse_including_loopback() {
        let content = "nameserver 127.0.0.53\nnameserver 10.0.0.2\noptions edns0\n";
        assert_eq!(
            nameservers_from(content),
            vec!["127.0.0.53".to_string(), "10.0.0.2".to_string()]
        );
    }

    #[test]
    fn parse_exit_status_maps_signals_to_failure() {
        assert_eq!(parse_exit_status("0\n"), 0);
        assert_eq!(parse_exit_status("42"), 42);
        assert_eq!(parse_exit_status("KILL"), 1);
        assert_eq!(parse_exit_status(""), 1);
    }

    #[test]
    fn systemd_unit_escapes_and_reboots_to_end_the_vm() {
        let unit = systemd_unit(
            &["bash".into(), "/src/ci/vm-ci.sh".into(), "100%".into()],
            &["PATH=/usr/bin".into()],
            "/root",
        )
        .unwrap();
        assert!(unit.contains("ExecStart=\"bash\" \"/src/ci/vm-ci.sh\" \"100%%\"\n"));
        assert!(unit.contains("SuccessAction=reboot"));
        assert!(unit.contains("FailureAction=reboot"));
        assert!(unit.contains("Environment=\"PATH=/usr/bin\"\n"));
        assert!(unit.contains("$EXIT_STATUS"));
        assert!(systemd_unit(&[], &[], "/").is_err());
    }

    #[test]
    fn unit_escape_word_handles_specifiers_and_expansion() {
        assert_eq!(unit_escape_word("a b").unwrap(), "\"a b\"");
        assert_eq!(unit_escape_word("50%").unwrap(), "\"50%%\"");
        assert_eq!(unit_escape_word("$HOME").unwrap(), "\"$$HOME\"");
        assert_eq!(unit_escape_word("q\"q").unwrap(), "\"q\\\"q\"");
        assert!(unit_escape_word("a\nb").is_err());
    }
}
