// SPDX-License-Identifier: Apache-2.0

//! virtio-mmio: this backend's virtio transport, standing in for the
//! KVM/x86_64 backend's virtio-**pci** (`crate::virtio::transport::
//! pci`) -- see `docs/design/0249`'s own note on why: real CentOS/
//! Ubuntu aarch64 kernel packages build `CONFIG_VIRTIO_MMIO` (unlike
//! their x86_64 builds, which is why PCI was chosen there), so there
//! is no PCI-parity reason to reuse `pci/{bus,configuration,msix}.rs`
//! at all here.
//!
//! This module implements the register file directly against
//! `crate::virtio::queue::Queue` and `crate::virtio::block::{disk,
//! request}` -- the transport- and hypervisor-agnostic parts of the
//! existing virtio-blk implementation the KVM/x86_64 backend's own
//! `virtio::block::device::VirtioBlock` also builds on. It does *not*
//! reuse `virtio::device::VirtioDevice` or `virtio::transport::
//! VirtioInterrupt`: those are shaped around `event_manager`'s
//! `MutEventSubscriber` and `vmm_sys_util::eventfd::EventFd`, an
//! epoll-driven event loop this backend has no equivalent of at all
//! (every `hvf` device -- `pl011`, and this one -- is instead
//! dispatched synchronously, directly out of the vCPU exit loop, the
//! same way `hvf::mmio::emulate` already works). [`MmioVirtioDevice`]
//! is this module's own, much smaller device trait for that model.
//!
//! Register layout: Virtio 1.x spec section "MMIO Device Register
//! Layout" -- a completely different register set from virtio-pci's
//! common configuration structure (`transport::pci::common_config`),
//! though the *protocol* semantics (feature negotiation, the device
//! status state machine, `QueueNotify`/interrupt handling) are the
//! same virtio spec both transports implement.

use crate::hvf::gic;
use crate::hvf::mmio::MmioDevice;
use crate::mem::GuestMemoryMmap;
use crate::virtio::queue::Queue;

mod reg {
    pub const MAGIC_VALUE: u64 = 0x000;
    pub const VERSION: u64 = 0x004;
    pub const DEVICE_ID: u64 = 0x008;
    pub const VENDOR_ID: u64 = 0x00c;
    pub const DEVICE_FEATURES: u64 = 0x010;
    pub const DEVICE_FEATURES_SEL: u64 = 0x014;
    pub const DRIVER_FEATURES: u64 = 0x020;
    pub const DRIVER_FEATURES_SEL: u64 = 0x024;
    pub const QUEUE_SEL: u64 = 0x030;
    pub const QUEUE_NUM_MAX: u64 = 0x034;
    pub const QUEUE_NUM: u64 = 0x038;
    pub const QUEUE_READY: u64 = 0x044;
    pub const QUEUE_NOTIFY: u64 = 0x050;
    pub const INTERRUPT_STATUS: u64 = 0x060;
    pub const INTERRUPT_ACK: u64 = 0x064;
    pub const STATUS: u64 = 0x070;
    pub const QUEUE_DESC_LOW: u64 = 0x080;
    pub const QUEUE_DESC_HIGH: u64 = 0x084;
    pub const QUEUE_DRIVER_LOW: u64 = 0x090;
    pub const QUEUE_DRIVER_HIGH: u64 = 0x094;
    pub const QUEUE_DEVICE_LOW: u64 = 0x0a0;
    pub const QUEUE_DEVICE_HIGH: u64 = 0x0a4;
    pub const CONFIG_GENERATION: u64 = 0x0fc;
    pub const CONFIG: u64 = 0x100;
}

/// `MagicValue`: the ASCII bytes `"virt"`, little-endian.
const MAGIC_VALUE: u32 = 0x7472_6976;
/// `Version`: `2` selects the modern (non-legacy) virtio-mmio
/// interface -- the only one this module implements, matching the
/// PCI transport's own "modern, not transitional" choice.
const VERSION: u32 = 2;
/// An arbitrary, fixed `VendorId` -- the spec places no requirement on
/// its value beyond existing; `"OCIV"`, little-endian, identifies this
/// project's own devices in a register dump without meaning anything
/// to the driver.
const VENDOR_ID: u32 = 0x5649_434f;

/// The device status bits this module actually branches on. The
/// virtio spec's own state machine (`ACKNOWLEDGE` -> `DRIVER` ->
/// `FEATURES_OK` -> `DRIVER_OK`) isn't enforced strictly here (a real,
/// well-behaved driver -- the only kind this project's pet VMs run --
/// always follows it correctly regardless); only the two transitions
/// that matter operationally are handled: `DRIVER_OK` (first set:
/// initialize every ready queue) and a write of `0` (full reset).
const STATUS_DRIVER_OK: u32 = 0x04;
const STATUS_DEVICE_NEEDS_RESET: u32 = 0x40;

/// `InterruptStatus`/`InterruptACK` bit 0: a used buffer notification.
const INT_VRING: u32 = 1 << 0;

/// This module's own device trait: a synchronous, non-`event_manager`
/// counterpart to `virtio::device::VirtioDevice` -- see the module
/// docs on why they aren't shared.
pub trait MmioVirtioDevice {
    /// The virtio device ID (`virtio::generated::virtio_ids`).
    fn device_id(&self) -> u32;
    /// The features this device offers (`DeviceFeatures`).
    fn avail_features(&self) -> u64;
    /// The number of virtqueues this device has.
    fn num_queues(&self) -> usize;
    /// The maximum size of virtqueue `index`.
    fn queue_max_size(&self, index: usize) -> u16;
    /// Reads the device-specific configuration space at `offset`.
    fn read_config(&self, offset: u64, data: &mut [u8]);
    /// Writes the device-specific configuration space at `offset`.
    fn write_config(&mut self, offset: u64, data: &[u8]);
    /// Processes every available descriptor chain currently in
    /// `queue` (a `QueueNotify` for virtqueue `index`). Returns `true`
    /// if the used ring changed and a used-buffer interrupt should be
    /// raised.
    fn process_queue(&mut self, index: usize, queue: &mut Queue, mem: &GuestMemoryMmap) -> bool;
}

/// The virtio-mmio transport: the register file plus the virtqueues
/// it manages on `device`'s behalf, wired to a GIC SPI for interrupt
/// delivery.
#[derive(Debug)]
pub struct VirtioMmioTransport<D> {
    device: D,
    mem: GuestMemoryMmap,
    queues: Vec<Queue>,
    queue_sel: usize,
    device_features_sel: u32,
    driver_features: u64,
    driver_features_sel: u32,
    status: u32,
    interrupt_status: u32,
    /// The GIC INTID (already offset by `layout::GIC_SPI_BASE`) this
    /// device's interrupt line is wired to.
    spi: u32,
}

impl<D: MmioVirtioDevice> VirtioMmioTransport<D> {
    /// Wraps `device` in a virtio-mmio transport, driving guest memory
    /// `mem` and asserting SPI `spi` (a full GIC INTID) for interrupts.
    pub fn new(device: D, mem: GuestMemoryMmap, spi: u32) -> Self {
        let queues = (0..device.num_queues())
            .map(|i| Queue::new(device.queue_max_size(i)))
            .collect();
        VirtioMmioTransport {
            device,
            mem,
            queues,
            queue_sel: 0,
            device_features_sel: 0,
            driver_features: 0,
            driver_features_sel: 0,
            status: 0,
            interrupt_status: 0,
            spi,
        }
    }

    fn selected_queue(&mut self) -> Option<&mut Queue> {
        self.queues.get_mut(self.queue_sel)
    }

    fn set_status(&mut self, value: u32) {
        let was_driver_ok = (self.status & STATUS_DRIVER_OK) != 0;
        self.status = value;

        if value == 0 {
            // "Writing 0 ... resets the device": drop every queue back
            // to not-ready, matching the PCI transport's own reset
            // path (transport::pci::common_config's `INIT` state).
            for queue in &mut self.queues {
                queue.reset();
            }
            self.interrupt_status = 0;
            let _ = gic::set_spi(self.spi, false);
            return;
        }

        let now_driver_ok = (value & STATUS_DRIVER_OK) != 0;
        if now_driver_ok && !was_driver_ok {
            // The virtio spec's device initialization sequence: once
            // DRIVER_OK is set for the first time, every queue the
            // driver marked ready has its final desc/avail/used
            // addresses and is ready to run.
            for queue in self.queues.iter_mut().filter(|q| q.ready) {
                if let Err(err) = queue.initialize(&self.mem) {
                    tracing::error!("hvf::virtio_mmio: queue initialize failed: {err}");
                    self.status |= STATUS_DEVICE_NEEDS_RESET;
                }
            }
        }
    }

    fn queue_notify(&mut self, index: u32) {
        let Ok(index) = usize::try_from(index) else {
            return;
        };
        let Some(queue) = self.queues.get_mut(index) else {
            return;
        };
        if !queue.ready {
            return;
        }

        if self.device.process_queue(index, queue, &self.mem) {
            self.interrupt_status |= INT_VRING;
            if let Err(err) = gic::set_spi(self.spi, true) {
                tracing::error!("hvf::virtio_mmio: hv_gic_set_spi failed: {err}");
            }
        }
    }

    fn interrupt_ack(&mut self, value: u32) {
        self.interrupt_status &= !value;
        if self.interrupt_status == 0 {
            let _ = gic::set_spi(self.spi, false);
        }
    }

    fn read_reg(&mut self, offset: u64) -> u32 {
        match offset {
            reg::MAGIC_VALUE => MAGIC_VALUE,
            reg::VERSION => VERSION,
            reg::DEVICE_ID => self.device.device_id(),
            reg::VENDOR_ID => VENDOR_ID,
            reg::DEVICE_FEATURES => {
                let features = self.device.avail_features();
                match self.device_features_sel {
                    0 => features as u32,
                    1 => (features >> 32) as u32,
                    _ => 0,
                }
            }
            reg::QUEUE_NUM_MAX => self
                .queues
                .get(self.queue_sel)
                .map_or(0, |q| u32::from(q.max_size)),
            reg::QUEUE_READY => u32::from(self.queues.get(self.queue_sel).is_some_and(|q| q.ready)),
            reg::INTERRUPT_STATUS => self.interrupt_status,
            reg::STATUS => self.status,
            reg::CONFIG_GENERATION => 0,
            _ => 0,
        }
    }

    fn write_reg(&mut self, offset: u64, value: u32) {
        match offset {
            reg::DEVICE_FEATURES_SEL => self.device_features_sel = value,
            reg::DRIVER_FEATURES => match self.driver_features_sel {
                0 => {
                    self.driver_features = (self.driver_features & !0xffff_ffff) | u64::from(value)
                }
                1 => {
                    self.driver_features =
                        (self.driver_features & 0xffff_ffff) | (u64::from(value) << 32)
                }
                _ => {}
            },
            reg::DRIVER_FEATURES_SEL => self.driver_features_sel = value,
            reg::QUEUE_SEL => {
                if let Ok(sel) = usize::try_from(value) {
                    self.queue_sel = sel;
                }
            }
            reg::QUEUE_NUM => {
                if let (Ok(size), Some(queue)) = (u16::try_from(value), self.selected_queue()) {
                    queue.size = size;
                }
            }
            reg::QUEUE_READY => {
                let ready = value != 0;
                if let Some(queue) = self.selected_queue() {
                    queue.ready = ready;
                }
            }
            reg::QUEUE_NOTIFY => self.queue_notify(value),
            reg::INTERRUPT_ACK => self.interrupt_ack(value),
            reg::STATUS => self.set_status(value),
            reg::QUEUE_DESC_LOW => {
                if let Some(queue) = self.selected_queue() {
                    queue.desc_table_address.0 =
                        (queue.desc_table_address.0 & !0xffff_ffff) | u64::from(value);
                }
            }
            reg::QUEUE_DESC_HIGH => {
                if let Some(queue) = self.selected_queue() {
                    queue.desc_table_address.0 =
                        (queue.desc_table_address.0 & 0xffff_ffff) | (u64::from(value) << 32);
                }
            }
            reg::QUEUE_DRIVER_LOW => {
                if let Some(queue) = self.selected_queue() {
                    queue.avail_ring_address.0 =
                        (queue.avail_ring_address.0 & !0xffff_ffff) | u64::from(value);
                }
            }
            reg::QUEUE_DRIVER_HIGH => {
                if let Some(queue) = self.selected_queue() {
                    queue.avail_ring_address.0 =
                        (queue.avail_ring_address.0 & 0xffff_ffff) | (u64::from(value) << 32);
                }
            }
            reg::QUEUE_DEVICE_LOW => {
                if let Some(queue) = self.selected_queue() {
                    queue.used_ring_address.0 =
                        (queue.used_ring_address.0 & !0xffff_ffff) | u64::from(value);
                }
            }
            reg::QUEUE_DEVICE_HIGH => {
                if let Some(queue) = self.selected_queue() {
                    queue.used_ring_address.0 =
                        (queue.used_ring_address.0 & 0xffff_ffff) | (u64::from(value) << 32);
                }
            }
            _ => {}
        }
    }
}

impl<D: MmioVirtioDevice> MmioDevice for VirtioMmioTransport<D> {
    fn read(&mut self, offset: u64, data: &mut [u8]) {
        if offset >= reg::CONFIG {
            self.device.read_config(offset - reg::CONFIG, data);
            return;
        }
        let value = self.read_reg(offset & !0x3);
        let bytes = value.to_le_bytes();
        let len = data.len().min(4);
        data[..len].copy_from_slice(&bytes[..len]);
    }

    fn write(&mut self, offset: u64, data: &[u8]) {
        if offset >= reg::CONFIG {
            self.device.write_config(offset - reg::CONFIG, data);
            return;
        }
        let mut bytes = [0u8; 4];
        let len = data.len().min(4);
        bytes[..len].copy_from_slice(&data[..len]);
        self.write_reg(offset & !0x3, u32::from_le_bytes(bytes));
    }
}
