// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
//
// oci-vmm original: this is the top-level assembly Firecracker's own
// `builder.rs`/`lib.rs` perform, rewritten from scratch for oci-vmm's
// own much narrower device set (one virtio-blk root disk, one
// virtio-net, one serial console, no snapshots/balloon/vsock/rate
// limiters/API server) and its own PIO/MMIO dispatch loop — none of
// Firecracker's own request-handling, seccomp, or jailer machinery
// applies to a single-shot "boot, run one workload, exit" VMM.

//! Ties every module in this crate together: guest memory, the MP
//! table, kernel/initrd/cmdline loading, the legacy serial/i8042
//! devices, the PCI bus with one virtio-blk and one virtio-net device
//! behind it, one thread per vCPU, and the shutdown path (the guest's
//! own `reboot=k` i8042 reset pulse ends the whole process — there is
//! no ACPI poweroff, and none is needed: [`VmmConfig`] boots exactly
//! one workload and exits).

use std::fs::File;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use event_manager::{
    EventManager as BaseEventManager, EventOps, Events, MutEventSubscriber, SubscriberOps,
};
use kvm_ioctls::{Kvm, VcpuExit};
use tracing::{debug, error, warn};
use vmm_sys_util::epoll::EventSet;
use vmm_sys_util::eventfd::EventFd;

use crate::arch::mptable;
use crate::boot;
use crate::legacy::i8042::I8042Device;
use crate::legacy::serial::SerialDevice;
use crate::mem::{self};
use crate::pci::bus::{PciBus, PciConfigIo, PciRoot};
use crate::pci::{PciDevice, PciSBDF};
use crate::virtio::block::VirtioBlock;
use crate::virtio::device::VirtioDevice;
use crate::virtio::net::VirtioNet;
use crate::virtio::transport::pci::device::{CAPABILITY_BAR_SIZE, VirtioPciDevice};
use crate::vstate::vcpu::Vcpu;
use crate::vstate::vm::Vm;

/// A subscriber the shared [`event_manager::EventManager`] drives —
/// every virtio device implements this via [`VirtioDevice`]'s own
/// supertrait bound.
type EventManager = BaseEventManager<Arc<Mutex<dyn MutEventSubscriber>>>;

/// One virtio-pci device's MMIO BAR range, for [`dispatch_mmio_read`]/
/// [`dispatch_mmio_write`] to route a vCPU's MMIO exit to the right
/// device.
type MmioDevice = (u64, u64, Arc<Mutex<dyn PciDevice>>);

/// Turns the guest's own i8042 reset pulse (`reset_evt`) into an
/// `EventManager` subscriber, so shutdown detection needs no thread of
/// its own: [`run`] drives the whole `EventManager` — devices and this
/// watcher alike — on a single thread (`dyn MutEventSubscriber` has no
/// `Send` bound, so an `EventManager` full of them cannot cross a
/// thread boundary at all; it doesn't need to, since vCPU threads
/// don't touch it either).
struct ShutdownWatcher {
    reset_evt: EventFd,
    should_stop: Arc<AtomicBool>,
}

impl MutEventSubscriber for ShutdownWatcher {
    fn process(&mut self, _event: Events, _ops: &mut EventOps) {
        let _ = self.reset_evt.read();
        self.should_stop.store(true, Ordering::Relaxed);
    }

    fn init(&mut self, ops: &mut EventOps) {
        if let Err(err) = ops.add(Events::new(&self.reset_evt, EventSet::IN)) {
            error!("failed to register the shutdown watcher: {err}");
        }
    }
}

/// Guest-physical base of the 64-bit MMIO region virtio-pci
/// capability BARs are allocated from, one [`CAPABILITY_BAR_SIZE`]
/// window per device: 1 TiB, comfortably above any guest RAM size
/// this VMM is ever configured with (real usage is 2-64 GiB), so it
/// can never collide with a RAM region.
const VIRTIO_PCI_BAR_REGION_START: u64 = 1 << 40;

/// Legacy PCI configuration-space I/O ports (conf1).
const PCI_CONFIG_IO_PORT: u16 = 0xcf8;
const PCI_CONFIG_IO_PORT_END: u16 = 0xcff;

/// Everything needed to boot one guest and run one workload.
pub struct VmmConfig {
    /// Number of vCPUs.
    pub vcpu_count: u8,
    /// Guest RAM in MiB.
    pub mem_mib: u32,
    /// The guest's own kernel image (a bzImage).
    pub kernel_path: PathBuf,
    /// The guest's own initramfs, if any.
    pub initrd_path: Option<PathBuf>,
    /// The full kernel command line.
    pub cmdline: String,
    /// The pet VM's root filesystem disk image.
    pub disk_path: PathBuf,
    /// Whether to expose the disk read-only.
    pub disk_read_only: bool,
    /// The guest's virtio-net MAC address.
    pub net_mac: [u8; 6],
    /// An already-connected unix-stream socket to passt.
    pub passt_socket: UnixStream,
}

/// Errors starting the VMM. Once vCPU threads are running, further
/// errors end the process directly (see the module docs) rather than
/// being returned here.
#[derive(Debug, thiserror::Error)]
pub enum BuilderError {
    /// Failed a KVM setup step. A plain formatted message rather than
    /// a typed source: this one variant collects errors from several
    /// unrelated crates (`kvm_ioctls::Error`, `VmError`,
    /// `vmm_sys_util::errno::Error`, `std::io::Error`, ...), and a
    /// setup-time failure only ever needs to be reported once, not
    /// programmatically matched on.
    #[error("KVM setup: {0}")]
    Kvm(String),
    /// Failed to build guest memory.
    #[error("guest memory: {0}")]
    Memory(#[from] mem::MemoryError),
    /// Failed to set up the MP table.
    #[error("MP table: {0}")]
    Mptable(#[from] mptable::MptableError),
    /// Failed loading the kernel/initrd/cmdline/boot_params.
    #[error("boot: {0}")]
    Boot(#[from] boot::BootError),
    /// Failed to open the root disk image.
    #[error("opening disk image {0}: {1}")]
    Disk(PathBuf, #[source] std::io::Error),
    /// Failed constructing a virtio device.
    #[error("virtio device: {0}")]
    VirtioDevice(String),
    /// Failed constructing the vCPUs.
    #[error("vcpu setup: {0}")]
    Vcpu(String),
    /// Failed to create the event manager.
    #[error("event manager: {0}")]
    EventManager(String),
}

/// Boot the guest and run until it shuts itself down.
///
/// **Never returns on success**: the guest's own `reboot=k` i8042
/// reset pulse (systemd's `SuccessAction`/`FailureAction=poweroff` on
/// the generated CI unit, or a plain interactive `poweroff`/`reboot`)
/// ends the whole process directly.
pub fn run(config: VmmConfig) -> Result<std::convert::Infallible, BuilderError> {
    let guest_mem = mem::create((config.mem_mib as usize) << 20)?;
    mptable::setup_mptable(&guest_mem, config.vcpu_count)?;

    let kvm = Kvm::new().map_err(|e| BuilderError::Kvm(e.to_string()))?;
    let vm = Arc::new(Vm::new(&kvm).map_err(|e| BuilderError::Kvm(e.to_string()))?);
    vm.register_memory_regions(&guest_mem)
        .map_err(|e| BuilderError::Kvm(e.to_string()))?;

    let kernel_file = File::open(&config.kernel_path)
        .map_err(|e| BuilderError::Disk(config.kernel_path.clone(), e))?;
    let entry_point = boot::load_kernel(&kernel_file, &guest_mem)?;
    let initrd = match &config.initrd_path {
        Some(path) => {
            let mut f = File::open(path).map_err(|e| BuilderError::Disk(path.clone(), e))?;
            Some(boot::load_initrd(&mut f, &guest_mem)?)
        }
        None => None,
    };
    boot::write_cmdline(&guest_mem, &config.cmdline)?;
    boot::configure_boot_params(&guest_mem, &initrd)?;

    // --- Legacy devices: serial (COM1, stdio) + i8042 (reset only) ---
    let reset_evt =
        EventFd::new(libc::EFD_NONBLOCK).map_err(|e| BuilderError::Kvm(e.to_string()))?;
    let com1_irq = 4;
    let com1_evt =
        EventFd::new(libc::EFD_NONBLOCK).map_err(|e| BuilderError::Kvm(e.to_string()))?;
    vm.register_irq(&com1_evt, com1_irq)
        .map_err(|e| BuilderError::Kvm(e.to_string()))?;
    // Console output only for now (no host stdin forwarding): CI's own
    // use is a build log, not an interactive session.
    let serial = Arc::new(Mutex::new(SerialDevice::new(com1_evt, None)));
    let i8042 = Arc::new(Mutex::new(I8042Device::new(
        reset_evt
            .try_clone()
            .map_err(|e| BuilderError::Kvm(e.to_string()))?,
    )));

    // --- PCI bus + virtio devices -------------------------------------
    let pci_bus = Arc::new(Mutex::new(PciBus::new(PciRoot::new(None))));
    let pci_config_io = Arc::new(Mutex::new(PciConfigIo::new(pci_bus.clone())));

    let block: Arc<Mutex<dyn VirtioDevice>> = Arc::new(Mutex::new(
        VirtioBlock::new(
            config.disk_path.clone(),
            config.disk_read_only,
            "ocivmm-rootfs".to_string(),
        )
        .map_err(|e| BuilderError::VirtioDevice(e.to_string()))?,
    ));
    let net: Arc<Mutex<dyn VirtioDevice>> = Arc::new(Mutex::new(
        VirtioNet::new(
            "ocivmm-net".to_string(),
            config.net_mac,
            config.passt_socket,
        )
        .map_err(|e| BuilderError::VirtioDevice(e.to_string()))?,
    ));

    let mut event_manager =
        EventManager::new().map_err(|e| BuilderError::EventManager(format!("{e:?}")))?;
    let mut mmio_devices: Vec<MmioDevice> = Vec::new();
    let mut next_bar_addr = VIRTIO_PCI_BAR_REGION_START;

    // Slot 0 is the PCI root bridge itself; devices start at slot 1.
    for (next_slot, device) in (1u8..).zip([block, net]) {
        let queues = device.lock().expect("poisoned").queues().len();
        let msix_num = u16::try_from(queues + 1).expect("queue count fits in u16");
        let msix_vectors = Arc::new(
            Vm::create_msix_group(vm.clone(), msix_num)
                .map_err(|e| BuilderError::VirtioDevice(format!("{e:?}")))?,
        );
        let sbdf = PciSBDF::new(0, 0, next_slot, 0);

        let mut virtio_pci = VirtioPciDevice::new(
            format!("{:?}", device.lock().expect("poisoned").device_type()),
            guest_mem.clone(),
            device.clone(),
            msix_vectors,
            sbdf,
        )
        .map_err(|e| BuilderError::VirtioDevice(format!("{e:?}")))?;
        virtio_pci.allocate_bars(next_bar_addr);
        virtio_pci
            .register_notification_ioevent(vm.fd())
            .map_err(|e| BuilderError::Kvm(e.to_string()))?;

        mmio_devices.push((
            next_bar_addr,
            next_bar_addr + CAPABILITY_BAR_SIZE,
            Arc::new(Mutex::new(virtio_pci)) as Arc<Mutex<dyn PciDevice>>,
        ));
        next_bar_addr += CAPABILITY_BAR_SIZE;

        pci_bus
            .lock()
            .expect("poisoned")
            .add_device(sbdf.device(), mmio_devices.last().unwrap().2.clone())
            .map_err(|e| BuilderError::VirtioDevice(e.to_string()))?;

        // `VirtioDevice: MutEventSubscriber` (trait upcasting): the
        // event manager drives queue-notification and passt-socket
        // readiness for every device uniformly.
        event_manager.add_subscriber(device);
    }

    // --- vCPUs ---------------------------------------------------------
    let should_stop = Arc::new(AtomicBool::new(false));
    for index in 0..config.vcpu_count {
        let mut vcpu =
            Vcpu::new(vm.fd(), index).map_err(|e| BuilderError::Vcpu(format!("{e:?}")))?;
        vcpu.configure(&kvm, &guest_mem, entry_point, config.vcpu_count)
            .map_err(|e| BuilderError::Vcpu(format!("{e:?}")))?;
        let serial = serial.clone();
        let i8042 = i8042.clone();
        let pci_config_io = pci_config_io.clone();
        let mmio_devices = mmio_devices.clone();
        let should_stop = should_stop.clone();
        // Detached: process::exit() below (on shutdown) or inside
        // run_vcpu (on a genuine failure) tears every thread down —
        // there is nothing to join, this VMM only ever runs once.
        std::thread::spawn(move || {
            run_vcpu(
                vcpu,
                index,
                serial,
                i8042,
                pci_config_io,
                mmio_devices,
                should_stop,
            )
        });
    }

    // The shutdown watcher makes the guest's i8042 reset pulse just
    // another EventManager subscriber, so the whole EventManager —
    // devices and shutdown detection alike — runs on this one thread
    // (`dyn MutEventSubscriber` has no `Send` bound; see
    // `ShutdownWatcher`'s own docs for why that rules out a second
    // thread for this).
    event_manager.add_subscriber(Arc::new(Mutex::new(ShutdownWatcher {
        reset_evt,
        should_stop: should_stop.clone(),
    })));
    while !should_stop.load(Ordering::Relaxed) {
        if let Err(err) = event_manager.run_with_timeout(200) {
            error!("event manager loop error: {err:?}");
        }
    }
    debug!("guest requested shutdown (i8042 reset)");
    std::process::exit(0);
}

/// One vCPU's own `KVM_RUN` loop: dispatches PIO/MMIO exits to the
/// legacy and PCI devices, and otherwise just keeps running (`Hlt` is
/// ordinary "wait for the next interrupt" and needs no handling — KVM
/// itself blocks the thread there). A genuinely abnormal exit
/// (`Shutdown`/`InternalError` — a triple fault, effectively) ends the
/// whole process, matching the "one workload, then exit" contract.
#[allow(clippy::too_many_arguments)]
fn run_vcpu(
    mut vcpu: Vcpu,
    index: u8,
    serial: Arc<Mutex<SerialDevice>>,
    i8042: Arc<Mutex<I8042Device>>,
    pci_config_io: Arc<Mutex<PciConfigIo>>,
    mmio_devices: Vec<MmioDevice>,
    should_stop: Arc<AtomicBool>,
) {
    while !should_stop.load(Ordering::Relaxed) {
        match vcpu.kvm_run() {
            Ok(VcpuExit::IoIn(port, data)) => {
                dispatch_io_in(port, data, &serial, &i8042, &pci_config_io)
            }
            Ok(VcpuExit::IoOut(port, data)) => {
                dispatch_io_out(port, data, &serial, &i8042, &pci_config_io)
            }
            Ok(VcpuExit::MmioRead(addr, data)) => dispatch_mmio_read(addr, data, &mmio_devices),
            Ok(VcpuExit::MmioWrite(addr, data)) => dispatch_mmio_write(addr, data, &mmio_devices),
            Ok(VcpuExit::Hlt) => {}
            Ok(VcpuExit::Shutdown) => {
                error!("vcpu {index}: guest triple fault (Shutdown exit)");
                std::process::exit(1);
            }
            Ok(VcpuExit::InternalError) => {
                error!("vcpu {index}: KVM internal error");
                std::process::exit(1);
            }
            Ok(other) => warn!("vcpu {index}: unhandled exit: {other:?}"),
            Err(e) if e.errno() == libc::EINTR => {}
            Err(e) => {
                error!("vcpu {index}: KVM_RUN failed: {e}");
                std::process::exit(1);
            }
        }
    }
}

fn dispatch_io_in(
    port: u16,
    data: &mut [u8],
    serial: &Arc<Mutex<SerialDevice>>,
    i8042: &Arc<Mutex<I8042Device>>,
    pci_config_io: &Arc<Mutex<PciConfigIo>>,
) {
    match port {
        0x3f8..=0x3ff => serial
            .lock()
            .expect("poisoned")
            .bus_read(u64::from(port - 0x3f8), data),
        0x60..=0x64 => i8042
            .lock()
            .expect("poisoned")
            .bus_read(u64::from(port - 0x60), data),
        PCI_CONFIG_IO_PORT..=PCI_CONFIG_IO_PORT_END => pci_config_io
            .lock()
            .expect("poisoned")
            .read(0, u64::from(port - PCI_CONFIG_IO_PORT), data),
        _ => data.iter_mut().for_each(|b| *b = 0xff),
    }
}

fn dispatch_io_out(
    port: u16,
    data: &[u8],
    serial: &Arc<Mutex<SerialDevice>>,
    i8042: &Arc<Mutex<I8042Device>>,
    pci_config_io: &Arc<Mutex<PciConfigIo>>,
) {
    match port {
        0x3f8..=0x3ff => serial
            .lock()
            .expect("poisoned")
            .bus_write(u64::from(port - 0x3f8), data),
        0x60..=0x64 => i8042
            .lock()
            .expect("poisoned")
            .bus_write(u64::from(port - 0x60), data),
        PCI_CONFIG_IO_PORT..=PCI_CONFIG_IO_PORT_END => {
            let _ = pci_config_io.lock().expect("poisoned").write(
                0,
                u64::from(port - PCI_CONFIG_IO_PORT),
                data,
            );
        }
        _ => {}
    }
}

fn dispatch_mmio_read(addr: u64, data: &mut [u8], devices: &[MmioDevice]) {
    for (start, end, device) in devices {
        if (*start..*end).contains(&addr) {
            device
                .lock()
                .expect("poisoned")
                .read_bar(*start, addr - start, data);
            return;
        }
    }
}

fn dispatch_mmio_write(addr: u64, data: &[u8], devices: &[MmioDevice]) {
    for (start, end, device) in devices {
        if (*start..*end).contains(&addr) {
            let _ = device
                .lock()
                .expect("poisoned")
                .write_bar(*start, addr - start, data);
            return;
        }
    }
}
