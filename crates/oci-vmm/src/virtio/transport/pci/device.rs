// Copyright 2025 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright 2018 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE-BSD-3-Clause file.
//
// Copyright © 2019 Intel Corporation
//
// SPDX-License-Identifier: Apache-2.0 AND BSD-3-Clause

// Ported into oci-vmm from Firecracker (src/vmm/src/devices/virtio/transport/pci/device.rs), trimmed of metrics/snapshot/MMIO.

//! [`VirtioPciDevice`]: the PCI transport wrapping a [`VirtioDevice`],
//! bridging PCI configuration space and BAR accesses to the device, with
//! MSI-X interrupts and ioeventfd-backed queue notifications.

// The virtio-pci capability structures are `#[repr(C, packed)]` PODs exposed
// to the guest byte-by-byte; `unsafe impl ByteValued` is required (and sound:
// all fields are plain integers, any bit pattern is valid, no padding).
#![allow(unsafe_code)]

use std::cmp;
use std::fmt::{Debug, Formatter};
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, Ordering};
use std::sync::{Arc, Barrier, Mutex};

use kvm_ioctls::{IoEventAddress, NoDatamatch, VmFd};
use tracing::{debug, error, warn};
use vm_memory::{ByteValued, Le32};
use vmm_sys_util::errno;
use vmm_sys_util::eventfd::EventFd;

use crate::mem::GuestMemoryMmap;
use crate::pci::configuration::{
    BAR0_REG_IDX, BarPrefetchable, Bars, NUM_BAR_REGS, PciCapability, PciConfiguration,
};
use crate::pci::msix::{MsixCap, MsixConfig};
use crate::pci::{
    PciCapabilityId, PciClassCode, PciDevice, PciMassStorageSubclass, PciNetworkControllerSubclass,
    PciSBDF,
};
use crate::virtio::device::{VirtioDevice, VirtioDeviceType};
use crate::virtio::queue::Queue;
use crate::virtio::transport::pci::common_config::{
    VirtioPciCommonConfig, VirtioPciCommonConfigState,
};
use crate::virtio::transport::pci::device_status::*;
use crate::virtio::transport::{VirtioInterrupt, VirtioInterruptType};
use crate::vstate::interrupts::{InterruptError, MsixVectorGroup};

/// Vector value used to disable MSI for a queue.
pub const VIRTQ_MSI_NO_VECTOR: u16 = 0xffff;

/// BAR index we are using for VirtIO configuration
const VIRTIO_BAR_INDEX: u8 = 0;

#[allow(dead_code)]
enum PciCapabilityType {
    Common = 1,
    Notify = 2,
    Isr = 3,
    Device = 4,
    Pci = 5,
    SharedMemory = 8,
}

// This offset represents the 2 bytes omitted from the VirtioPciCap structure
// as they are already handled through add_capability(). These 2 bytes are the
// fields cap_vndr (1 byte) and cap_next (1 byte) defined in the virtio spec.
const VIRTIO_PCI_CAP_OFFSET: u16 = 2;

#[repr(C, packed)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct VirtioPciCap {
    cap_len: u8,      // Generic PCI field: capability length
    cfg_type: u8,     // Identifies the structure.
    pci_bar: u8,      // Where to find it.
    id: u8,           // Multiple capabilities of the same type.
    padding: [u8; 2], // Pad to full dword.
    offset: Le32,     // Offset within bar.
    length: Le32,     // Length of the structure, in bytes.
}

// SAFETY: All members are simple numbers and any value is valid.
unsafe impl ByteValued for VirtioPciCap {}

impl PciCapability for VirtioPciCap {
    fn bytes(&self) -> &[u8] {
        self.as_slice()
    }

    fn id(&self) -> PciCapabilityId {
        PciCapabilityId::VendorSpecific
    }
}

const VIRTIO_PCI_CAP_LEN_OFFSET: u8 = 2;

impl VirtioPciCap {
    pub fn new(cfg_type: PciCapabilityType, offset: u32, length: u32) -> Self {
        VirtioPciCap {
            cap_len: u8::try_from(size_of::<VirtioPciCap>()).unwrap() + VIRTIO_PCI_CAP_LEN_OFFSET,
            cfg_type: cfg_type as u8,
            pci_bar: VIRTIO_BAR_INDEX,
            id: 0,
            padding: [0; 2],
            offset: Le32::from(offset),
            length: Le32::from(length),
        }
    }
}

#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
struct VirtioPciNotifyCap {
    cap: VirtioPciCap,
    notify_off_multiplier: Le32,
}
// SAFETY: All members are simple numbers and any value is valid.
unsafe impl ByteValued for VirtioPciNotifyCap {}

impl PciCapability for VirtioPciNotifyCap {
    fn bytes(&self) -> &[u8] {
        self.as_slice()
    }

    fn id(&self) -> PciCapabilityId {
        PciCapabilityId::VendorSpecific
    }
}

impl VirtioPciNotifyCap {
    pub fn new(cfg_type: PciCapabilityType, offset: u32, length: u32, multiplier: Le32) -> Self {
        VirtioPciNotifyCap {
            cap: VirtioPciCap {
                cap_len: u8::try_from(size_of::<VirtioPciNotifyCap>()).unwrap()
                    + VIRTIO_PCI_CAP_LEN_OFFSET,
                cfg_type: cfg_type as u8,
                pci_bar: VIRTIO_BAR_INDEX,
                id: 0,
                padding: [0; 2],
                offset: Le32::from(offset),
                length: Le32::from(length),
            },
            notify_off_multiplier: multiplier,
        }
    }
}

#[repr(C, packed)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct VirtioPciCfgCap {
    cap: VirtioPciCap,
    pci_cfg_data: [u8; 4],
}
// SAFETY: All members are simple numbers and any value is valid.
unsafe impl ByteValued for VirtioPciCfgCap {}

impl PciCapability for VirtioPciCfgCap {
    fn bytes(&self) -> &[u8] {
        self.as_slice()
    }

    fn id(&self) -> PciCapabilityId {
        PciCapabilityId::VendorSpecific
    }
}

impl VirtioPciCfgCap {
    fn new() -> Self {
        VirtioPciCfgCap {
            cap: VirtioPciCap {
                cap_len: u8::try_from(size_of::<Self>()).unwrap() + VIRTIO_PCI_CAP_LEN_OFFSET,
                cfg_type: PciCapabilityType::Pci as u8,
                pci_bar: VIRTIO_BAR_INDEX,
                id: 0,
                padding: [0; 2],
                offset: Le32::from(0),
                length: Le32::from(0),
            },
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct VirtioPciCfgCapInfo {
    offset: u16,
    cap: VirtioPciCfgCap,
}

impl VirtioPciCfgCapInfo {
    fn in_range(&self, reg_idx: u16, offset: u8, data_len: usize) -> bool {
        let base = reg_idx * 4;
        let cap_start = self.offset;
        let cap_end = self.offset as usize + self.cap.bytes().len();
        let start = base + u16::from(offset);
        let end = (base + u16::from(offset)) as usize + data_len;
        cap_start <= start && end <= cap_end
    }
}

/// PCI subclass used for VirtIO devices without a more specific class.
#[derive(Debug, Copy, Clone)]
#[repr(u8)]
pub enum PciVirtioSubclass {
    /// Modern, non-transitional VirtIO device.
    NonTransitionalBase = 0xff,
}

// Allocate one bar for the structs pointed to by the capability structures.
// As per the PCI specification, because the same BAR shares MSI-X and non
// MSI-X structures, it is recommended to use 8KiB alignment for all those
// structures.
const COMMON_CONFIG_BAR_OFFSET: u32 = 0x0000;
const COMMON_CONFIG_SIZE: u32 = 56;
const ISR_CONFIG_BAR_OFFSET: u32 = 0x2000;
const ISR_CONFIG_SIZE: u32 = 1;
const DEVICE_CONFIG_BAR_OFFSET: u32 = 0x4000;
const DEVICE_CONFIG_SIZE: u32 = 0x1000;
const NOTIFICATION_BAR_OFFSET: u32 = 0x6000;
const NOTIFICATION_SIZE: u32 = 0x1000;
const MSIX_TABLE_BAR_OFFSET: u32 = 0x8000;
// The size is 256KiB because the table can hold up to 2048 entries, with each
// entry being 128 bits (4 DWORDS).
const MSIX_TABLE_SIZE: u32 = 0x40000;
const MSIX_PBA_BAR_OFFSET: u32 = 0x48000;
// The size is 2KiB because the Pending Bit Array has one bit per vector and it
// can support up to 2048 vectors.
const MSIX_PBA_SIZE: u32 = 0x800;
/// The BAR size must be a power of 2.
pub const CAPABILITY_BAR_SIZE: u64 = 0x80000;

const NOTIFY_OFF_MULTIPLIER: u32 = 4; // A dword per notification address.

const VIRTIO_PCI_VENDOR_ID: u16 = 0x1af4;
const VIRTIO_PCI_DEVICE_ID_BASE: u16 = 0x1040; // Add to device type to get device ID.

/// Errors from creating a [`VirtioPciDevice`].
#[derive(Debug, thiserror::Error)]
pub enum VirtioPciDeviceError {
    /// Error creating MSI configuration.
    #[error("Error creating MSI configuration: {0}")]
    Msi(#[from] InterruptError),
}

/// The PCI transport of a VirtIO device.
pub struct VirtioPciDevice {
    id: String,

    /// The subscriber ID returned by the EventManager
    pub sub_id: Option<event_manager::SubscriberId>,

    /// SBDF assigned to the device
    pub sbdf: PciSBDF,

    // PCI configuration registers.
    configuration: PciConfiguration,
    // BARs region from configuration space handled separately
    bars: Bars,

    // virtio PCI common configuration
    common_config: VirtioPciCommonConfig,

    // Virtio device reference and status
    device: Arc<Mutex<dyn VirtioDevice>>,
    device_activated: Arc<AtomicBool>,

    // PCI interrupts.
    virtio_interrupt: Option<Arc<VirtioInterruptMsix>>,

    // Guest memory
    memory: GuestMemoryMmap,

    // Add a dedicated structure to hold information about the very specific
    // virtio-pci capability VIRTIO_PCI_CAP_PCI_CFG. This is needed to support
    // the legacy/backward compatible mechanism of letting the guest access the
    // other virtio capabilities without mapping the PCI BARs. This can be
    // needed when the guest tries to early access the virtio configuration of
    // a device.
    cap_pci_cfg_info: VirtioPciCfgCapInfo,
    msix_config_cap_offset: u16,
    msix_config: Arc<Mutex<MsixConfig>>,
}

impl Debug for VirtioPciDevice {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        f.debug_struct("VirtioPciDevice")
            .field("id", &self.id)
            .finish()
    }
}

impl VirtioPciDevice {
    fn pci_configuration(device_type: VirtioDeviceType) -> PciConfiguration {
        let pci_device_id = VIRTIO_PCI_DEVICE_ID_BASE + device_type as u16;
        let (class, subclass) = match device_type {
            VirtioDeviceType::Net => (
                PciClassCode::NetworkController,
                PciNetworkControllerSubclass::EthernetController as u8,
            ),
            VirtioDeviceType::Block => (
                PciClassCode::MassStorageController,
                PciMassStorageSubclass::MassStorage as u8,
            ),
            _ => (
                PciClassCode::UnassignedClass,
                PciVirtioSubclass::NonTransitionalBase as u8,
            ),
        };

        PciConfiguration::new_type0(
            VIRTIO_PCI_VENDOR_ID,
            pci_device_id,
            0x1, // For modern virtio-PCI devices
            class,
            subclass,
            VIRTIO_PCI_VENDOR_ID,
            pci_device_id,
        )
    }

    /// Set up the PCI BAR for the VirtIO device and its associated capabilities.
    ///
    /// `virtio_pci_bar_addr` is the guest physical address of the capability
    /// BAR; the builder allocates it from the 64-bit MMIO region (it must be
    /// [`CAPABILITY_BAR_SIZE`]-aligned and [`CAPABILITY_BAR_SIZE`] bytes big).
    ///
    /// See http://docs.oasis-open.org/virtio/virtio/v1.0/cs04/virtio-v1.0-cs04.html#x1-740004
    pub fn allocate_bars(&mut self, virtio_pci_bar_addr: u64) {
        self.bars.set_bar_64(
            VIRTIO_BAR_INDEX,
            virtio_pci_bar_addr,
            CAPABILITY_BAR_SIZE,
            BarPrefetchable::No,
        );
        self.add_pci_capabilities();
    }

    /// Constructs a new PCI transport for the given virtio device.
    pub fn new(
        id: String,
        memory: GuestMemoryMmap,
        device: Arc<Mutex<dyn VirtioDevice>>,
        msix_vectors: Arc<MsixVectorGroup>,
        sbdf: PciSBDF,
    ) -> Result<Self, VirtioPciDeviceError> {
        let num_queues = device.lock().expect("Poisoned lock").queues().len();

        let msix_config = Arc::new(Mutex::new(MsixConfig::new(msix_vectors.clone(), sbdf)));
        let pci_config =
            Self::pci_configuration(device.lock().expect("Poisoned lock").device_type());

        let virtio_common_config = VirtioPciCommonConfig::new(VirtioPciCommonConfigState {
            driver_status: 0,
            config_generation: 0,
            device_feature_select: 0,
            driver_feature_select: 0,
            queue_select: 0,
            msix_config: VIRTQ_MSI_NO_VECTOR,
            msix_queues: vec![VIRTQ_MSI_NO_VECTOR; num_queues],
        });
        let interrupt = Arc::new(VirtioInterruptMsix::new(
            msix_config.clone(),
            virtio_common_config.msix_config.clone(),
            virtio_common_config.msix_queues.clone(),
            msix_vectors,
        ));

        let virtio_pci_device = VirtioPciDevice {
            id,
            sub_id: None,
            sbdf,
            configuration: pci_config,
            common_config: virtio_common_config,
            device,
            device_activated: Arc::new(AtomicBool::new(false)),
            virtio_interrupt: Some(interrupt),
            memory,
            cap_pci_cfg_info: VirtioPciCfgCapInfo::default(),
            bars: Bars::default(),
            msix_config,
            msix_config_cap_offset: 0,
        };

        Ok(virtio_pci_device)
    }

    fn is_driver_ready(&self) -> bool {
        let ready_bits = ACKNOWLEDGE | DRIVER | DRIVER_OK | FEATURES_OK;
        self.common_config.driver_status == ready_bits
    }

    /// Determines if the driver has requested the device (re)init / reset itself
    fn is_driver_init(&self) -> bool {
        self.common_config.driver_status == INIT
    }

    /// Guest physical address of the VirtIO capability BAR.
    pub fn config_bar_addr(&self) -> u64 {
        self.bars.get_bar_addr_64(VIRTIO_BAR_INDEX)
    }

    fn add_pci_capabilities(&mut self) {
        // Add pointers to the different configuration structures from the PCI capabilities.
        let common_cap = VirtioPciCap::new(
            PciCapabilityType::Common,
            COMMON_CONFIG_BAR_OFFSET,
            COMMON_CONFIG_SIZE,
        );
        self.configuration.add_capability(&common_cap);

        let isr_cap = VirtioPciCap::new(
            PciCapabilityType::Isr,
            ISR_CONFIG_BAR_OFFSET,
            ISR_CONFIG_SIZE,
        );
        self.configuration.add_capability(&isr_cap);

        let device_cap = VirtioPciCap::new(
            PciCapabilityType::Device,
            DEVICE_CONFIG_BAR_OFFSET,
            DEVICE_CONFIG_SIZE,
        );
        self.configuration.add_capability(&device_cap);

        let notify_cap = VirtioPciNotifyCap::new(
            PciCapabilityType::Notify,
            NOTIFICATION_BAR_OFFSET,
            NOTIFICATION_SIZE,
            Le32::from(NOTIFY_OFF_MULTIPLIER),
        );
        self.configuration.add_capability(&notify_cap);

        let configuration_cap = VirtioPciCfgCap::new();
        self.cap_pci_cfg_info.offset =
            u16::from(self.configuration.add_capability(&configuration_cap))
                + VIRTIO_PCI_CAP_OFFSET;
        self.cap_pci_cfg_info.cap = configuration_cap;

        if let Some(interrupt) = &self.virtio_interrupt {
            let msix_cap = MsixCap::new(
                VIRTIO_BAR_INDEX,
                interrupt
                    .msix_config
                    .lock()
                    .expect("Poisoned lock")
                    .vectors
                    .num_vectors(),
                MSIX_TABLE_BAR_OFFSET,
                VIRTIO_BAR_INDEX,
                MSIX_PBA_BAR_OFFSET,
            );
            // The whole Configuration region is 4K, so u16 can address it all
            #[allow(clippy::cast_possible_truncation)]
            let offset = self.configuration.add_capability(&msix_cap) as u16;
            self.msix_config_cap_offset = offset;
        }
    }

    fn read_cap_pci_cfg(&mut self, offset: usize, mut data: &mut [u8]) {
        let cap_slice = self.cap_pci_cfg_info.cap.as_slice();
        let data_len = data.len();
        let cap_len = cap_slice.len();
        if offset + data_len > cap_len {
            error!("Failed to read cap_pci_cfg from config space");
            return;
        }

        if offset < size_of::<VirtioPciCap>() {
            if let Some(end) = offset.checked_add(data_len) {
                // This write can't fail, offset and end are checked against config_len.
                data.write_all(&cap_slice[offset..cmp::min(end, cap_len)])
                    .unwrap();
            }
        } else {
            let bar_offset: u32 = self.cap_pci_cfg_info.cap.cap.offset.into();
            let len = u32::from(self.cap_pci_cfg_info.cap.cap.length) as usize;
            // BAR reads expect that the buffer has the exact size of the field that
            // offset is pointing to. So, do some check that the `length` has a meaningful value
            // and only use the part of the buffer we actually need.
            if len <= 4 {
                self.read_bar(0, bar_offset as u64, &mut data[..len]);
            }
        }
    }

    fn write_cap_pci_cfg(&mut self, offset: usize, data: &[u8]) -> Option<Arc<Barrier>> {
        let cap_slice = self.cap_pci_cfg_info.cap.as_mut_slice();
        let data_len = data.len();
        let cap_len = cap_slice.len();
        if offset + data_len > cap_len {
            error!("Failed to write cap_pci_cfg to config space");
            return None;
        }

        if offset < size_of::<VirtioPciCap>() {
            let (_, right) = cap_slice.split_at_mut(offset);
            right[..data_len].copy_from_slice(data);
            None
        } else {
            let bar_offset: u32 = self.cap_pci_cfg_info.cap.cap.offset.into();
            let len = u32::from(self.cap_pci_cfg_info.cap.cap.length) as usize;
            // BAR writes expect that the buffer has the exact size of the field that
            // offset is pointing to. So, do some check that the `length` has a meaningful value
            // and only use the part of the buffer we actually need.
            if len <= 4 {
                let len = len.min(data.len());
                self.write_bar(0, bar_offset as u64, &data[..len])
            } else {
                None
            }
        }
    }

    /// Returns the wrapped [`VirtioDevice`].
    pub fn virtio_device(&self) -> Arc<Mutex<dyn VirtioDevice>> {
        self.device.clone()
    }

    fn needs_activation(&self) -> bool {
        !self.device_activated.load(Ordering::SeqCst) && self.is_driver_ready()
    }

    /// Register the IoEvent notification for a VirtIO device.
    ///
    /// Registers one ioeventfd per queue with KVM, at the queue's notify
    /// address inside the notification region of the capability BAR, so that
    /// guest writes to the notify addresses kick the queue eventfds directly.
    pub fn register_notification_ioevent(&self, vm_fd: &VmFd) -> Result<(), errno::Error> {
        let bar_addr = self.config_bar_addr();
        for (i, queue_evt) in self
            .device
            .lock()
            .expect("Poisoned lock")
            .queue_events()
            .iter()
            .enumerate()
        {
            let notify_base = bar_addr + u64::from(NOTIFICATION_BAR_OFFSET);
            let io_addr =
                IoEventAddress::Mmio(notify_base + i as u64 * u64::from(NOTIFY_OFF_MULTIPLIER));
            vm_fd.register_ioevent(queue_evt, &io_addr, NoDatamatch)?;
        }
        Ok(())
    }
}

/// MSI-X backed implementation of [`VirtioInterrupt`].
pub struct VirtioInterruptMsix {
    msix_config: Arc<Mutex<MsixConfig>>,
    config_vector: Arc<AtomicU16>,
    queues_vectors: Arc<Mutex<Vec<u16>>>,
    vectors: Arc<MsixVectorGroup>,
}

impl Debug for VirtioInterruptMsix {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VirtioInterruptMsix")
            .field("msix_config", &self.msix_config)
            .field("config_vector", &self.config_vector)
            .field("queues_vectors", &self.queues_vectors)
            .finish()
    }
}

impl VirtioInterruptMsix {
    /// Create a new [`VirtioInterruptMsix`].
    pub fn new(
        msix_config: Arc<Mutex<MsixConfig>>,
        config_vector: Arc<AtomicU16>,
        queues_vectors: Arc<Mutex<Vec<u16>>>,
        vectors: Arc<MsixVectorGroup>,
    ) -> Self {
        VirtioInterruptMsix {
            msix_config,
            config_vector,
            queues_vectors,
            vectors,
        }
    }
}

impl VirtioInterrupt for VirtioInterruptMsix {
    fn trigger(&self, int_type: VirtioInterruptType) -> Result<(), InterruptError> {
        let vector = match int_type {
            VirtioInterruptType::Config => self.config_vector.load(Ordering::Acquire),
            VirtioInterruptType::Queue(queue_index) => *self
                .queues_vectors
                .lock()
                .unwrap()
                .get(queue_index as usize)
                .ok_or(InterruptError::InvalidVectorIndex(queue_index as usize))?,
        };

        if vector == VIRTQ_MSI_NO_VECTOR {
            return Ok(());
        }

        let config = &mut self.msix_config.lock().unwrap();
        let entry = &config.table_entries[vector as usize];
        // In case the vector control register associated with the entry
        // has its first bit set, this means the vector is masked and the
        // device should not inject the interrupt.
        // Instead, the Pending Bit Array table is updated to reflect there
        // is a pending interrupt for this specific vector.
        if config.masked || entry.masked() {
            config.set_pba_bit(vector, false);
            return Ok(());
        }

        self.vectors.trigger(vector as usize)
    }

    fn notifier(&self, int_type: VirtioInterruptType) -> Option<&EventFd> {
        let vector = match int_type {
            VirtioInterruptType::Config => self.config_vector.load(Ordering::Acquire),
            VirtioInterruptType::Queue(queue_index) => *self
                .queues_vectors
                .lock()
                .unwrap()
                .get(queue_index as usize)?,
        };

        self.vectors.notifier(vector as usize)
    }

    fn status(&self) -> Arc<AtomicU32> {
        Arc::new(AtomicU32::new(0))
    }
}

impl PciDevice for VirtioPciDevice {
    fn write_config_register(
        &mut self,
        reg_idx: u16,
        offset: u8,
        data: &[u8],
    ) -> Option<Arc<Barrier>> {
        let in_bars = BAR0_REG_IDX <= reg_idx && reg_idx < BAR0_REG_IDX + u16::from(NUM_BAR_REGS);
        let in_msix_cap_header = reg_idx * 4 == self.msix_config_cap_offset;
        let in_pci_cfg = self.cap_pci_cfg_info.in_range(reg_idx, offset, data.len());
        if in_bars {
            // reg_idx is in [BAR0_REG_IDX, BAR0_REG_IDX+NUM_BAR_REGS), so the difference is 0..5.
            #[allow(clippy::cast_possible_truncation)]
            let bar_idx = (reg_idx - BAR0_REG_IDX) as u8;
            self.bars.write(bar_idx, offset, data);
            None
        } else if in_msix_cap_header {
            // For the MsixCap structure, we need to capture writes to the second 2 bytes
            // of the capability header where Function Mask and MSI-X Enable bits are present.
            // Everything else can be served from `self.configuration`.
            self.msix_config
                .lock()
                .unwrap()
                .write_msg_ctl_register(offset, data);
            self.configuration
                .write_config_register(reg_idx, offset, data);
            None
        } else if in_pci_cfg {
            let offset = (reg_idx * 4 + u16::from(offset) - self.cap_pci_cfg_info.offset) as usize;
            self.write_cap_pci_cfg(offset, data)
        } else {
            self.configuration
                .write_config_register(reg_idx, offset, data);
            None
        }
    }

    fn read_config_register(&mut self, reg_idx: u16) -> u32 {
        let in_bars = BAR0_REG_IDX <= reg_idx && reg_idx < BAR0_REG_IDX + u16::from(NUM_BAR_REGS);
        let in_pci_cfg = self.cap_pci_cfg_info.in_range(reg_idx, 0, 4);

        if in_bars {
            // reg_idx is in [BAR0_REG_IDX, BAR0_REG_IDX+NUM_BAR_REGS), so the difference is 0..5.
            #[allow(clippy::cast_possible_truncation)]
            let bar_idx = (reg_idx - BAR0_REG_IDX) as u8;
            let mut value = [0u8; 4];
            self.bars.read(bar_idx, 0, &mut value);
            u32::from_le_bytes(value)
        } else if in_pci_cfg {
            // Handle the special case where the capability VIRTIO_PCI_CAP_PCI_CFG
            // is accessed. This capability has a special meaning as it allows the
            // guest to access other capabilities without mapping the PCI BAR.
            let offset = (reg_idx * 4 - self.cap_pci_cfg_info.offset) as usize;
            let mut data = [0u8; 4];
            let len = u32::from(self.cap_pci_cfg_info.cap.cap.length) as usize;
            if len <= 4 {
                self.read_cap_pci_cfg(offset, &mut data[..len]);
                u32::from_le_bytes(data)
            } else {
                0
            }
        } else {
            self.configuration.read_reg(reg_idx)
        }
    }

    fn read_bar(&mut self, _base: u64, offset: u64, data: &mut [u8]) {
        match offset {
            o if o < u64::from(COMMON_CONFIG_BAR_OFFSET + COMMON_CONFIG_SIZE) => {
                self.common_config.read(
                    o - u64::from(COMMON_CONFIG_BAR_OFFSET),
                    data,
                    self.device.clone(),
                )
            }
            o if (u64::from(ISR_CONFIG_BAR_OFFSET)
                ..u64::from(ISR_CONFIG_BAR_OFFSET + ISR_CONFIG_SIZE))
                .contains(&o) =>
            {
                // We don't actually support legacy INT#x interrupts for VirtIO PCI devices
                warn!("pci: read access to unsupported ISR status field");
                data.fill(0);
            }
            o if (u64::from(DEVICE_CONFIG_BAR_OFFSET)
                ..u64::from(DEVICE_CONFIG_BAR_OFFSET + DEVICE_CONFIG_SIZE))
                .contains(&o) =>
            {
                let device = self.device.lock().unwrap();
                device.read_config(o - u64::from(DEVICE_CONFIG_BAR_OFFSET), data);
            }
            o if (u64::from(NOTIFICATION_BAR_OFFSET)
                ..u64::from(NOTIFICATION_BAR_OFFSET + NOTIFICATION_SIZE))
                .contains(&o) =>
            {
                // Handled with ioeventfds.
                warn!("pci: unexpected read to notification BAR. Offset {o:#x}");
            }
            o if (u64::from(MSIX_TABLE_BAR_OFFSET)
                ..u64::from(MSIX_TABLE_BAR_OFFSET + MSIX_TABLE_SIZE))
                .contains(&o) =>
            {
                if let Some(interrupt) = &self.virtio_interrupt {
                    interrupt
                        .msix_config
                        .lock()
                        .unwrap()
                        .read_table(o - u64::from(MSIX_TABLE_BAR_OFFSET), data);
                }
            }
            o if (u64::from(MSIX_PBA_BAR_OFFSET)
                ..u64::from(MSIX_PBA_BAR_OFFSET + MSIX_PBA_SIZE))
                .contains(&o) =>
            {
                if let Some(interrupt) = &self.virtio_interrupt {
                    interrupt
                        .msix_config
                        .lock()
                        .unwrap()
                        .read_pba(o - u64::from(MSIX_PBA_BAR_OFFSET), data);
                }
            }
            _ => (),
        }
    }

    fn write_bar(&mut self, _base: u64, offset: u64, data: &[u8]) -> Option<Arc<Barrier>> {
        match offset {
            o if o < u64::from(COMMON_CONFIG_BAR_OFFSET + COMMON_CONFIG_SIZE) => {
                self.common_config.write(
                    o - u64::from(COMMON_CONFIG_BAR_OFFSET),
                    data,
                    self.device.clone(),
                    self.device_activated.load(Ordering::SeqCst),
                )
            }
            o if (u64::from(ISR_CONFIG_BAR_OFFSET)
                ..u64::from(ISR_CONFIG_BAR_OFFSET + ISR_CONFIG_SIZE))
                .contains(&o) =>
            {
                // We don't actually support legacy INT#x interrupts for VirtIO PCI devices
                warn!("pci: access to unsupported ISR status field");
            }
            o if (u64::from(DEVICE_CONFIG_BAR_OFFSET)
                ..u64::from(DEVICE_CONFIG_BAR_OFFSET + DEVICE_CONFIG_SIZE))
                .contains(&o) =>
            {
                let mut device = self.device.lock().unwrap();
                device.write_config(o - u64::from(DEVICE_CONFIG_BAR_OFFSET), data);
            }
            o if (u64::from(NOTIFICATION_BAR_OFFSET)
                ..u64::from(NOTIFICATION_BAR_OFFSET + NOTIFICATION_SIZE))
                .contains(&o) =>
            {
                // Handled with ioeventfds.
                warn!("pci: unexpected write to notification BAR. Offset {o:#x}");
            }
            o if (u64::from(MSIX_TABLE_BAR_OFFSET)
                ..u64::from(MSIX_TABLE_BAR_OFFSET + MSIX_TABLE_SIZE))
                .contains(&o) =>
            {
                if let Some(interrupt) = &self.virtio_interrupt {
                    interrupt
                        .msix_config
                        .lock()
                        .unwrap()
                        .write_table(o - u64::from(MSIX_TABLE_BAR_OFFSET), data);
                }
            }
            o if (u64::from(MSIX_PBA_BAR_OFFSET)
                ..u64::from(MSIX_PBA_BAR_OFFSET + MSIX_PBA_SIZE))
                .contains(&o) =>
            {
                if let Some(interrupt) = &self.virtio_interrupt {
                    interrupt
                        .msix_config
                        .lock()
                        .unwrap()
                        .write_pba(o - u64::from(MSIX_PBA_BAR_OFFSET), data);
                }
            }
            _ => (),
        };

        // Try and activate the device if the driver status has changed
        if self.needs_activation() {
            debug!("Activating device");
            let interrupt = Arc::clone(self.virtio_interrupt.as_ref().unwrap());
            match self
                .virtio_device()
                .lock()
                .unwrap()
                .activate(self.memory.clone(), interrupt.clone())
            {
                Ok(()) => self.device_activated.store(true, Ordering::SeqCst),
                Err(err) => {
                    self.common_config.driver_status |= DEVICE_NEEDS_RESET;
                    error!("Error activating device: {err:?}");

                    // Section 2.1.2 of the specification states that we need to send a device
                    // configuration change interrupt
                    let _ = interrupt.trigger(VirtioInterruptType::Config);
                }
            }
        }

        // Device has been reset by the driver
        if self.device_activated.load(Ordering::SeqCst) && self.is_driver_init() {
            let mut device = self.device.lock().unwrap();
            let reset_result = device.reset();
            match reset_result {
                Some(_) => {
                    // Upon reset the device returns its interrupt EventFD
                    self.virtio_interrupt = None;
                    self.device_activated.store(false, Ordering::SeqCst);

                    // Reset queue readiness (changes queue_enable), queue sizes
                    // and selected_queue as per spec for reset
                    self.virtio_device()
                        .lock()
                        .unwrap()
                        .queues_mut()
                        .iter_mut()
                        .for_each(Queue::reset);
                    self.common_config.queue_select = 0;
                }
                None => {
                    error!("Attempt to reset device when not implemented in underlying device");
                    // The virtio spec does not specify what to do if reset fails.
                    //
                    // Firecracker's MMIO transport sets FAILED in this case, but we must NOT do
                    // that for PCI. During shutdown, the Linux kernel issues a reset to each
                    // virtio device. The virtio PCI driver then polls device_status until it
                    // reads back 0, unlike the virtio MMIO driver which simply writes 0 and
                    // returns. Setting FAILED would cause the poll to spin forever, breaking
                    // reboot command and Ctrl-Alt-Del.
                    // - PCI: https://elixir.bootlin.com/linux/v6.19.8/source/drivers/virtio/virtio_pci_modern.c#L546-L565
                    // - MMIO: https://elixir.bootlin.com/linux/v6.19.8/source/drivers/virtio/virtio_mmio.c#L251-L258
                    //
                    // Since device_status was already set to INIT by set_device_status(), we don't
                    // need to set it again here.  However, the backend device is still active since
                    // reset() is unimplemented.  The combination of device_activated == true and
                    // device_status == INIT will cause set_device_status() to block any
                    // re-initialization attempts.
                }
            }
        }
        None
    }
}
