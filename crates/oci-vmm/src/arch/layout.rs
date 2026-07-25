// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Portions Copyright 2017 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the THIRD-PARTY file.
// Ported into oci-vmm from Firecracker (src/vmm/src/arch/x86_64/layout.rs), trimmed of metrics/snapshot/ACPI.

//! Magic addresses externally used to lay out x86_64 VMs.

/// Initial stack for the boot CPU.
pub const BOOT_STACK_POINTER: u64 = 0x8ff0;

/// Kernel command line start address.
pub const CMDLINE_START: u64 = 0x20000;
/// Kernel command line maximum size.
pub const CMDLINE_MAX_SIZE: usize = 2048;

/// Start of the high memory (1 MiB).
pub const HIMEM_START: u64 = 0x0010_0000;

/// The 'zero page', a.k.a. Linux kernel boot params.
pub const ZERO_PAGE_START: u64 = 0x7000;

/// Address for the TSS setup.
pub const KVM_TSS_ADDRESS: u64 = 0xfffb_d000;

/// APIC address.
pub const APIC_ADDR: u32 = 0xfee0_0000;

/// IOAPIC address.
pub const IOAPIC_ADDR: u32 = 0xfec0_0000;

/// Start of memory region we will use for system data (the MP table). We put
/// its start address where the EBDA normally starts, i.e. in the last 1 KiB
/// of the first 640 KiB of memory.
pub const SYSTEM_MEM_START: u64 = 0x9fc00;
/// Size of the reserved legacy area from [`SYSTEM_MEM_START`] up to
/// [`HIMEM_START`] (EBDA/VGA/option-ROM space on a real PC; marked
/// e820-reserved rather than RAM).
pub const SYSTEM_MEM_SIZE: u64 = HIMEM_START - SYSTEM_MEM_START;

/// First address that cannot be addressed using 32 bits anymore.
pub const FIRST_ADDR_PAST_32BITS: u64 = 1 << 32;

/// The size of the memory area reserved for MMIO 32-bit accesses (1 GiB).
pub const MMIO32_MEM_SIZE: u64 = 1 << 30;
/// The start of the memory area reserved for MMIO 32-bit accesses (3 GiB).
pub const MMIO32_MEM_START: u64 = FIRST_ADDR_PAST_32BITS - MMIO32_MEM_SIZE;

/// Highest address (exclusive) at which the initrd may be placed. The initrd
/// must stay below the 32-bit MMIO gap because the Linux boot protocol's
/// `ramdisk_image` header field is a u32.
pub const INITRD_HIGHEST_ADDR: u64 = MMIO32_MEM_START;

// Typically, on x86 systems 24 IRQs are used for legacy devices (0-23).
// However, the first 5 are reserved. We allocate the remaining GSIs to MSIs.
/// First usable GSI for legacy interrupts (IRQ) on x86_64.
pub const GSI_LEGACY_START: u32 = 5;
/// Last usable GSI for legacy interrupts (IRQ) on x86_64.
pub const GSI_LEGACY_END: u32 = 23;
/// First GSI used by MSI after legacy GSI.
pub const GSI_MSI_START: u32 = GSI_LEGACY_END + 1;
/// The highest available GSI in KVM (KVM_MAX_IRQ_ROUTES=4096).
pub const GSI_MSI_END: u32 = 4095;
