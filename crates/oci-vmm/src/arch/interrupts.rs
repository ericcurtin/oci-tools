// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Portions Copyright 2017 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the THIRD-PARTY file.
// Ported into oci-vmm from Firecracker (src/vmm/src/arch/x86_64/interrupts.rs), trimmed of metrics/snapshot/ACPI.

//! Boot-time LAPIC configuration: routes LINT0 to ExtINT and LINT1 to NMI.

use kvm_bindings::kvm_lapic_state;
use kvm_ioctls::VcpuFd;

/// Errors thrown while configuring the LAPIC.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum InterruptError {
    /// Failure in getting the LAPIC configuration.
    #[error("Failure in getting the LAPIC configuration: {0}")]
    GetLapic(kvm_ioctls::Error),
    /// Failure in setting the LAPIC configuration.
    #[error("Failure in setting the LAPIC configuration: {0}")]
    SetLapic(kvm_ioctls::Error),
}

// Defines poached from apicdef.h kernel header.
const APIC_LVT0: usize = 0x350;
const APIC_LVT1: usize = 0x360;
const APIC_MODE_NMI: u32 = 0x4;
const APIC_MODE_EXTINT: u32 = 0x7;

fn get_klapic_reg(klapic: &kvm_lapic_state, reg_offset: usize) -> u32 {
    let reg = klapic
        .regs
        .get(reg_offset..reg_offset + 4)
        .expect("get_klapic_reg range");
    let mut bytes = [0u8; 4];
    for (byte, reg) in bytes.iter_mut().zip(reg.iter()) {
        *byte = *reg as u8;
    }
    u32::from_le_bytes(bytes)
}

fn set_klapic_reg(klapic: &mut kvm_lapic_state, reg_offset: usize, value: u32) {
    let reg = klapic
        .regs
        .get_mut(reg_offset..reg_offset + 4)
        .expect("set_klapic_reg range");
    for (reg, byte) in reg.iter_mut().zip(value.to_le_bytes()) {
        #[allow(clippy::cast_possible_wrap)]
        {
            *reg = byte as i8;
        }
    }
}

fn set_apic_delivery_mode(reg: u32, mode: u32) -> u32 {
    ((reg) & !0x700) | ((mode) << 8)
}

/// Configures LAPICs. LAPIC0 is set for external interrupts, LAPIC1 is set for NMI.
///
/// # Arguments
/// * `vcpu` - The VCPU object to configure.
pub fn set_lint(vcpu: &VcpuFd) -> Result<(), InterruptError> {
    let mut klapic = vcpu.get_lapic().map_err(InterruptError::GetLapic)?;

    let lvt_lint0 = get_klapic_reg(&klapic, APIC_LVT0);
    set_klapic_reg(
        &mut klapic,
        APIC_LVT0,
        set_apic_delivery_mode(lvt_lint0, APIC_MODE_EXTINT),
    );
    let lvt_lint1 = get_klapic_reg(&klapic, APIC_LVT1);
    set_klapic_reg(
        &mut klapic,
        APIC_LVT1,
        set_apic_delivery_mode(lvt_lint1, APIC_MODE_NMI),
    );

    vcpu.set_lapic(&klapic).map_err(InterruptError::SetLapic)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_set_and_get_klapic_reg() {
        let reg_offset = 0x340;
        let mut klapic = kvm_lapic_state::default();
        set_klapic_reg(&mut klapic, reg_offset, 3);
        let value = get_klapic_reg(&klapic, reg_offset);
        assert_eq!(value, 3);
    }

    #[test]
    fn test_apic_delivery_mode() {
        assert_eq!(set_apic_delivery_mode(0xffff_ffff, 2), 0xffff_faff);
        assert_eq!(set_apic_delivery_mode(0, 0x7), 0x700);
    }
}
