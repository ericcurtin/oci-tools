// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Portions Copyright 2017 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the THIRD-PARTY file.
// Ported into oci-vmm from Firecracker (src/vmm/src/vstate/vm.rs and
// src/vmm/src/arch/x86_64/vm.rs), trimmed of metrics/snapshot/templates.

//! The x86_64 KVM virtual machine: VM fd creation, in-kernel irqchip + PIT,
//! guest memory region registration, and GSI routing for device interrupts.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use kvm_bindings::{
    KVM_IRQ_ROUTING_IRQCHIP, KVM_IRQ_ROUTING_MSI, KVM_IRQCHIP_IOAPIC, KVM_MSI_VALID_DEVID,
    KVM_PIT_SPEAKER_DUMMY, KvmIrqRouting, kvm_irq_routing_entry, kvm_pit_config,
    kvm_userspace_memory_region,
};
use kvm_ioctls::{Kvm, VmFd};
use tracing::{debug, info};
use vm_memory::{Address, GuestMemory, GuestMemoryRegion, MemoryRegionAddress};
use vmm_sys_util::errno;
use vmm_sys_util::eventfd::EventFd;

use crate::arch::layout;
use crate::mem::GuestMemoryMmap;
use crate::vstate::interrupts::{InterruptError, MsixVector, MsixVectorConfig, MsixVectorGroup};

#[derive(Debug)]
/// A struct representing an interrupt line used by some device of the microVM
pub struct RoutingEntry {
    entry: kvm_irq_routing_entry,
    masked: bool,
}

/// Errors associated with the wrappers over KVM ioctls.
#[derive(Debug, thiserror::Error)]
pub enum VmError {
    /// KVM_CREATE_VM failed.
    #[error("Failed to create VM: {0}")]
    CreateVm(kvm_ioctls::Error),
    /// KVM_SET_TSS_ADDR failed.
    #[error("Failed during KVM_SET_TSS_ADDRESS: {0}")]
    SetTssAddress(kvm_ioctls::Error),
    /// KVM_CREATE_IRQCHIP or KVM_CREATE_PIT2 failed.
    #[error("Failed to setup in-kernel irqchip: {0}")]
    SetupIrqChip(kvm_ioctls::Error),
    /// KVM_SET_USER_MEMORY_REGION failed.
    #[error("Cannot set the memory regions: {0}")]
    SetUserMemoryRegion(kvm_ioctls::Error),
    /// Ran out of KVM memory slots.
    #[error("The number of configured slots is bigger than the maximum reported by KVM: {0}")]
    NotEnoughMemorySlots(u32),
    /// Could not resolve a region's host address.
    #[error("Failed to get host address of guest memory region: {0}")]
    HostAddress(#[from] vm_memory::GuestMemoryError),
}

/// The KVM virtual machine, trimmed to what a pet VM needs on x86_64: the VM
/// file descriptor, an in-kernel irqchip + PIT, plain (non-dirty-logged)
/// memory slots, and the interrupt routing table for legacy and MSI GSIs.
#[derive(Debug)]
pub struct Vm {
    /// The KVM file descriptor used to access this Vm.
    fd: VmFd,
    /// Maximum number of memory slots reported by KVM.
    max_memslots: u32,
    /// Next free KVM memory slot.
    next_kvm_slot: AtomicU32,
    /// Next free GSI for MSI vectors (bump allocator over the MSI GSI space).
    next_gsi: AtomicU32,
    /// Interrupt routing entries used by the Vm's devices.
    interrupts: Mutex<HashMap<u32, RoutingEntry>>,
}

impl Vm {
    /// Create a new `Vm` struct: the KVM VM fd, the TSS region, and the
    /// in-kernel irqchip (PIC/IOAPIC) plus PIT.
    pub fn new(kvm: &Kvm) -> Result<Vm, VmError> {
        // It is known that KVM_CREATE_VM occasionally fails with EINTR on heavily loaded machines
        // with many VMs.
        //
        // The behavior itself that KVM_CREATE_VM can return EINTR is intentional. This is because
        // the KVM_CREATE_VM path includes mm_take_all_locks() that is CPU intensive and all CPU
        // intensive syscalls should check for pending signals and return EINTR immediately to allow
        // userland to remain interactive.
        // https://lists.nongnu.org/archive/html/qemu-devel/2014-01/msg01740.html
        //
        // However, it is empirically confirmed that, even though there is no pending signal,
        // KVM_CREATE_VM returns EINTR.
        // https://lore.kernel.org/qemu-devel/8735e0s1zw.wl-maz@kernel.org/
        //
        // To mitigate it, QEMU does an infinite retry on EINTR that greatly improves reliabiliy:
        // - https://github.com/qemu/qemu/commit/94ccff133820552a859c0fb95e33a539e0b90a75
        // - https://github.com/qemu/qemu/commit/bbde13cd14ad4eec18529ce0bf5876058464e124
        //
        // Similarly, we do retries up to 5 times.
        const MAX_ATTEMPTS: u32 = 5;
        let mut attempt = 1;
        let fd = loop {
            match kvm.create_vm() {
                Ok(fd) => break fd,
                Err(e) if e.errno() == libc::EINTR && attempt < MAX_ATTEMPTS => {
                    info!("Attempt #{attempt} of KVM_CREATE_VM returned EINTR");
                    // Exponential backoff (1us, 2us, 4us, and 8us => 15us in total)
                    std::thread::sleep(std::time::Duration::from_micros(2u64.pow(attempt - 1)));
                }
                Err(e) => return Err(VmError::CreateVm(e)),
            }

            attempt += 1;
        };

        fd.set_tss_address(usize::try_from(layout::KVM_TSS_ADDRESS).unwrap())
            .map_err(VmError::SetTssAddress)?;

        // For x86_64 the interrupt controller must be created before the vCPUs.
        fd.create_irq_chip().map_err(VmError::SetupIrqChip)?;
        // We need to enable the emulation of a dummy speaker port stub so that writing to port 0x61
        // (i.e. KVM_SPEAKER_BASE_ADDRESS) does not trigger an exit to user space.
        let pit_config = kvm_pit_config {
            flags: KVM_PIT_SPEAKER_DUMMY,
            ..Default::default()
        };
        fd.create_pit2(pit_config).map_err(VmError::SetupIrqChip)?;

        Ok(Vm {
            fd,
            max_memslots: u32::try_from(kvm.get_nr_memslots()).unwrap_or(u32::MAX),
            next_kvm_slot: AtomicU32::new(0),
            next_gsi: AtomicU32::new(layout::GSI_MSI_START),
            interrupts: Mutex::new(HashMap::new()),
        })
    }

    /// Gets a reference to the kvm file descriptor owned by this VM.
    pub fn fd(&self) -> &VmFd {
        &self.fd
    }

    /// Reserves the next `slot_cnt` contiguous kvm slot ids and returns the first one
    pub fn next_kvm_slot(&self, slot_cnt: u32) -> Option<u32> {
        let next = self.next_kvm_slot.fetch_add(slot_cnt, Ordering::Relaxed);
        if self.max_memslots <= next {
            None
        } else {
            Some(next)
        }
    }

    /// Register all regions of `guest_mem` with this [`Vm`] via
    /// KVM_SET_USER_MEMORY_REGION (plain regions, no dirty logging).
    #[allow(unsafe_code)] // KVM_SET_USER_MEMORY_REGION needs valid userspace addresses, provided by GuestMemoryMmap.
    pub fn register_memory_regions(&self, guest_mem: &GuestMemoryMmap) -> Result<(), VmError> {
        for region in guest_mem.iter() {
            let slot = self
                .next_kvm_slot(1)
                .ok_or(VmError::NotEnoughMemorySlots(self.max_memslots))?;
            let kvm_region = kvm_userspace_memory_region {
                slot,
                guest_phys_addr: region.start_addr().raw_value(),
                memory_size: region.len(),
                userspace_addr: region.get_host_address(MemoryRegionAddress(0))? as u64,
                flags: 0,
            };
            // SAFETY: the fd is a valid KVM fd, and the region points to memory mmap'ed
            // by GuestMemoryMmap, valid for the whole lifetime of the guest.
            unsafe {
                self.fd
                    .set_user_memory_region(kvm_region)
                    .map_err(VmError::SetUserMemoryRegion)?;
            }
        }

        Ok(())
    }

    /// Allocate `count` contiguous GSIs for MSI vectors (bump allocator over
    /// `[layout::GSI_MSI_START, layout::GSI_MSI_END]`).
    pub fn allocate_gsis(&self, count: u16) -> Result<Vec<u32>, InterruptError> {
        let first = self.next_gsi.fetch_add(u32::from(count), Ordering::Relaxed);
        let last = first + u32::from(count);
        if layout::GSI_MSI_END < last.saturating_sub(1) {
            return Err(InterruptError::GsiExhausted);
        }
        Ok((first..last).collect())
    }

    /// Register a device IRQ: an irqfd routed through the in-kernel irqchip
    /// (IOAPIC) at pin `gsi`.
    pub fn register_irq(&self, fd: &EventFd, gsi: u32) -> Result<(), errno::Error> {
        self.fd.register_irqfd(fd, gsi)?;

        let mut entry = kvm_irq_routing_entry {
            gsi,
            type_: KVM_IRQ_ROUTING_IRQCHIP,
            ..Default::default()
        };
        entry.u.irqchip.irqchip = KVM_IRQCHIP_IOAPIC;
        entry.u.irqchip.pin = gsi;

        self.interrupts.lock().expect("Poisoned lock").insert(
            gsi,
            RoutingEntry {
                entry,
                masked: false,
            },
        );
        Ok(())
    }

    /// Register an MSI device interrupt
    pub fn register_msi(
        &self,
        route: &MsixVector,
        masked: bool,
        config: MsixVectorConfig,
    ) -> Result<(), errno::Error> {
        let mut entry = kvm_irq_routing_entry {
            gsi: route.gsi,
            type_: KVM_IRQ_ROUTING_MSI,
            ..Default::default()
        };
        entry.u.msi.address_lo = config.low_addr;
        entry.u.msi.address_hi = config.high_addr;
        entry.u.msi.data = config.data;

        if self.fd.check_extension(kvm_ioctls::Cap::MsiDevid) {
            entry.flags = KVM_MSI_VALID_DEVID;
            entry.u.msi.__bindgen_anon_1.devid = config.devid.into();
        }

        self.interrupts
            .lock()
            .expect("Poisoned lock")
            .insert(route.gsi, RoutingEntry { entry, masked });

        Ok(())
    }

    /// Create a group of MSI-X interrupts
    pub fn create_msix_group(vm: Arc<Vm>, count: u16) -> Result<MsixVectorGroup, InterruptError> {
        debug!("Creating new MSI group with {count} vectors");
        let mut vectors = Vec::with_capacity(count as usize);
        for gsi in vm.allocate_gsis(count)? {
            vectors.push(MsixVector::new(gsi, false)?);
        }

        Ok(MsixVectorGroup { vm, vectors })
    }

    /// Set GSI routes to KVM
    pub fn set_gsi_routes(&self) -> Result<(), InterruptError> {
        let entries = self.interrupts.lock().expect("Poisoned lock");
        let mut routes = KvmIrqRouting::new(0)?;

        for entry in entries.values() {
            if entry.masked {
                continue;
            }
            routes.push(entry.entry)?;
        }

        self.fd.set_gsi_routing(&routes)?;
        Ok(())
    }
}
