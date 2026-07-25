// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Portions Copyright 2017 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the THIRD-PARTY file.
// Ported into oci-vmm from Firecracker (src/vmm/src/vstate/vcpu.rs,
// src/vmm/src/arch/x86_64/vcpu.rs and src/vmm/src/arch/x86_64/msr.rs),
// trimmed of metrics/snapshot/templates.

//! x86_64 KVM vCPU: creation, boot-time configuration (CPUID, MSRs,
//! registers, LAPIC) and a thin KVM_RUN passthrough — the builder owns
//! the vCPU threads and the exit dispatch loop.

use kvm_bindings::{
    CpuId, KVM_CPUID_FLAG_SIGNIFCANT_INDEX, KVM_MAX_CPUID_ENTRIES, Msrs, kvm_cpuid_entry2,
    kvm_msr_entry,
};
use kvm_ioctls::{Kvm, VcpuExit, VcpuFd, VmFd};

use crate::arch::EntryPoint;
use crate::mem::GuestMemoryMmap;

/// Hard limit on the number of vCPUs we support (mirrors Firecracker's
/// `MAX_SUPPORTED_VCPUS`), used to size the CPUID core-domain topology.
const MAX_SUPPORTED_VCPUS: u32 = 32;

// MSR indices from Linux's arch/x86/include/asm/msr-index.h (Firecracker
// generates these into arch/x86_64/generated/msr_index.rs).
const MSR_IA32_SYSENTER_CS: u32 = 0x174;
const MSR_IA32_SYSENTER_ESP: u32 = 0x175;
const MSR_IA32_SYSENTER_EIP: u32 = 0x176;
const MSR_STAR: u32 = 0xc000_0081;
const MSR_LSTAR: u32 = 0xc000_0082;
const MSR_CSTAR: u32 = 0xc000_0083;
const MSR_SYSCALL_MASK: u32 = 0xc000_0084;
const MSR_KERNEL_GS_BASE: u32 = 0xc000_0102;
const MSR_IA32_TSC: u32 = 0x10;
const MSR_IA32_MISC_ENABLE: u32 = 0x1a0;
const MSR_IA32_MISC_ENABLE_FAST_STRING: u32 = 0x1;
const MSR_MTRR_DEF_TYPE: u32 = 0x2ff;

/// Errors associated with creating a vCPU.
#[derive(Debug, thiserror::Error)]
pub enum VcpuError {
    /// KVM_CREATE_VCPU failed.
    #[error("Cannot open the VCPU file descriptor: {0}")]
    VcpuFd(kvm_ioctls::Error),
}

/// Error type for [`Vcpu::configure`].
#[derive(Debug, thiserror::Error)]
pub enum VcpuConfigureError {
    /// KVM_GET_SUPPORTED_CPUID failed.
    #[error("Failed to get supported CPUID: {0}")]
    GetSupportedCpuid(kvm_ioctls::Error),
    /// A FamStructWrapper operation failed.
    #[error("FamStruct error: {0}")]
    Fam(#[from] vmm_sys_util::fam::Error),
    /// KVM_SET_CPUID2 failed.
    #[error("Failed to set CPUID: {0}")]
    SetCpuid(kvm_ioctls::Error),
    /// KVM_SET_MSRS failed.
    #[error("Failed to set MSRs: {0}")]
    SetMsrs(kvm_ioctls::Error),
    /// KVM_SET_MSRS wrote fewer entries than requested.
    #[error("Failed to set all KVM MSRs for this vCPU. Only a partial write was done.")]
    SetMsrsIncomplete,
    /// Setting up the general purpose registers failed.
    #[error("Failed to setup registers: {0}")]
    SetupRegisters(#[from] crate::arch::regs::SetupRegistersError),
    /// Setting up the FPU failed.
    #[error("Failed to setup FPU: {0}")]
    SetupFpu(#[from] crate::arch::regs::SetupFpuError),
    /// Setting up the special registers failed.
    #[error("Failed to setup special registers: {0}")]
    SetupSpecialRegisters(#[from] crate::arch::regs::SetupSpecialRegistersError),
    /// Configuring the LAPIC failed.
    #[error("Failed to configure LAPICs: {0}")]
    SetLint(#[from] crate::arch::interrupts::InterruptError),
}

/// A wrapper around creating and using a kvm x86_64 vcpu.
#[derive(Debug)]
pub struct Vcpu {
    /// KVM vcpu fd.
    fd: VcpuFd,
    /// Index of vcpu.
    index: u8,
}

impl Vcpu {
    /// Constructs a new vcpu for `vm`.
    ///
    /// # Arguments
    ///
    /// * `vm` - The VM fd to which this vcpu will get attached.
    /// * `index` - Represents the 0-based CPU index between [0, max vcpus).
    pub fn new(vm: &VmFd, index: u8) -> Result<Self, VcpuError> {
        let fd = vm.create_vcpu(index.into()).map_err(VcpuError::VcpuFd)?;
        Ok(Vcpu { fd, index })
    }

    /// Index of this vcpu.
    pub fn index(&self) -> u8 {
        self.index
    }

    /// Gets a reference to the KVM vcpu fd.
    pub fn fd(&self) -> &VcpuFd {
        &self.fd
    }

    /// Configures a x86_64 specific vcpu for booting Linux and should be called once per vcpu.
    ///
    /// # Arguments
    ///
    /// * `kvm` - The KVM fd, used to query the supported CPUID.
    /// * `guest_mem` - The guest memory used by this microvm.
    /// * `entry_point` - Specifies the boot protocol and offset from `guest_mem` at which the
    ///   kernel starts.
    /// * `vcpu_count` - The total number of vCPUs of the microvm.
    pub fn configure(
        &mut self,
        kvm: &Kvm,
        guest_mem: &GuestMemoryMmap,
        entry_point: EntryPoint,
        vcpu_count: u8,
    ) -> Result<(), VcpuConfigureError> {
        let mut cpuid = kvm
            .get_supported_cpuid(KVM_MAX_CPUID_ENTRIES)
            .map_err(VcpuConfigureError::GetSupportedCpuid)?;

        // Apply machine specific changes to CPUID.
        normalize_cpuid(&mut cpuid, self.index, vcpu_count)?;

        self.fd
            .set_cpuid2(&cpuid)
            .map_err(VcpuConfigureError::SetCpuid)?;

        // Apply MSR modification to comply with the Linux boot protocol.
        set_msrs(&self.fd, &create_boot_msr_entries())?;

        crate::arch::regs::setup_regs(&self.fd, entry_point)?;
        crate::arch::regs::setup_fpu(&self.fd)?;
        crate::arch::regs::setup_sregs(guest_mem, &self.fd)?;
        crate::arch::interrupts::set_lint(&self.fd)?;
        Ok(())
    }

    /// Runs the vCPU in KVM context (KVM_RUN passthrough). The builder owns
    /// the dispatch loop that handles the returned exit reason.
    pub fn kvm_run(&mut self) -> Result<VcpuExit<'_>, kvm_ioctls::Error> {
        self.fd.run()
    }

    /// Sets the `immediate_exit` flag in the shared kvm_run structure, making
    /// the next (or current, when kicked with a signal) KVM_RUN return with
    /// EINTR without entering the guest.
    pub fn set_kvm_immediate_exit(&mut self, val: u8) {
        self.fd.set_kvm_immediate_exit(val);
    }
}

/// Replace a bit range in `dst` (bits `lsb..=msb`, inclusive) with `val`.
fn set_range(dst: &mut u32, lsb: u8, msb: u8, val: u32) {
    let mask = if msb - lsb == 31 {
        u32::MAX
    } else {
        ((1u32 << (msb - lsb + 1)) - 1) << lsb
    };
    *dst = (*dst & !mask) | ((val << lsb) & mask);
}

/// Applies the required modifications to the supported CPUID for a vCPU: the
/// compact, faithful subset of Firecracker's `Cpuid::normalize` (no SMT, no
/// CPU templates) — initial APIC ID and cpu-count bits in leaf 0x1, the
/// TSC-deadline and hypervisor bits, and the x2APIC topology in leaf 0xB.
fn normalize_cpuid(
    cpuid: &mut CpuId,
    cpu_index: u8,
    cpu_count: u8,
) -> Result<(), VcpuConfigureError> {
    // The following commit changed the behavior of KVM_GET_SUPPORTED_CPUID to no longer
    // include CPUID.(EAX=0BH,ECX=1); add it back so the core domain gets enumerated.
    // https://lore.kernel.org/all/20221027092036.2698180-1-pbonzini@redhat.com/
    if !cpuid
        .as_slice()
        .iter()
        .any(|entry| entry.function == 0xB && entry.index == 0x1)
    {
        cpuid.push(kvm_cpuid_entry2 {
            function: 0xB,
            index: 0x1,
            flags: KVM_CPUID_FLAG_SIGNIFCANT_INDEX,
            ..Default::default()
        })?;
    }

    for entry in cpuid.as_mut_slice() {
        match entry.function {
            0x1 => {
                // CPUID.01H:EBX[15:08]
                // CLFLUSH line size (Value * 8 = cache line size in bytes).
                set_range(&mut entry.ebx, 8, 15, 8);

                // CPUID.01H:EBX[23:16]
                // Maximum number of addressable IDs for logical processors in this physical
                // package: the nearest power-of-2 integer that is not smaller than cpu_count.
                set_range(
                    &mut entry.ebx,
                    16,
                    23,
                    u32::from(cpu_count.next_power_of_two()),
                );

                // CPUID.01H:EBX[31:24]
                // Initial APIC ID.
                set_range(&mut entry.ebx, 24, 31, u32::from(cpu_index));

                // CPUID.01H:ECX[24] (Mnemonic: TSC-Deadline)
                entry.ecx |= 1 << 24;

                // CPUID.01H:ECX[31] (Mnemonic: Hypervisor)
                entry.ecx |= 1 << 31;

                // CPUID.01H:EDX[28] (Mnemonic: HTT)
                // Whether CPUID.1.EBX[23:16] is valid; set iff there is more than one
                // logical processor in the package.
                if cpu_count > 1 {
                    entry.edx |= 1 << 28;
                } else {
                    entry.edx &= !(1 << 28);
                }
            }
            0xB => {
                // Reset eax, ebx, ecx; EDX is the x2APIC ID of the current logical processor.
                entry.eax = 0;
                entry.ebx = 0;
                entry.ecx = 0;
                entry.edx = u32::from(cpu_index);
                entry.flags = KVM_CPUID_FLAG_SIGNIFCANT_INDEX;

                match entry.index {
                    // Logical processor domain: no SMT, so 0 bits to shift right to get
                    // to the core domain, 1 logical processor per core, domain type 1.
                    0 => {
                        set_range(&mut entry.eax, 0, 4, 0);
                        set_range(&mut entry.ebx, 0, 15, 1);
                        set_range(&mut entry.ecx, 8, 15, 1);
                    }
                    // Core domain: the next higher-scoped domain (socket) includes all
                    // logical processors; domain type 2.
                    1 => {
                        set_range(
                            &mut entry.eax,
                            0,
                            4,
                            MAX_SUPPORTED_VCPUS.next_power_of_two().ilog2(),
                        );
                        set_range(&mut entry.ebx, 0, 15, u32::from(cpu_count));
                        set_range(&mut entry.ecx, 0, 7, entry.index);
                        set_range(&mut entry.ecx, 8, 15, 2);
                    }
                    // KVM no longer returns any subleaves greater than 1 on supported
                    // kernels; leave the input ECX in place like Firecracker does.
                    index => entry.ecx = index,
                }
            }
            _ => (),
        }
    }

    Ok(())
}

/// Creates and populates required MSR entries for booting Linux on X86_64.
fn create_boot_msr_entries() -> Vec<kvm_msr_entry> {
    let msr_entry_default = |msr| kvm_msr_entry {
        index: msr,
        data: 0x0,
        ..Default::default()
    };

    vec![
        msr_entry_default(MSR_IA32_SYSENTER_CS),
        msr_entry_default(MSR_IA32_SYSENTER_ESP),
        msr_entry_default(MSR_IA32_SYSENTER_EIP),
        // x86_64 specific msrs, we only run on x86_64 not x86.
        msr_entry_default(MSR_STAR),
        msr_entry_default(MSR_CSTAR),
        msr_entry_default(MSR_KERNEL_GS_BASE),
        msr_entry_default(MSR_SYSCALL_MASK),
        msr_entry_default(MSR_LSTAR),
        // end of x86_64 specific code
        msr_entry_default(MSR_IA32_TSC),
        kvm_msr_entry {
            index: MSR_IA32_MISC_ENABLE,
            data: u64::from(MSR_IA32_MISC_ENABLE_FAST_STRING),
            ..Default::default()
        },
        // set default memory type for physical memory outside configured
        // memory ranges to write-back by setting MTRR enable bit (11) and
        // setting memory type to write-back (value 6).
        // https://wiki.osdev.org/MTRR
        kvm_msr_entry {
            index: MSR_MTRR_DEF_TYPE,
            data: (1 << 11) | 0x6,
            ..Default::default()
        },
    ]
}

/// Configure Model Specific Registers (MSRs) required to boot Linux for a given x86_64 vCPU.
fn set_msrs(vcpu: &VcpuFd, msr_entries: &[kvm_msr_entry]) -> Result<(), VcpuConfigureError> {
    let msrs = Msrs::from_entries(msr_entries)?;
    vcpu.set_msrs(&msrs)
        .map_err(VcpuConfigureError::SetMsrs)
        .and_then(|msrs_written| {
            if msrs_written == msrs.as_fam_struct_ref().nmsrs as usize {
                Ok(())
            } else {
                Err(VcpuConfigureError::SetMsrsIncomplete)
            }
        })
}
