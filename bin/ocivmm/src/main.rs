//! `ocivmm` — pet microVMs from OCI images (krunvm equivalent).
//!
//! Creates long-lived pet *virtual machines* the same way `ocibox`
//! creates pet containers: `ocivmm run ubuntu:26.04` resolves/pulls
//! the image, extracts a real, dedicated, writable rootfs for a named
//! VM (derived from the image name, `ubuntu-26.04`), and boots it as a
//! libkrun microVM — the rootfs lives on as a plain host directory, so
//! everything installed or written inside the guest persists across
//! runs, exactly the "pet" model. Studied directly from
//! `~/git/libkrun` (whose own `krunvm` companion pioneered this
//! OCI-image-as-VM-rootfs design; `ocivmm` replaces its buildah layer
//! with this project's own `oci-registry`/`oci-store`/`oci-layer`
//! pull-and-extract stack, the exact same one `ocibox create` uses).
//!
//! Unlike krunvm, nothing here is dynamically loaded and nothing in
//! the guest belongs to libkrun: the VMM is libkrun's own Rust crates
//! **statically linked** into this binary (see `microvm.rs` — no
//! `libkrun.so`, no libkrunfw, no dlopen), and the pet VM runs the
//! **distro's own kernel and systemd**. `create` provisions the fresh
//! rootfs by running *the distro's own package manager in it as a
//! container* (this project's own `oci_runtime_core::launch`, the
//! same machinery `ocibox enter` uses): `dnf install kernel systemd
//! ...` inside centos:stream10, `apt-get install linux-image-virtual
//! systemd ...` inside ubuntu:26.04 — plus a dracut initramfs able to
//! mount the virtiofs root (`root=virtiofs:/dev/root`) and a
//! systemd-networkd DHCP config. Every `run` then loads the guest's
//! own `/boot/vmlinuz-*` (the external-kernel loader unwraps a distro
//! bzImage by scanning for its compression magic) and boots straight
//! into the distro's systemd as PID 1: `ocivmm run centos-stream10`
//! with no command lands on a real autologin root console
//! (serial-getty on hvc0), and with a command runs it as a generated
//! oneshot unit that powers the VM off and hands its exit status back
//! to the host through a file on the shared rootfs.
//!
//! Networking is a passt-backed virtio-net device; systemd-networkd
//! does DHCP against passt, and `--publish` maps host ports via
//! passt's own `-t` forwarding.
//!
//! Host requirements (run time only — building `ocivmm` needs
//! nothing): `/dev/kvm` and the `passt` package. Guest uid/gid
//! fidelity (package managers chown what they install, both in the
//! provisioning container and over virtiofs) wants real root — run
//! `ocivmm` as root for full pet-distro behavior, the way CI does.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clap::Parser;
use oci_spec_types::Reference;
use oci_store::Store;
use serde::{Deserialize, Serialize};

mod microvm;

/// Command-line interface.
#[derive(Debug, Parser)]
#[command(
    name = "ocivmm",
    about = "Pet microVMs from OCI images (libkrun-based)",
    version = oci_cli_common::version::long(env!("CARGO_PKG_VERSION")),
)]
struct Cli {
    #[command(flatten)]
    global: oci_cli_common::GlobalArgs,

    #[command(subcommand)]
    command: Option<Command>,
}

/// Default guest RAM in MiB when `--mem` is not given. libkrun
/// allocates guest memory lazily, so an unused allowance costs nothing.
const DEFAULT_MEM_MIB: u32 = 4096;

/// Where the guest command's exit status lands, relative to the rootfs
/// (written by the generated oneshot unit's `ExecStopPost`, read back
/// by the host through the shared directory once the VMM exits).
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
    /// persistent rootfs on every use after that. With no `COMMAND`,
    /// boots to a root login on the console; with one, runs it as a
    /// oneshot systemd unit, powers off, and exits with its status.
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
        /// Extra host directory to share with the guest
        /// (`HOST_DIR:GUEST_DIR`, repeatable), mounted via virtiofs.
        #[arg(long = "volume", short = 'v', value_name = "HOST:GUEST")]
        volumes: Vec<String>,
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
        /// VM's rootfs is never silently replaced).
        #[arg(long)]
        pull: bool,
    },
    /// Create (and provision) a pet VM without leaving it running:
    /// resolves `--image`, extracts a dedicated writable rootfs, then
    /// installs the distro's own kernel, initramfs, and systemd into
    /// it using its own package manager — the `ocibox create` model
    /// plus the one bootstrap boot a VM needs on top.
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
    },
    /// List created VMs (name, image, creation time), sorted by name —
    /// the same shape (and the same tolerance for one unreadable
    /// record not hiding every other one) as `ocibox list`.
    #[command(alias = "ls")]
    List,
    /// Remove a VM entirely (its rootfs and persisted record).
    Rm {
        /// The VM's name, exactly as shown by `ocivmm list`. Required
        /// unless `--all` is given instead.
        name: Option<String>,
        /// Remove every existing VM. Mutually exclusive with a
        /// positional `name`.
        #[arg(long, short = 'a')]
        all: bool,
    },
    /// Hidden: become the VMM for a spec prepared by the parent
    /// `ocivmm` process. [`microvm::boot`] turns its caller into the
    /// VMM (it never returns; the process `_exit`s when the guest
    /// powers off), so `run` — which must keep running to read the
    /// exit-status file back and clean up per-run guest files — boots
    /// through this re-exec'd child instead. The same self-re-exec
    /// technique `ocicri`'s own `__launch` uses.
    #[command(name = "__boot", hide = true)]
    Boot {
        /// Path of the serialized [`microvm::VmSpec`] JSON.
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
                volumes,
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
                volumes,
                publish,
                env,
                workdir,
                pull,
            }),
            Some(Command::Create { image, name, pull }) => {
                cmd_create(&image, name.as_deref(), pull)
            }
            Some(Command::List) => cmd_list(cli.global.json),
            Some(Command::Rm { name, all }) => cmd_rm(name.as_deref(), all),
            Some(Command::Boot { spec }) => cmd_boot(&spec),
            None => anyhow::bail!("no subcommand given (try `ocivmm run ubuntu:26.04`)"),
        }
    })
}

/// Where every VM's own on-disk state lives — a sibling of `oci_store`'s
/// own `blobs`/`images` directories, this project's established
/// convention for per-capability state under the one shared storage
/// root (`containers/` for `ociman`, `boxes/` for `ocibox`, `vms/`
/// here).
fn vms_root() -> PathBuf {
    oci_cli_common::storage::default_root().join("vms")
}

/// A conservative charset check matching real `docker`/`podman`'s own
/// `--name` convention — the same small, deliberate duplicate `ocibox`
/// keeps (see `validate_box_name` there for the cross-binary-dependency
/// reasoning); also the path-traversal guard before joining onto
/// [`vms_root`].
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

/// Derive a friendly default VM name from an image reference: the last
/// path component of the repository plus the tag — `ubuntu:26.04` ->
/// `ubuntu-26.04`, `quay.io/centos/centos:stream10` ->
/// `centos-stream10`, tagless/`latest` references just the repository
/// basename. Any character outside the VM-name charset becomes `-`.
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

/// A VM's own persisted metadata (`<vms_root>/<name>/vm.json`) —
/// deliberately minimal, the same shape (and the same captured-once-at-
/// create-time reasoning) as `ocibox`'s `BoxRecord`. The kernel and
/// initramfs are *not* recorded here: they belong to the guest (its own
/// package manager installs and upgrades them), so every boot
/// re-detects the newest ones from the rootfs instead of trusting a
/// stale record ([`find_guest_kernel`]).
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
/// `env` at all — matching real `podman`'s identical fallback, the
/// same small duplicate `ocibox`/`ociman` each keep.
const DEFAULT_ENV_WHEN_VM_DECLARES_NONE: &str =
    "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// The real create logic `cmd_create` and `cmd_run`'s create-on-first-
/// use path share: resolve/pull the image, extract a dedicated
/// writable rootfs (mirroring `ocibox`'s `create_box`, including the
/// deliberate choice *not* to share `oci_store`'s read-only rootfs
/// cache), persist the record, then provision the distro's own kernel
/// and systemd into it ([`provision_vm`]). Any failure removes the
/// half-created VM directory.
fn create_vm(image: &str, name: &str, pull: bool) -> anyhow::Result<VmRecord> {
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
    let record = oci_registry::resolve_or_pull(&store, &reference, pull_policy, true, || {
        oci_registry::pull_unconditionally(&store, &reference, true)
    })
    .with_context(|| format!("resolving {reference}"))?;

    let manifest = store
        .image_manifest(&record)
        .with_context(|| format!("reading manifest for {reference}"))?;
    let config = store
        .image_config(&record)
        .with_context(|| format!("reading config for {reference}"))?;
    let container_config = config.config.unwrap_or_default();

    let rootfs = vm_dir.join("rootfs");
    std::fs::create_dir_all(&rootfs).with_context(|| format!("creating {}", rootfs.display()))?;
    let vm_record = VmRecord {
        name: name.to_string(),
        image: reference.to_string(),
        manifest_digest: record.manifest_digest.to_string(),
        created: oci_spec_types::time::format_rfc3339_utc(std::time::SystemTime::now()),
        env: container_config.env,
    };
    let result = extract_rootfs(&store, &manifest, &rootfs)
        .and_then(|()| ensure_guest_files(&rootfs, name))
        // A freshly extracted image has no pet customizations to
        // preserve, and base images routinely leak a meaningless
        // resolv.conf from their own build environment (checked:
        // centos:stream10 ships a NetworkManager-generated file
        // pointing at a libvirt bridge) -- the provisioning container
        // needs the host's, unconditionally.
        .and_then(|()| reset_resolv_conf(&rootfs))
        .and_then(|()| {
            let vm_json_path = vm_dir.join("vm.json");
            std::fs::write(
                &vm_json_path,
                serde_json::to_vec_pretty(&vm_record).context("serializing VM record")?,
            )
            .with_context(|| format!("writing {}", vm_json_path.display()))
        })
        .and_then(|()| provision_vm(&vm_dir, &rootfs, name));
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
/// `rootfs` — identical to `ocibox`'s extraction for identical
/// reasons (see [`create_vm`]).
fn extract_rootfs(
    store: &Store,
    manifest: &oci_spec_types::image::ImageManifest,
    rootfs: &Path,
) -> anyhow::Result<()> {
    for layer in &manifest.layers {
        let compression = oci_layer::compression_for_media_type(&layer.media_type)
            .with_context(|| format!("unsupported layer media type {:?}", layer.media_type))?;
        let blob = store
            .open_blob(&layer.digest)
            .with_context(|| format!("opening layer blob {}", layer.digest))?;
        oci_layer::apply(blob, compression, rootfs)
            .with_context(|| format!("extracting layer {}", layer.digest))?;
    }
    Ok(())
}

/// The distro-specific half of the provisioning script: install the
/// distro's own kernel, dracut, kmod, and systemd with the distro's
/// own package manager. Distro differences are data, not logic — the
/// `ci/vm-prepare.sh` convention. On the apt side dracut is installed
/// *before* the kernel so it both satisfies `linux-image-*`'s
/// `initramfs-tools | linux-initramfs-tool` alternative and owns the
/// kernel's initramfs hooks.
#[cfg(target_os = "linux")]
const PROVISION_PACKAGES: &str = r#"
if command -v dnf >/dev/null 2>&1; then
    dnf -y --setopt=install_weak_deps=False install \
        kernel dracut kmod systemd systemd-resolved dbus-broker util-linux
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

/// The distro-independent half: a dracut initramfs that can mount the
/// virtiofs root (`root=virtiofs:/dev/root`, dracut's own documented
/// syntax — checked in dracut's `modules.d/*virtiofs/parse-virtiofs.sh`),
/// systemd-networkd DHCP for the passt-backed virtio-net device, and a
/// root autologin on the hvc0 console systemd's getty-generator spawns
/// for `console=hvc0`.
#[cfg(target_os = "linux")]
const PROVISION_CONFIGURE: &str = r#"
kver=$(ls /lib/modules | sort -V | tail -n 1)
[ -n "$kver" ] || { echo 'ocivmm provision: no kernel modules installed' >&2; exit 1; }
# --add-drivers virtio_mmio: the VMM attaches every device over
# virtio-MMIO (no PCI), and dracut's default driver policy does not
# pull that transport in even with --no-hostonly -- without it the
# root filesystem's virtiofs device never appears in the initramfs
# (found the hard way: "dracut: FATAL: virtiofs: failed to mount
# root fs").
dracut --force --no-hostonly --add virtiofs --add-drivers 'virtio_mmio virtiofs' \
    "/boot/ocivmm-initrd-$kver.img" "$kver"

mkdir -p /etc/systemd/network \
    /etc/systemd/system/multi-user.target.wants \
    /etc/systemd/system/network-online.target.wants \
    '/etc/systemd/system/serial-getty@hvc0.service.d'

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
ln -sf /usr/lib/systemd/system/systemd-resolved.service \
    /etc/systemd/system/multi-user.target.wants/systemd-resolved.service

# From here on DNS belongs to systemd-resolved, fed by whatever DNS
# the DHCP lease carries (passt advertises itself and forwards to the
# host's resolvers, looping in the host's own loopback stub if that's
# what the host uses) -- the host-written static resolv.conf above was
# only ever for this provisioning container itself.
ln -sfn ../run/systemd/resolve/resolv.conf /etc/resolv.conf

cat > '/etc/systemd/system/serial-getty@hvc0.service.d/autologin.conf' <<'EOF'
[Service]
ExecStart=
ExecStart=-/sbin/agetty --autologin root --keep-baud 115200,57600,38400,9600 %I $TERM
EOF

echo 'ocivmm provision: done'
"#;

/// Provision a freshly extracted rootfs with the distro's own kernel
/// and systemd by running the provisioning script *as a container on
/// the rootfs* — the same shared `oci_runtime_core::launch`/`Bundle`/
/// `validate` lifecycle `ocibox enter` uses (host network kept: the
/// package manager needs the registry mirrors), no VM involved: a
/// fresh OCI rootfs has no kernel to boot yet, and a container needs
/// none. Images with no `dnf`/`apt-get` (alpine, distroless) are a
/// clear, upfront error: without a distro able to install its own
/// kernel and systemd there is nothing `ocivmm` could boot.
#[cfg(target_os = "linux")]
fn provision_vm(vm_dir: &Path, rootfs: &Path, name: &str) -> anyhow::Result<()> {
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
    // filter: this is our own trusted provisioning script (found the
    // hard way: the default profile ENOSYS-blocked `socket(2)` here,
    // taking DNS down with it), and the same invocation is about to
    // boot a whole VM anyway — there is no privilege boundary a
    // filter would enforce.
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

    let config_path = vm_dir.join(oci_runtime_core::bundle::CONFIG_FILENAME);
    std::fs::write(&config_path, serde_json::to_vec_pretty(&spec)?)
        .with_context(|| format!("writing {}", config_path.display()))?;
    let result = (|| -> anyhow::Result<()> {
        let bundle = oci_runtime_core::Bundle::load(vm_dir)
            .with_context(|| format!("loading bundle from {}", vm_dir.display()))?;
        let validated_rootfs = oci_runtime_core::validate::validate(&bundle)
            .context("provisioning config.json failed validation")?;
        // SAFETY: `ocivmm create` has not spawned any additional
        // threads by this point (pulling and extracting the image
        // don't), matching `ocibox enter`'s identical safety note for
        // this same entry point. Stdin is closed (a package install is
        // never interactive); output passes through for progress.
        #[allow(unsafe_code)]
        let exit_code = unsafe {
            oci_runtime_core::launch::run(
                &format!("ocivmm-provision-{name}"),
                &bundle,
                &validated_rootfs,
                true,
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
fn provision_vm(_vm_dir: &Path, _rootfs: &Path, _name: &str) -> anyhow::Result<()> {
    anyhow::bail!("ocivmm can only provision and run VMs on Linux (KVM + containers)");
}

/// Boot `spec` in a re-exec'd `ocivmm __boot` child (stdio inherited)
/// and wait for it — see [`Command::Boot`] for why a child.
fn boot_in_child(
    vm_dir: &Path,
    spec: &microvm::VmSpec,
) -> anyhow::Result<std::process::ExitStatus> {
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
fn cmd_boot(spec_path: &Path) -> anyhow::Result<()> {
    let bytes = std::fs::read(spec_path)
        .with_context(|| format!("reading boot spec {}", spec_path.display()))?;
    let spec: microvm::VmSpec = serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing boot spec {}", spec_path.display()))?;
    raise_nofile_limit();
    microvm::boot(&spec).map(|never| match never {})
}

/// Read one VM's persisted [`VmRecord`] back from `vm.json`.
fn load_vm(name: &str) -> anyhow::Result<VmRecord> {
    let vm_json_path = vms_root().join(name).join("vm.json");
    let bytes = std::fs::read(&vm_json_path)
        .with_context(|| format!("reading {}", vm_json_path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", vm_json_path.display()))
}

fn cmd_create(image: &str, name: Option<&str>, pull: bool) -> anyhow::Result<()> {
    let reference =
        Reference::parse(image).with_context(|| format!("parsing image reference {image:?}"))?;
    let name = match name {
        Some(name) => name.to_string(),
        None => derive_vm_name(&reference),
    };
    create_vm(image, &name, pull)?;
    println!("{name}");
    Ok(())
}

/// Every VM's persisted record, sorted by name — same enumeration
/// shape (and unreadable-entry tolerance) as `ocibox list`.
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

/// Remove exactly one VM's directory and print its name — validated
/// first for the same path-traversal reason `ocibox`'s equivalent is.
fn remove_one_vm(name: &str) -> anyhow::Result<()> {
    validate_vm_name(name)?;
    let vm_dir = vms_root().join(name);
    anyhow::ensure!(vm_dir.is_dir(), "{name}: no such VM");
    std::fs::remove_dir_all(&vm_dir).with_context(|| format!("removing {}", vm_dir.display()))?;
    println!("{name}");
    Ok(())
}

/// `ocivmm rm <NAME>` / `ocivmm rm --all` — one or the other, not
/// both; `--all` keeps going past a per-VM failure and reports the
/// first error at the end, matching `ocibox rm` exactly.
fn cmd_rm(name: Option<&str>, all: bool) -> anyhow::Result<()> {
    match (name, all) {
        (Some(_), true) => anyhow::bail!("cannot give both a VM name and --all"),
        (None, false) => anyhow::bail!("no VM name given (try `ocivmm rm <NAME>` or `--all`)"),
        (Some(name), false) => remove_one_vm(name),
        (None, true) => {
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

/// Everything `ocivmm run` was asked for, bundled so the handler
/// signature stays readable.
struct RunRequest {
    target: String,
    command: Vec<String>,
    name: Option<String>,
    cpus: Option<u8>,
    mem: Option<u32>,
    volumes: Vec<String>,
    publish: Vec<String>,
    env: Vec<String>,
    workdir: Option<String>,
    pull: bool,
}

/// `ocivmm run`: resolve `TARGET` to a (possibly freshly created and
/// provisioned) pet VM, then boot it with the guest's own kernel and
/// systemd ([`run_systemd_vm`]) — exiting the process with the guest
/// command's own exit status.
fn cmd_run(request: &RunRequest) -> anyhow::Result<()> {
    let record = resolve_or_create_vm(request)?;
    let vm_dir = vms_root().join(&record.name);
    let rootfs = vm_dir.join("rootfs");
    anyhow::ensure!(
        rootfs.is_dir(),
        "{}: VM record exists but its rootfs is missing (remove it with `ocivmm rm`)",
        record.name
    );

    ensure_guest_files(&rootfs, &record.name)
        .with_context(|| format!("preparing guest files for {}", record.name))?;

    match find_guest_kernel(&vm_dir, &rootfs)? {
        Some(kernel) if has_systemd(&rootfs) => {
            run_systemd_vm(request, &record, &vm_dir, &rootfs, kernel)
        }
        _ => anyhow::bail!(
            "{}: no bootable distro kernel + systemd in this VM's rootfs (created by an \
             older ocivmm?); `ocivmm rm {}` and recreate it",
            record.name,
            record.name
        ),
    }
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
    create_vm(target, &name, request.pull)
}

/// Default vCPU count: every host CPU (saturating into the `u8` the
/// VM config carries).
fn default_cpus() -> u8 {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(u8::MAX as usize) as u8
}

fn path_string(path: &Path) -> anyhow::Result<String> {
    Ok(path
        .to_str()
        .with_context(|| format!("{} is not valid UTF-8", path.display()))?
        .to_string())
}

// ---------------------------------------------------------------------------
// Distro-kernel + systemd boots
// ---------------------------------------------------------------------------

/// A guest kernel found in the rootfs, ready for `krun_set_kernel`.
struct GuestKernel {
    vmlinuz: PathBuf,
    initramfs: Option<PathBuf>,
    format: u32,
}

/// The kernel command line for distro-kernel boots: dracut mounts the
/// virtiofs root (`root=virtiofs:/dev/root`) and switches into the
/// distro's own systemd (`/sbin/init`, the kernel default — no
/// `init=`). Two consoles: `ttyS0` (built into distro kernels, so
/// early boot/dracut/panic output is never lost) then `hvc0` (virtio,
/// the primary — last `console=` wins `/dev/console`); systemd's
/// getty-generator spawns the autologin console the provisioning step
/// configured on both. No `quiet`: boot noise is cheap, and an
/// unbootable guest with an invisible panic is exactly how the old
/// qemu harness's serial console log earned its keep. `selinux=0`
/// because the container image ships no policy to load.
fn kernel_cmdline() -> String {
    // rd.shell=0 + rd.emergency=poweroff: a root-mount failure must
    // end the VM (which reports it), not park it at an interactive
    // dracut emergency prompt nothing is attached to — found the hard
    // way as a silent 90-minute CI hang.
    format!(
        "reboot=k panic=-1 console=ttyS0 console=hvc0 root=virtiofs:{} rw selinux=0 \
         systemd.firstboot=off rd.shell=0 rd.emergency=poweroff",
        microvm::ROOT_TAG
    )
}

/// Does the rootfs have a real systemd to boot as PID 1?
fn has_systemd(rootfs: &Path) -> bool {
    ["usr/lib/systemd/systemd", "lib/systemd/systemd"]
        .iter()
        .any(|p| rootfs.join(p).exists())
}

/// Find the newest kernel the guest's own package manager installed:
/// the highest-versioned `/lib/modules/<kver>` with a matching
/// `/boot/vmlinuz-<kver>`, plus its initramfs (preferring the
/// virtiofs-capable one the provisioning step generated). Re-detected
/// on every boot so a `dnf upgrade` inside the pet VM takes effect —
/// nothing is cached in `vm.json`.
///
/// On x86_64 the compressed distro bzImage is unwrapped to its inner
/// ELF vmlinux (cached in the VM directory): the ELF loader is the
/// path that honors the kernel's PVH entry point, and PVH is the boot
/// protocol whose initrd pointer is a full 64 bits — the legacy
/// 64-bit boot-params path truncates `ramdisk_image` to u32 (checked
/// in the pinned krun-arch's `configure_64bit_boot`), which with the
/// initrd placed at top-of-RAM breaks any guest bigger than 4 GiB,
/// found the hard way as a "VFS: Unable to mount root fs" panic on
/// the 8 GiB CI guests. Both target distros build with CONFIG_PVH=y
/// (checked in their real x86_64 kernel packages).
fn find_guest_kernel(vm_dir: &Path, rootfs: &Path) -> anyhow::Result<Option<GuestKernel>> {
    let modules_dir = rootfs.join("lib/modules");
    let entries = match std::fs::read_dir(&modules_dir) {
        Ok(entries) => entries,
        Err(_) => return Ok(None),
    };
    let mut kvers: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    kvers.sort_by(|a, b| compare_versions(a, b));
    while let Some(kver) = kvers.pop() {
        // Debian/Ubuntu install the image at /boot/vmlinuz-<kver>;
        // RHEL-family kernels own it at /lib/modules/<kver>/vmlinuz
        // (kernel-install only copies it to /boot on real systems,
        // which a container-provisioned rootfs is not — checked
        // against a real centos:stream10 provisioning run).
        let Some(vmlinuz) = [
            format!("boot/vmlinuz-{kver}"),
            format!("lib/modules/{kver}/vmlinuz"),
        ]
        .iter()
        .map(|p| rootfs.join(p))
        .find(|p| p.is_file()) else {
            continue;
        };
        let bytes =
            std::fs::read(&vmlinuz).with_context(|| format!("reading {}", vmlinuz.display()))?;
        let Some(mut format) = kernel_format(&bytes, std::env::consts::ARCH) else {
            tracing::debug!(kernel = %vmlinuz.display(), "unrecognized kernel image format");
            continue;
        };
        let mut vmlinuz = vmlinuz;
        if matches!(format, 4 /* IMAGE_GZ */ | 5 /* IMAGE_ZSTD */) {
            let elf = extract_vmlinux(&bytes)
                .with_context(|| format!("extracting vmlinux from {}", vmlinuz.display()))?;
            let cache = vm_dir.join("vmlinux");
            std::fs::write(&cache, elf).with_context(|| format!("writing {}", cache.display()))?;
            vmlinuz = cache;
            format = 1; // KRUN_KERNEL_FORMAT_ELF -> the PVH-capable loader
        }
        let initramfs = [
            format!("boot/ocivmm-initrd-{kver}.img"),
            format!("boot/initramfs-{kver}.img"),
            format!("boot/initrd.img-{kver}"),
        ]
        .iter()
        .map(|p| rootfs.join(p))
        .find(|p| p.is_file());
        return Ok(Some(GuestKernel {
            vmlinuz,
            initramfs,
            format,
        }));
    }
    Ok(None)
}

/// Unwrap a bzImage's inner ELF vmlinux: decompress from the earliest
/// gzip/zstd magic (both single-stream; trailing bzImage bytes after
/// the stream are ignored by construction) — the same technique the
/// kernel's own `extract-vmlinux` script and libkrun's Image* loaders
/// use, done host-side so the PVH-capable ELF loader can be used
/// instead (see [`find_guest_kernel`]).
fn extract_vmlinux(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    use std::io::Read as _;
    let find = |magic: &[u8]| bytes.windows(magic.len()).position(|w| w == magic);
    let gz = find(&[0x1f, 0x8b, 0x08]);
    let zst = find(&[0x28, 0xb5, 0x2f, 0xfd]);
    let mut elf = Vec::new();
    match (gz, zst) {
        (Some(g), z) if z.is_none_or(|z| g < z) => {
            flate2::read::GzDecoder::new(&bytes[g..])
                .read_to_end(&mut elf)
                .context("decompressing gzip vmlinux")?;
        }
        (_, Some(z)) => {
            ruzstd::decoding::StreamingDecoder::new(&bytes[z..])
                .context("initializing zstd decoder")?
                .read_to_end(&mut elf)
                .context("decompressing zstd vmlinux")?;
        }
        _ => anyhow::bail!("no gzip/zstd stream found in the kernel image"),
    }
    anyhow::ensure!(
        elf.starts_with(&[0x7f, b'E', b'L', b'F']),
        "decompressed kernel payload is not an ELF vmlinux"
    );
    Ok(elf)
}

/// Order two kernel-version strings by their numeric segments
/// (`6.12.10-300` > `6.12.9-400`), falling back to lexicographic for
/// equal numeric prefixes — a tiny `sort -V` equivalent.
fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let segments = |s: &str| -> Vec<u64> {
        s.split(|c: char| !c.is_ascii_digit())
            .filter(|seg| !seg.is_empty())
            .filter_map(|seg| seg.parse().ok())
            .collect()
    };
    segments(a).cmp(&segments(b)).then_with(|| a.cmp(b))
}

/// Sniff a kernel image's `KRUN_KERNEL_FORMAT_*` constant from its own
/// bytes, mirroring exactly what libkrun's `load_external_kernel` does
/// with each format (checked in `~/git/libkrun`'s
/// `src/vmm/src/builder.rs`): on x86_64 a distro `vmlinuz` is a
/// bzImage whose embedded vmlinux libkrun finds by scanning for the
/// compression magic — gzip (`IMAGE_GZ`), zstd (`IMAGE_ZSTD`), or
/// bzip2 (`IMAGE_BZ2`) — while a bare ELF vmlinux is `ELF`; on aarch64
/// an EFI-stub Image ("MZ") loads as `RAW` and a gzipped Image as
/// `PE_GZ`.
fn kernel_format(bytes: &[u8], arch: &str) -> Option<u32> {
    const ELF: u32 = 1;
    const PE_GZ: u32 = 2;
    const IMAGE_BZ2: u32 = 3;
    const IMAGE_GZ: u32 = 4;
    const IMAGE_ZSTD: u32 = 5;
    const RAW: u32 = 0;

    if bytes.starts_with(&[0x7f, b'E', b'L', b'F']) {
        return Some(ELF);
    }
    let find = |magic: &[u8]| bytes.windows(magic.len()).position(|w| w == magic);
    match arch {
        "x86_64" => {
            let candidates = [
                (find(&[0x1f, 0x8b, 0x08]), IMAGE_GZ),
                (find(&[0x28, 0xb5, 0x2f, 0xfd]), IMAGE_ZSTD),
                (find(b"BZh"), IMAGE_BZ2),
            ];
            candidates
                .iter()
                .filter_map(|(pos, fmt)| pos.map(|p| (p, *fmt)))
                .min_by_key(|(p, _)| *p)
                .map(|(_, fmt)| fmt)
        }
        "aarch64" => {
            if bytes.starts_with(b"MZ") {
                Some(RAW)
            } else {
                find(&[0x1f, 0x8b, 0x08]).map(|_| PE_GZ)
            }
        }
        _ => None,
    }
}

/// Boot the pet VM with its own kernel and systemd. With a command,
/// installs a per-run oneshot unit that runs it (console output on
/// hvc0), powers the VM off, and leaves its exit status in
/// [`EXIT_STATUS_FILE`] for us to read back and exit with; without
/// one, boots to the autologin root console and exits 0 on guest
/// poweroff. Never returns on success.
fn run_systemd_vm(
    request: &RunRequest,
    record: &VmRecord,
    vm_dir: &Path,
    rootfs: &Path,
    kernel: GuestKernel,
) -> anyhow::Result<()> {
    let volumes = parse_volumes(&request.volumes)?;
    let guest_paths: Vec<String> = request
        .volumes
        .iter()
        .map(|v| split_volume(v).map(|(_, guest)| guest))
        .collect::<Result<_, _>>()?;
    set_fstab_volumes(rootfs, &guest_paths)?;

    let interactive = request.command.is_empty();
    let exit_file = rootfs.join(EXIT_STATUS_FILE);
    let _ = std::fs::remove_file(&exit_file);
    if interactive {
        remove_run_unit(rootfs)?;
    } else {
        let env = unit_env(request, record);
        let workdir = request.workdir.clone().unwrap_or_else(|| "/root".into());
        write_run_unit(rootfs, &systemd_unit(&request.command, &env, &workdir)?)?;
    }

    // Checked here rather than left to the VMM: krun-vmm's Kvm setup
    // panics (not errors) on a missing /dev/kvm.
    anyhow::ensure!(
        Path::new("/dev/kvm").exists(),
        "/dev/kvm not found; ocivmm microVMs need KVM"
    );

    let ports = parse_ports(&request.publish)?;
    let passt_socket = spawn_passt(vm_dir, &ports)?;

    let spec = microvm::VmSpec {
        cpus: request.cpus.unwrap_or_else(default_cpus),
        mem_mib: request.mem.unwrap_or(DEFAULT_MEM_MIB),
        rootfs: path_string(rootfs)?,
        volumes,
        kernel: microvm::KernelSpec {
            path: path_string(&kernel.vmlinuz)?,
            format: kernel.format,
            initramfs: kernel.initramfs.as_deref().map(path_string).transpose()?,
            cmdline: kernel_cmdline(),
        },
        passt_socket,
    };

    eprintln!(
        "ocivmm: booting {} (image {}, kernel {}, initramfs {}, {} vcpu(s), {} MiB)",
        record.name,
        record.image,
        kernel
            .vmlinuz
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        kernel
            .initramfs
            .as_deref()
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "none".to_string()),
        spec.cpus,
        spec.mem_mib
    );
    let status = boot_in_child(vm_dir, &spec)?;
    let code = if interactive {
        status.code().unwrap_or(1)
    } else {
        remove_run_unit(rootfs)?;
        if !status.success() {
            anyhow::bail!("the VM exited abnormally ({status})");
        }
        let raw = std::fs::read_to_string(&exit_file).with_context(|| {
            format!(
                "the guest powered off without reporting a command status \
                 (missing {})",
                exit_file.display()
            )
        })?;
        let _ = std::fs::remove_file(&exit_file);
        parse_exit_status(&raw)
    };
    std::process::exit(code);
}

/// The guest environment for the generated oneshot unit: the image's
/// declared env (or the standard `PATH` fallback), `HOME`, the host's
/// `TERM`, then `-e` overrides — same merge the bundled-init path uses.
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

/// Map a unit's `$EXIT_STATUS` content to our own exit code: the
/// numeric status when the command exited, 1 for anything else (a
/// signal name like `KILL`, or garbage).
fn parse_exit_status(raw: &str) -> i32 {
    raw.trim().parse().unwrap_or(1)
}

/// Render the per-run oneshot unit. `SuccessAction`/`FailureAction`
/// power the VM off once the command (and the `ExecStopPost` that
/// records `$EXIT_STATUS` — systemd leaves `$`-in-the-middle-of-a-word
/// unexpanded, so the quoted `sh -c` sees it) has finished; oneshot
/// units have no start timeout, so long builds are fine.
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
         Wants=network-online.target\n\
         After=network-online.target\n\
         SuccessAction=poweroff\n\
         FailureAction=poweroff\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         StandardOutput=tty\n\
         StandardError=tty\n\
         TTYPath=/dev/hvc0\n",
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

/// Quote one `ExecStart` argument per systemd.service syntax: double
/// quotes with `\`-escapes, `%` doubled (specifier syntax), `$`
/// doubled (variable expansion).
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

/// Install the per-run unit: the unit file, a `multi-user.target.wants`
/// symlink, and a mask for `serial-getty@hvc0` (the autologin console
/// would fight the unit for the same tty).
fn write_run_unit(rootfs: &Path, unit: &str) -> anyhow::Result<()> {
    let system = rootfs.join("etc/systemd/system");
    let wants = system.join("multi-user.target.wants");
    std::fs::create_dir_all(&wants).with_context(|| format!("creating {}", wants.display()))?;
    std::fs::write(system.join(RUN_UNIT), unit).with_context(|| format!("writing {RUN_UNIT}"))?;
    let link = wants.join(RUN_UNIT);
    let _ = std::fs::remove_file(&link);
    symlink(&format!("/etc/systemd/system/{RUN_UNIT}"), &link)?;
    let mask = system.join("serial-getty@hvc0.service");
    let _ = std::fs::remove_file(&mask);
    symlink("/dev/null", &mask)?;
    Ok(())
}

/// Remove everything [`write_run_unit`] installed (idempotent).
fn remove_run_unit(rootfs: &Path) -> anyhow::Result<()> {
    let system = rootfs.join("etc/systemd/system");
    let _ = std::fs::remove_file(system.join("multi-user.target.wants").join(RUN_UNIT));
    let _ = std::fs::remove_file(system.join(RUN_UNIT));
    let _ = std::fs::remove_file(system.join("serial-getty@hvc0.service"));
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

/// Point systemd at this run's `--volume`s: an ocivmm-managed block in
/// the guest's `/etc/fstab` (`ocivmm0 /guest/path virtiofs ...`),
/// rewritten wholesale on every boot so removed volumes disappear too;
/// systemd's fstab generator mounts them. Mount points are created
/// host-side.
fn set_fstab_volumes(rootfs: &Path, guest_paths: &[String]) -> anyhow::Result<()> {
    for guest in guest_paths {
        let mount_point = rootfs.join(guest.trim_start_matches('/'));
        std::fs::create_dir_all(&mount_point)
            .with_context(|| format!("creating mount point {}", mount_point.display()))?;
    }
    let fstab_path = rootfs.join("etc/fstab");
    let existing = std::fs::read_to_string(&fstab_path).unwrap_or_default();
    let lines: Vec<String> = guest_paths
        .iter()
        .enumerate()
        .map(|(index, guest)| format!("ocivmm{index} {guest} virtiofs defaults 0 0"))
        .collect();
    std::fs::create_dir_all(rootfs.join("etc")).context("creating /etc")?;
    std::fs::write(&fstab_path, splice_fstab(&existing, &lines))
        .with_context(|| format!("writing {}", fstab_path.display()))?;
    Ok(())
}

/// Pure fstab surgery for [`set_fstab_volumes`]: drop any previous
/// ocivmm-managed block, append the new one (if any lines).
fn splice_fstab(existing: &str, lines: &[String]) -> String {
    const BEGIN: &str = "# ocivmm-volumes-begin";
    const END: &str = "# ocivmm-volumes-end";
    let mut out = String::new();
    let mut in_block = false;
    for line in existing.lines() {
        match line.trim() {
            BEGIN => in_block = true,
            END => in_block = false,
            _ if !in_block => {
                out.push_str(line);
                out.push('\n');
            }
            _ => {}
        }
    }
    if !lines.is_empty() {
        out.push_str(BEGIN);
        out.push('\n');
        for line in lines {
            out.push_str(line);
            out.push('\n');
        }
        out.push_str(END);
        out.push('\n');
    }
    out
}

/// Start passt for one VM boot and return its unix socket path. passt
/// daemonizes itself once the socket is listening (so waiting for the
/// foreground parent is the readiness barrier) and `--one-off` makes
/// it exit when the VMM disconnects — no lifecycle management needed.
/// `--publish` mappings become passt's own `-t host:guest` TCP
/// forwards.
fn spawn_passt(vm_dir: &Path, ports: &[String]) -> anyhow::Result<String> {
    let socket = vm_dir.join("passt.sock");
    let _ = std::fs::remove_file(&socket);
    let passt = std::env::var("OCIVMM_PASST").unwrap_or_else(|_| "passt".to_string());
    let mut command = std::process::Command::new(&passt);
    command
        .arg("--quiet")
        .arg("--one-off")
        .arg("--socket")
        .arg(&socket);
    for port in ports {
        command.arg("-t").arg(port);
    }
    let status = command.status().with_context(|| {
        format!("running {passt:?} (passt provides guest networking; install the passt package)")
    })?;
    anyhow::ensure!(status.success(), "passt failed to start ({status})");
    path_string(&socket)
}

/// Parse every `--volume HOST:GUEST` into `(virtiofs_tag, host_path)`
/// device pairs; the guest destinations are consumed separately, in
/// the same order, by [`set_fstab_volumes`].
fn parse_volumes(volumes: &[String]) -> anyhow::Result<Vec<(String, String)>> {
    volumes
        .iter()
        .enumerate()
        .map(|(index, volume)| {
            let (host, _guest) = split_volume(volume)?;
            let host_dir = PathBuf::from(&host);
            anyhow::ensure!(
                host_dir.is_dir(),
                "--volume {volume}: host path {host} is not a directory"
            );
            let host = path_string(
                &host_dir
                    .canonicalize()
                    .with_context(|| format!("--volume {volume}: resolving {host}"))?,
            )?;
            Ok((format!("ocivmm{index}"), host))
        })
        .collect()
}

/// Split one `HOST:GUEST` volume argument and validate the guest side.
fn split_volume(volume: &str) -> anyhow::Result<(String, String)> {
    let (host, guest) = volume
        .split_once(':')
        .with_context(|| format!("--volume {volume}: expected HOST_DIR:GUEST_DIR"))?;
    anyhow::ensure!(
        guest.starts_with('/'),
        "--volume {volume}: guest path must be absolute"
    );
    anyhow::ensure!(
        !guest.contains('\'') && !host.contains('\'') && !guest.contains(char::is_whitespace),
        "--volume {volume}: paths with quotes or whitespace are not supported"
    );
    Ok((host.to_string(), guest.to_string()))
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
/// with the same `NAME` in place and appending the rest — so a later
/// `-e PATH=...` overrides the image's `PATH` instead of duplicating it.
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
/// `/etc/hostname` usable. OCI base images ship these absent, empty,
/// or as dangling symlinks (container engines bind-mount over them at
/// run time; there is no engine here, the rootfs *is* the machine).
///
/// resolv.conf handling is two-phased by design: before provisioning
/// the only consumer is the provisioning *container* (host network
/// namespace), so the right content is the **host's own resolv.conf
/// verbatim** — loopback stub included, since host loopback works
/// there and public resolvers may well be blocked (Azure-hosted CI
/// runners are). Provisioning then hands the file over to
/// systemd-resolved (a symlink into `/run/systemd/resolve/`), which
/// gets the *VM's* DNS dynamically from the DHCP lease — passt
/// advertises itself as the resolver and forwards to whatever the
/// host uses. That symlink (dangling from the host's point of view)
/// is therefore deliberately left alone here, as is any regular file
/// with nameservers in it — a pet VM's own customizations belong to
/// it.
fn ensure_guest_files(rootfs: &Path, name: &str) -> anyhow::Result<()> {
    let etc = rootfs.join("etc");
    std::fs::create_dir_all(&etc).with_context(|| format!("creating {}", etc.display()))?;

    let resolv = etc.join("resolv.conf");
    let usable = match std::fs::symlink_metadata(&resolv) {
        Ok(meta) if meta.file_type().is_symlink() => {
            let target = std::fs::read_link(&resolv).unwrap_or_default();
            if target.to_string_lossy().contains("systemd/resolve") {
                // Provisioned: systemd-resolved owns DNS in the guest.
                true
            } else {
                // A dangling stub link from the image itself.
                std::fs::remove_file(&resolv)
                    .with_context(|| format!("removing symlink {}", resolv.display()))?;
                false
            }
        }
        Ok(_) => std::fs::read_to_string(&resolv)
            .map(|content| !nameservers_from(&content).is_empty())
            .unwrap_or(false),
        Err(_) => false,
    };
    if !usable {
        std::fs::write(&resolv, host_resolv_conf())
            .with_context(|| format!("writing {}", resolv.display()))?;
    }

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

/// Unconditionally point the rootfs's resolv.conf at the host's (used
/// at create time, before the provisioning container runs — see
/// [`ensure_guest_files`] for the two-phase design).
fn reset_resolv_conf(rootfs: &Path) -> anyhow::Result<()> {
    let resolv = rootfs.join("etc/resolv.conf");
    let _ = std::fs::remove_file(&resolv);
    std::fs::write(&resolv, host_resolv_conf())
        .with_context(|| format!("writing {}", resolv.display()))
}

/// The host's own resolv.conf, verbatim (for the host-network
/// provisioning container — see [`ensure_guest_files`]), with a
/// public-resolver fallback only when the host has nothing usable to
/// offer at all.
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

/// Raise `RLIMIT_NOFILE` to its hard limit, best effort: libkrun's
/// in-process virtiofs server holds an fd per open guest file, and the
/// default soft limit (often 1024) is nowhere near enough for a
/// package-manager transaction or a cargo build over virtiofs.
fn raise_nofile_limit() {
    use rustix::process::{Resource, getrlimit, setrlimit};
    let mut limit = getrlimit(Resource::Nofile);
    if limit.current < limit.maximum {
        limit.current = limit.maximum;
        if let Err(e) = setrlimit(Resource::Nofile, limit) {
            tracing::debug!(error = %e, "could not raise RLIMIT_NOFILE");
        }
    }
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
    fn compare_versions_orders_numerically() {
        use std::cmp::Ordering;
        assert_eq!(
            compare_versions("6.12.10-300.el10.x86_64", "6.12.9-400.el10.x86_64"),
            Ordering::Greater
        );
        assert_eq!(
            compare_versions("6.8.0-31-generic", "6.8.0-31-generic"),
            Ordering::Equal
        );
        assert_eq!(compare_versions("5.14.0", "6.1.0"), Ordering::Less);
    }

    #[test]
    fn extract_vmlinux_unwraps_gzip_and_zstd_bzimages() {
        use std::io::Write as _;
        let fake_elf = {
            let mut v = vec![0x7f, b'E', b'L', b'F'];
            v.extend_from_slice(&[0u8; 64]);
            v
        };
        // bzImage-shaped: setup stub bytes, then the compressed payload.
        let mut gz_image = vec![0u8; 512];
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&fake_elf).unwrap();
        gz_image.extend_from_slice(&encoder.finish().unwrap());
        assert_eq!(extract_vmlinux(&gz_image).unwrap(), fake_elf);

        let mut zst_image = vec![0u8; 512];
        zst_image.extend_from_slice(&ruzstd::encoding::compress_to_vec(
            fake_elf.as_slice(),
            ruzstd::encoding::CompressionLevel::Fastest,
        ));
        assert_eq!(extract_vmlinux(&zst_image).unwrap(), fake_elf);

        assert!(extract_vmlinux(&[0u8; 128]).is_err());
    }

    #[test]
    fn kernel_format_detects_elf_and_compressed_bzimages() {
        // Bare ELF vmlinux.
        assert_eq!(
            kernel_format(&[0x7f, b'E', b'L', b'F', 0, 0], "x86_64"),
            Some(1)
        );
        // bzImage wrapping a zstd-compressed vmlinux.
        let mut bz = vec![0u8; 512];
        bz.extend_from_slice(&[0x28, 0xb5, 0x2f, 0xfd]);
        assert_eq!(kernel_format(&bz, "x86_64"), Some(5));
        // bzImage wrapping a gzip-compressed vmlinux.
        let mut gz = vec![0u8; 512];
        gz.extend_from_slice(&[0x1f, 0x8b, 0x08]);
        assert_eq!(kernel_format(&gz, "x86_64"), Some(4));
        // Unrecognized.
        assert_eq!(kernel_format(&[0u8; 64], "x86_64"), None);
        // aarch64 EFI-stub Image loads raw.
        assert_eq!(kernel_format(b"MZ\x00\x00", "aarch64"), Some(0));
    }

    #[test]
    fn splice_fstab_replaces_only_the_managed_block() {
        let existing = "/dev/vda1 / ext4 defaults 0 1\n\
                        # ocivmm-volumes-begin\n\
                        ocivmm0 /old virtiofs defaults 0 0\n\
                        # ocivmm-volumes-end\n";
        let out = splice_fstab(
            existing,
            &["ocivmm0 /src virtiofs defaults 0 0".to_string()],
        );
        assert!(out.contains("/dev/vda1 / ext4"));
        assert!(out.contains("ocivmm0 /src virtiofs"));
        assert!(!out.contains("/old"));
        let cleared = splice_fstab(&out, &[]);
        assert!(!cleared.contains("ocivmm"));
        assert!(cleared.contains("/dev/vda1 / ext4"));
    }

    #[test]
    fn systemd_unit_escapes_and_powers_off() {
        let unit = systemd_unit(
            &["bash".into(), "/src/ci/vm-ci.sh".into(), "100%".into()],
            &["PATH=/usr/bin".into()],
            "/root",
        )
        .unwrap();
        assert!(unit.contains("ExecStart=\"bash\" \"/src/ci/vm-ci.sh\" \"100%%\"\n"));
        assert!(unit.contains("SuccessAction=poweroff"));
        assert!(unit.contains("FailureAction=poweroff"));
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

    #[test]
    fn parse_exit_status_maps_signals_to_failure() {
        assert_eq!(parse_exit_status("0\n"), 0);
        assert_eq!(parse_exit_status("42"), 42);
        assert_eq!(parse_exit_status("KILL"), 1);
        assert_eq!(parse_exit_status(""), 1);
    }

    #[test]
    fn split_volume_requires_absolute_guest_path() {
        assert!(split_volume("/host:relative").is_err());
        assert!(split_volume("/host").is_err());
        assert!(split_volume("/host:/guest").is_ok());
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
        // Loopback stubs are deliberately kept: the file's first
        // consumer is the host-network provisioning container, where
        // the host's own 127.0.0.53 works (and public resolvers may
        // be blocked); VM boots get DNS from systemd-resolved instead.
        let content = "nameserver 127.0.0.53\nnameserver 10.0.0.2\noptions edns0\n";
        assert_eq!(
            nameservers_from(content),
            vec!["127.0.0.53".to_string(), "10.0.0.2".to_string()]
        );
    }
}
