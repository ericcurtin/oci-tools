//! The microVM itself: libkrun's own `krun-vmm`/`krun-devices`/
//! `krun-polly` crates, **statically linked** as ordinary, pinned Rust
//! dependencies — no `libkrun.so`, no `dlopen`, no run-time linkage of
//! any kind. This module is the (much thinner) equivalent of libkrun's
//! C-API layer: where `chroot_vm.c` would call `krun_set_root`/
//! `krun_set_kernel`/`krun_start_enter` through the shared library,
//! [`boot`] assembles the same `VmResources` and enters the same
//! `build_microvm` + event-loop directly — ported line for line from
//! `krun_start_enter` in `~/git/libkrun`'s `src/libkrun/src/lib.rs`
//! (the exact revision the workspace pins), minus everything a pet VM
//! never uses (TEE, GPU, vsock/TSI, the embedded init, and libkrunfw:
//! pet VMs boot the *distro's own* kernel via the external-kernel
//! loader, and provisioning runs as a container, so the bundled-kernel
//! path doesn't exist here at all).
//!
//! One deliberate consequence: the guest kernel is always the guest's
//! own `/boot/vmlinuz-*` (`ExternalKernel`; on x86_64 the loader scans
//! the bzImage for its gzip/zstd/bzip2 magic and ELF-loads the inner
//! vmlinux), the initramfs is the guest's own dracut image, PID 1 is
//! the guest's own systemd, and networking is a passt-backed
//! virtio-net device — every moving part belongs to the distro except
//! the VMM compiled into this binary.
//!
//! On success [`boot`] never returns: the calling process *becomes*
//! the VMM (`Vmm::stop` ends it with `_exit` when the guest powers
//! off), which is why callers that need to keep running boot through
//! the re-exec'd `ocivmm __boot` child instead.

use serde::{Deserialize, Serialize};

/// The virtiofs tag reserved for the root filesystem (libkrun's
/// `KRUN_FS_ROOT_TAG`); also the tag the generated dracut initramfs
/// mounts (`root=virtiofs:/dev/root`).
pub const ROOT_TAG: &str = "/dev/root";

/// The guest's own kernel, found in its rootfs by `main.rs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KernelSpec {
    /// Absolute host path of the kernel image (the rootfs's own
    /// `/boot/vmlinuz-*`).
    pub path: String,
    /// `KRUN_KERNEL_FORMAT_*`-numbered image format, sniffed from the
    /// image's own bytes (see `main.rs::kernel_format`).
    pub format: u32,
    /// Absolute host path of the initramfs (the dracut image that
    /// mounts the virtiofs root), if any.
    pub initramfs: Option<String>,
    /// The full kernel command line.
    pub cmdline: String,
}

/// The full description of the VM to boot — plain, serializable data,
/// handed to the `ocivmm __boot` child as JSON (see module docs for
/// why a child).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmSpec {
    /// Number of vCPUs.
    pub cpus: u8,
    /// Guest RAM in MiB.
    pub mem_mib: u32,
    /// Host directory to expose as the guest's root filesystem.
    pub rootfs: String,
    /// Extra host directories to expose as virtiofs devices:
    /// `(tag, host_path)`; the guest mounts them by tag via the
    /// ocivmm-managed fstab block.
    pub volumes: Vec<(String, String)>,
    /// The guest's own kernel.
    pub kernel: KernelSpec,
    /// Path of passt's already-listening unix socket, backing the
    /// guest's one virtio-net device.
    pub passt_socket: String,
}

#[cfg(target_os = "linux")]
mod imp {
    use anyhow::Context as _;
    use krun_devices::virtio::net::device::VirtioNetBackend;
    use krun_polly::event_manager::EventManager;
    use krun_vmm::resources::{
        DefaultVirtioConsoleConfig, SerialConsoleConfig, VirtioConsoleConfigMode, VmResources,
    };
    use krun_vmm::vmm_config::external_kernel::{ExternalKernel, KernelFormat};
    use krun_vmm::vmm_config::fs::FsDeviceConfig;
    use krun_vmm::vmm_config::machine_config::VmConfig;
    use krun_vmm::vmm_config::net::NetworkInterfaceConfig;

    use super::VmSpec;

    /// A fixed locally-administered MAC for the guest's one virtio-net
    /// device (the same convention krunvm uses; nothing routes on it,
    /// passt NATs everything).
    const GUEST_MAC: [u8; 6] = [0x5a, 0x94, 0xef, 0xe4, 0x0c, 0xee];

    /// libkrun's `COMPAT_NET_FEATURES`: the default virtio-net feature
    /// set for unixstream backends such as passt — CSUM | GUEST_CSUM |
    /// GUEST_TSO4 | GUEST_UFO | HOST_TSO4 | HOST_UFO.
    const COMPAT_NET_FEATURES: u32 = 1 | (1 << 1) | (1 << 7) | (1 << 10) | (1 << 11) | (1 << 14);

    /// The DAX window size libkrun's own `krun_set_root` gives the
    /// root filesystem ("a conservative 512 MB window").
    const ROOT_SHM_SIZE: usize = 1 << 29;

    /// Map `main.rs`'s sniffed `KRUN_KERNEL_FORMAT_*` number to the
    /// loader's enum — the same mapping `krun_set_kernel` applies
    /// (`Raw` is aarch64-only there too; x86_64 raw kernels went
    /// through libkrunfw-style mapping this port has no use for).
    fn kernel_format(format: u32) -> anyhow::Result<KernelFormat> {
        Ok(match format {
            #[cfg(target_arch = "aarch64")]
            0 => KernelFormat::Raw,
            1 => KernelFormat::Elf,
            2 => KernelFormat::PeGz,
            3 => KernelFormat::ImageBz2,
            4 => KernelFormat::ImageGz,
            5 => KernelFormat::ImageZstd,
            other => anyhow::bail!("unsupported kernel image format {other}"),
        })
    }

    /// See the module docs: `krun_start_enter`, inlined and pruned.
    pub fn boot(spec: &VmSpec) -> anyhow::Result<std::convert::Infallible> {
        let mut vmr = VmResources::default();

        vmr.set_vm_config(&VmConfig {
            vcpu_count: Some(spec.cpus),
            mem_size_mib: Some(spec.mem_mib as usize),
            ht_enabled: Some(false),
            cpu_template: None,
        })
        .map_err(|e| anyhow::anyhow!("invalid VM config: {e}"))?;

        // The root filesystem, exactly as krun_set_root configures it
        // (minus the embedded-init virtual entry: PID 1 is the guest's
        // own systemd, not libkrun's init).
        vmr.add_fs_device(FsDeviceConfig {
            fs_id: super::ROOT_TAG.to_string(),
            shared_dir: Some(spec.rootfs.clone()),
            shm_size: Some(ROOT_SHM_SIZE),
            read_only: false,
            virtual_entries: Vec::new(),
        });
        for (tag, path) in &spec.volumes {
            vmr.add_fs_device(FsDeviceConfig {
                fs_id: tag.clone(),
                shared_dir: Some(path.clone()),
                shm_size: None,
                read_only: false,
                virtual_entries: Vec::new(),
            });
        }

        // The guest's own kernel, as krun_set_kernel stores it (the
        // provided cmdline replaces the default+KRUN_* one outright —
        // there is no embedded init to configure).
        let initramfs_size = match &spec.kernel.initramfs {
            Some(path) => std::fs::metadata(path)
                .with_context(|| format!("reading initramfs metadata for {path}"))?
                .len(),
            None => 0,
        };
        vmr.external_kernel = Some(ExternalKernel {
            path: spec.kernel.path.clone().into(),
            format: kernel_format(spec.kernel.format)?,
            initramfs_path: spec.kernel.initramfs.clone().map(Into::into),
            initramfs_size,
            cmdline: Some(spec.kernel.cmdline.clone()),
        });

        // Stdio as the guest consoles: a legacy 16550 serial (ttyS0)
        // *and* a virtio console (hvc0), both as
        // krun_add_serial_console_default/krun_add_virtio_console_default
        // wire them. The serial one matters because distro kernels
        // build virtio_console as a module — everything before the
        // initramfs loads it (early boot, dracut root-mount failures,
        // panics) is only visible on the built-in serial driver.
        //
        // Console *input* is stdin only when stdin is a real terminal:
        // a hung-up/closed stdin (every CI step) otherwise floods the
        // device event loop with EPOLLHUP forever — found the hard
        // way, a warning storm followed by a VMM panic. /dev/null
        // polls quiet; the fd is deliberately leaked (this function
        // never returns).
        let input_fd = if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            0
        } else {
            std::os::fd::IntoRawFd::into_raw_fd(
                std::fs::File::open("/dev/null").context("opening /dev/null")?,
            )
        };
        vmr.serial_consoles.push(SerialConsoleConfig {
            input_fd,
            output_fd: 1,
        });
        vmr.virtio_consoles
            .push(VirtioConsoleConfigMode::Autoconfigure(
                DefaultVirtioConsoleConfig {
                    input_fd,
                    output_fd: 1,
                    err_fd: 2,
                },
            ));

        // One passt-backed virtio-net device, as krun_add_net_unixstream
        // configures it. No vsock/TSI: that needs libkrunfw's patched
        // kernel, and this VMM only ever boots distro kernels.
        vmr.add_network_interface(NetworkInterfaceConfig {
            iface_id: "eth0".to_string(),
            backend: VirtioNetBackend::UnixstreamPath(spec.passt_socket.clone().into()),
            mac: GUEST_MAC,
            features: COMPAT_NET_FEATURES,
        })
        .map_err(|e| anyhow::anyhow!("configuring virtio-net: {e:?}"))?;

        // krun_start_enter's tail: build the microVM and run its event
        // loop until the guest powers off (Vmm::stop _exit()s the
        // process — this function never returns on success).
        let mut event_manager = EventManager::new()
            .map_err(|e| anyhow::anyhow!("creating the VMM event manager: {e:?}"))?;
        let (sender, _receiver) = crossbeam_channel::unbounded();
        let _vmm = krun_vmm::builder::build_microvm(&vmr, &mut event_manager, None, sender)
            .map_err(|e| anyhow::anyhow!("building the microVM: {e:?}"))?;
        loop {
            event_manager
                .run()
                .map_err(|e| anyhow::anyhow!("VMM event loop: {e:?}"))?;
        }
    }
}

#[cfg(target_os = "linux")]
pub use imp::boot;

/// KVM is Linux-only; everywhere else `ocivmm` still builds (and can
/// create/list/rm VM state) but cannot boot.
#[cfg(not(target_os = "linux"))]
pub fn boot(_spec: &VmSpec) -> anyhow::Result<std::convert::Infallible> {
    anyhow::bail!("ocivmm can only run VMs on Linux (KVM)");
}
