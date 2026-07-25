// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Portions Copyright 2017 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the THIRD-PARTY file.
// Ported into oci-vmm from Firecracker (src/vmm/src/arch/x86_64/mptable.rs), trimmed of metrics/snapshot/ACPI.

//! MP (MultiProcessor Specification) table setup, so the guest kernel can
//! discover its CPUs and the IOAPIC without ACPI.
#![allow(unsafe_code)] // `unsafe impl ByteValued` for the plain-old-data mpspec structs below.

use std::mem::{self, size_of};

use libc::c_char;
use tracing::debug;
use vm_memory::{Address, ByteValued, Bytes, GuestAddress, GuestMemory};

use super::layout::{GSI_LEGACY_END, SYSTEM_MEM_START};
use crate::mem::GuestMemoryMmap;

/// Structures from the Intel MultiProcessor Specification (mpspec), matching
/// the layouts bindgen generates from the Linux kernel headers in Firecracker
/// (src/vmm/src/arch/x86_64/generated/mpspec.rs).
mod mpspec {
    #![allow(non_camel_case_types, non_upper_case_globals, dead_code)]

    pub const MP_PROCESSOR: u32 = 0;
    pub const MP_BUS: u32 = 1;
    pub const MP_IOAPIC: u32 = 2;
    pub const MP_INTSRC: u32 = 3;
    pub const MP_LINTSRC: u32 = 4;
    pub const CPU_ENABLED: u32 = 1;
    pub const CPU_BOOTPROCESSOR: u32 = 2;
    pub const MPC_APIC_USABLE: u32 = 1;
    pub const MP_IRQPOL_DEFAULT: u32 = 0;

    pub mod mp_irq_source_types {
        pub type Type = ::std::os::raw::c_uint;
        pub const mp_INT: Type = 0;
        pub const mp_NMI: Type = 1;
        pub const mp_SMI: Type = 2;
        pub const mp_ExtINT: Type = 3;
    }

    #[repr(C)]
    #[derive(Debug, Default, Copy, Clone, PartialEq)]
    pub struct mpf_intel {
        pub signature: [::std::os::raw::c_char; 4usize],
        pub physptr: ::std::os::raw::c_uint,
        pub length: ::std::os::raw::c_uchar,
        pub specification: ::std::os::raw::c_uchar,
        pub checksum: ::std::os::raw::c_uchar,
        pub feature1: ::std::os::raw::c_uchar,
        pub feature2: ::std::os::raw::c_uchar,
        pub feature3: ::std::os::raw::c_uchar,
        pub feature4: ::std::os::raw::c_uchar,
        pub feature5: ::std::os::raw::c_uchar,
    }

    #[repr(C)]
    #[derive(Debug, Default, Copy, Clone, PartialEq)]
    pub struct mpc_table {
        pub signature: [::std::os::raw::c_char; 4usize],
        pub length: ::std::os::raw::c_ushort,
        pub spec: ::std::os::raw::c_char,
        pub checksum: ::std::os::raw::c_char,
        pub oem: [::std::os::raw::c_char; 8usize],
        pub productid: [::std::os::raw::c_char; 12usize],
        pub oemptr: ::std::os::raw::c_uint,
        pub oemsize: ::std::os::raw::c_ushort,
        pub oemcount: ::std::os::raw::c_ushort,
        pub lapic: ::std::os::raw::c_uint,
        pub reserved: ::std::os::raw::c_uint,
    }

    #[repr(C)]
    #[derive(Debug, Default, Copy, Clone, PartialEq)]
    pub struct mpc_cpu {
        pub type_: ::std::os::raw::c_uchar,
        pub apicid: ::std::os::raw::c_uchar,
        pub apicver: ::std::os::raw::c_uchar,
        pub cpuflag: ::std::os::raw::c_uchar,
        pub cpufeature: ::std::os::raw::c_uint,
        pub featureflag: ::std::os::raw::c_uint,
        pub reserved: [::std::os::raw::c_uint; 2usize],
    }

    #[repr(C)]
    #[derive(Debug, Default, Copy, Clone, PartialEq)]
    pub struct mpc_bus {
        pub type_: ::std::os::raw::c_uchar,
        pub busid: ::std::os::raw::c_uchar,
        pub bustype: [::std::os::raw::c_uchar; 6usize],
    }

    #[repr(C)]
    #[derive(Debug, Default, Copy, Clone, PartialEq)]
    pub struct mpc_ioapic {
        pub type_: ::std::os::raw::c_uchar,
        pub apicid: ::std::os::raw::c_uchar,
        pub apicver: ::std::os::raw::c_uchar,
        pub flags: ::std::os::raw::c_uchar,
        pub apicaddr: ::std::os::raw::c_uint,
    }

    #[repr(C)]
    #[derive(Debug, Default, Copy, Clone, PartialEq)]
    pub struct mpc_intsrc {
        pub type_: ::std::os::raw::c_uchar,
        pub irqtype: ::std::os::raw::c_uchar,
        pub irqflag: ::std::os::raw::c_ushort,
        pub srcbus: ::std::os::raw::c_uchar,
        pub srcbusirq: ::std::os::raw::c_uchar,
        pub dstapic: ::std::os::raw::c_uchar,
        pub dstirq: ::std::os::raw::c_uchar,
    }

    #[repr(C)]
    #[derive(Debug, Default, Copy, Clone, PartialEq)]
    pub struct mpc_lintsrc {
        pub type_: ::std::os::raw::c_uchar,
        pub irqtype: ::std::os::raw::c_uchar,
        pub irqflag: ::std::os::raw::c_ushort,
        pub srcbusid: ::std::os::raw::c_uchar,
        pub srcbusirq: ::std::os::raw::c_uchar,
        pub destapic: ::std::os::raw::c_uchar,
        pub destapiclint: ::std::os::raw::c_uchar,
    }
}

// These `mpspec` wrapper types are only data, reading them from data is a safe initialization.
// SAFETY: POD
unsafe impl ByteValued for mpspec::mpc_bus {}
// SAFETY: POD
unsafe impl ByteValued for mpspec::mpc_cpu {}
// SAFETY: POD
unsafe impl ByteValued for mpspec::mpc_intsrc {}
// SAFETY: POD
unsafe impl ByteValued for mpspec::mpc_ioapic {}
// SAFETY: POD
unsafe impl ByteValued for mpspec::mpc_table {}
// SAFETY: POD
unsafe impl ByteValued for mpspec::mpc_lintsrc {}
// SAFETY: POD
unsafe impl ByteValued for mpspec::mpf_intel {}

/// Errors thrown while writing the MP table to guest memory.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum MptableError {
    /// There was too little guest memory to store the entire MP table.
    #[error("There was too little guest memory to store the entire MP table.")]
    NotEnoughMemory,
    /// The MP table has too little address space to be stored.
    #[error("The MP table has too little address space to be stored.")]
    AddressOverflow,
    /// Failure while zeroing out the memory for the MP table.
    #[error("Failure while zeroing out the memory for the MP table.")]
    Clear,
    /// Number of CPUs exceeds the maximum supported CPUs.
    #[error("Number of CPUs exceeds the maximum supported CPUs")]
    TooManyCpus,
    /// Number of IRQs exceeds the maximum supported IRQs.
    #[error("Number of IRQs exceeds the maximum supported IRQs")]
    TooManyIrqs,
    /// Failure to write the MP floating pointer.
    #[error("Failure to write the MP floating pointer.")]
    WriteMpfIntel,
    /// Failure to write MP CPU entry.
    #[error("Failure to write MP CPU entry.")]
    WriteMpcCpu,
    /// Failure to write MP ioapic entry.
    #[error("Failure to write MP ioapic entry.")]
    WriteMpcIoapic,
    /// Failure to write MP bus entry.
    #[error("Failure to write MP bus entry.")]
    WriteMpcBus,
    /// Failure to write MP interrupt source entry.
    #[error("Failure to write MP interrupt source entry.")]
    WriteMpcIntsrc,
    /// Failure to write MP local interrupt source entry.
    #[error("Failure to write MP local interrupt source entry.")]
    WriteMpcLintsrc,
    /// Failure to write MP table header.
    #[error("Failure to write MP table header.")]
    WriteMpcTable,
}

/// Maximum number of CPUs the MP table supports.
///
/// With APIC/xAPIC, there are only 255 APIC IDs available. And IOAPIC occupies
/// one APIC ID, so only 254 CPUs at maximum may be supported. Actually it's
/// a large number for our usecases.
pub const MAX_SUPPORTED_CPUS: u8 = 254;

// Convenience macro for making arrays of diverse character types.
macro_rules! char_array {
    ($t:ty; $( $c:expr ),*) => ( [ $( $c as $t ),* ] )
}

// Most of these variables are sourced from the Intel MP Spec 1.4.
const SMP_MAGIC_IDENT: [c_char; 4] = char_array!(c_char; '_', 'M', 'P', '_');
const MPC_SIGNATURE: [c_char; 4] = char_array!(c_char; 'P', 'C', 'M', 'P');
const MPC_SPEC: i8 = 4;
const MPC_OEM: [c_char; 8] = char_array!(c_char; 'F', 'C', ' ', ' ', ' ', ' ', ' ', ' ');
const MPC_PRODUCT_ID: [c_char; 12] = ['0' as c_char; 12];
const BUS_TYPE_ISA: [u8; 6] = *b"ISA   ";
const IO_APIC_DEFAULT_PHYS_BASE: u32 = 0xfec0_0000; // source: linux/arch/x86/include/asm/apicdef.h
const APIC_DEFAULT_PHYS_BASE: u32 = 0xfee0_0000; // source: linux/arch/x86/include/asm/apicdef.h
const APIC_VERSION: u8 = 0x14;
const CPU_STEPPING: u32 = 0x600;
const CPU_FEATURE_APIC: u32 = 0x200;
const CPU_FEATURE_FPU: u32 = 0x001;

fn compute_checksum<T: ByteValued>(v: &T) -> u8 {
    let mut checksum: u8 = 0;
    for i in v.as_slice() {
        checksum = checksum.wrapping_add(*i);
    }
    checksum
}

fn mpf_intel_compute_checksum(v: &mpspec::mpf_intel) -> u8 {
    let checksum = compute_checksum(v).wrapping_sub(v.checksum);
    (!checksum).wrapping_add(1)
}

fn compute_mp_size(num_cpus: u8) -> usize {
    mem::size_of::<mpspec::mpf_intel>()
        + mem::size_of::<mpspec::mpc_table>()
        + mem::size_of::<mpspec::mpc_cpu>() * (num_cpus as usize)
        + mem::size_of::<mpspec::mpc_ioapic>()
        + mem::size_of::<mpspec::mpc_bus>()
        + mem::size_of::<mpspec::mpc_intsrc>() * (GSI_LEGACY_END as usize + 1)
        + mem::size_of::<mpspec::mpc_lintsrc>() * 2
}

/// Performs setup of the MP table for the given `num_cpus`.
///
/// The table is written at [`SYSTEM_MEM_START`] (the start of the EBDA).
pub fn setup_mptable(mem: &GuestMemoryMmap, num_cpus: u8) -> Result<(), MptableError> {
    if num_cpus > MAX_SUPPORTED_CPUS {
        return Err(MptableError::TooManyCpus);
    }

    let mp_size = compute_mp_size(num_cpus);
    let mptable_addr = SYSTEM_MEM_START;
    debug!(
        "mptable: Writing {mp_size} bytes for MPTable {num_cpus} vCPUs at address {mptable_addr:#010x}"
    );

    // Used to keep track of the next base pointer into the MP table.
    let mut base_mp = GuestAddress(mptable_addr);
    let mut mp_num_entries: u16 = 0;

    let mut checksum: u8 = 0;
    let ioapicid: u8 = num_cpus + 1;

    // The checked_add here ensures the all of the following base_mp.unchecked_add's will be without
    // overflow.
    if let Some(end_mp) = base_mp.checked_add((mp_size - 1) as u64) {
        if !mem.address_in_range(end_mp) {
            return Err(MptableError::NotEnoughMemory);
        }
    } else {
        return Err(MptableError::AddressOverflow);
    }

    mem.write_slice(&vec![0; mp_size], base_mp)
        .map_err(|_| MptableError::Clear)?;

    {
        let size = mem::size_of::<mpspec::mpf_intel>() as u64;
        let mut mpf_intel = mpspec::mpf_intel {
            signature: SMP_MAGIC_IDENT,
            physptr: u32::try_from(base_mp.raw_value() + size).unwrap(),
            length: 1,
            specification: 4,
            ..mpspec::mpf_intel::default()
        };
        mpf_intel.checksum = mpf_intel_compute_checksum(&mpf_intel);
        mem.write_obj(mpf_intel, base_mp)
            .map_err(|_| MptableError::WriteMpfIntel)?;
        base_mp = base_mp.unchecked_add(size);
        mp_num_entries += 1;
    }

    // We set the location of the mpc_table here but we can't fill it out until we have the length
    // of the entire table later.
    let table_base = base_mp;
    base_mp = base_mp.unchecked_add(mem::size_of::<mpspec::mpc_table>() as u64);

    {
        let size = mem::size_of::<mpspec::mpc_cpu>() as u64;
        for cpu_id in 0..num_cpus {
            let mpc_cpu = mpspec::mpc_cpu {
                type_: mpspec::MP_PROCESSOR.try_into().unwrap(),
                apicid: cpu_id,
                apicver: APIC_VERSION,
                cpuflag: u8::try_from(mpspec::CPU_ENABLED).unwrap()
                    | if cpu_id == 0 {
                        u8::try_from(mpspec::CPU_BOOTPROCESSOR).unwrap()
                    } else {
                        0
                    },
                cpufeature: CPU_STEPPING,
                featureflag: CPU_FEATURE_APIC | CPU_FEATURE_FPU,
                ..Default::default()
            };
            mem.write_obj(mpc_cpu, base_mp)
                .map_err(|_| MptableError::WriteMpcCpu)?;
            base_mp = base_mp.unchecked_add(size);
            checksum = checksum.wrapping_add(compute_checksum(&mpc_cpu));
            mp_num_entries += 1;
        }
    }
    {
        let size = mem::size_of::<mpspec::mpc_bus>() as u64;
        let mpc_bus = mpspec::mpc_bus {
            type_: mpspec::MP_BUS.try_into().unwrap(),
            busid: 0,
            bustype: BUS_TYPE_ISA,
        };
        mem.write_obj(mpc_bus, base_mp)
            .map_err(|_| MptableError::WriteMpcBus)?;
        base_mp = base_mp.unchecked_add(size);
        checksum = checksum.wrapping_add(compute_checksum(&mpc_bus));
        mp_num_entries += 1;
    }
    {
        let size = mem::size_of::<mpspec::mpc_ioapic>() as u64;
        let mpc_ioapic = mpspec::mpc_ioapic {
            type_: mpspec::MP_IOAPIC.try_into().unwrap(),
            apicid: ioapicid,
            apicver: APIC_VERSION,
            flags: mpspec::MPC_APIC_USABLE.try_into().unwrap(),
            apicaddr: IO_APIC_DEFAULT_PHYS_BASE,
        };
        mem.write_obj(mpc_ioapic, base_mp)
            .map_err(|_| MptableError::WriteMpcIoapic)?;
        base_mp = base_mp.unchecked_add(size);
        checksum = checksum.wrapping_add(compute_checksum(&mpc_ioapic));
        mp_num_entries += 1;
    }
    // Per kvm_setup_default_irq_routing() in kernel
    for i in 0..=u8::try_from(GSI_LEGACY_END).map_err(|_| MptableError::TooManyIrqs)? {
        let size = mem::size_of::<mpspec::mpc_intsrc>() as u64;
        let mpc_intsrc = mpspec::mpc_intsrc {
            type_: mpspec::MP_INTSRC.try_into().unwrap(),
            irqtype: mpspec::mp_irq_source_types::mp_INT.try_into().unwrap(),
            irqflag: mpspec::MP_IRQPOL_DEFAULT.try_into().unwrap(),
            srcbus: 0,
            srcbusirq: i,
            dstapic: ioapicid,
            dstirq: i,
        };
        mem.write_obj(mpc_intsrc, base_mp)
            .map_err(|_| MptableError::WriteMpcIntsrc)?;
        base_mp = base_mp.unchecked_add(size);
        checksum = checksum.wrapping_add(compute_checksum(&mpc_intsrc));
        mp_num_entries += 1;
    }
    {
        let size = mem::size_of::<mpspec::mpc_lintsrc>() as u64;
        let mpc_lintsrc = mpspec::mpc_lintsrc {
            type_: mpspec::MP_LINTSRC.try_into().unwrap(),
            irqtype: mpspec::mp_irq_source_types::mp_ExtINT.try_into().unwrap(),
            irqflag: mpspec::MP_IRQPOL_DEFAULT.try_into().unwrap(),
            srcbusid: 0,
            srcbusirq: 0,
            destapic: 0,
            destapiclint: 0,
        };
        mem.write_obj(mpc_lintsrc, base_mp)
            .map_err(|_| MptableError::WriteMpcLintsrc)?;
        base_mp = base_mp.unchecked_add(size);
        checksum = checksum.wrapping_add(compute_checksum(&mpc_lintsrc));
        mp_num_entries += 1;
    }
    {
        let size = mem::size_of::<mpspec::mpc_lintsrc>() as u64;
        let mpc_lintsrc = mpspec::mpc_lintsrc {
            type_: mpspec::MP_LINTSRC.try_into().unwrap(),
            irqtype: mpspec::mp_irq_source_types::mp_NMI.try_into().unwrap(),
            irqflag: mpspec::MP_IRQPOL_DEFAULT.try_into().unwrap(),
            srcbusid: 0,
            srcbusirq: 0,
            destapic: 0xFF,
            destapiclint: 1,
        };
        mem.write_obj(mpc_lintsrc, base_mp)
            .map_err(|_| MptableError::WriteMpcLintsrc)?;
        base_mp = base_mp.unchecked_add(size);
        checksum = checksum.wrapping_add(compute_checksum(&mpc_lintsrc));
        mp_num_entries += 1;
    }

    // At this point we know the size of the mp_table.
    let table_end = base_mp;

    {
        let mut mpc_table = mpspec::mpc_table {
            signature: MPC_SIGNATURE,
            // it's safe to use unchecked_offset_from because
            // table_end > table_base
            length: table_end
                .unchecked_offset_from(table_base)
                .try_into()
                .unwrap(),
            spec: MPC_SPEC,
            oem: MPC_OEM,
            oemcount: mp_num_entries,
            productid: MPC_PRODUCT_ID,
            lapic: APIC_DEFAULT_PHYS_BASE,
            ..Default::default()
        };
        debug_assert_eq!(
            mpc_table.length as usize + size_of::<mpspec::mpf_intel>(),
            mp_size
        );
        checksum = checksum.wrapping_add(compute_checksum(&mpc_table));
        #[allow(clippy::cast_possible_wrap)]
        let checksum_final = (!checksum).wrapping_add(1) as i8;
        mpc_table.checksum = checksum_final;
        mem.write_obj(mpc_table, table_base)
            .map_err(|_| MptableError::WriteMpcTable)?;
    }

    Ok(())
}
