// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
// Ported into oci-vmm from Firecracker (src/vmm/src/arch/mod.rs), trimmed of metrics/snapshot/ACPI.

//! x86_64 architecture setup: guest memory layout, boot-time register and
//! table initialization (GDT, page tables, MP table, LAPIC LINTs).

pub mod gdt;
pub mod interrupts;
pub mod layout;
pub mod mptable;
pub mod regs;

/// Types of boot protocols the guest can be started with.
///
/// We only ever direct-boot Linux bzImage/ELF kernels via the Linux 64-bit
/// boot protocol; PVH boot was dropped in the port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootProtocol {
    /// Linux 64-bit boot protocol.
    LinuxBoot,
}

/// Specifies the entry point address where the guest must start executing
/// code, as well as which boot protocol is to be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryPoint {
    /// Address in guest memory where the guest must start execution.
    pub entry_addr: vm_memory::GuestAddress,
    /// Specifies which boot protocol to use.
    pub protocol: BootProtocol,
}
