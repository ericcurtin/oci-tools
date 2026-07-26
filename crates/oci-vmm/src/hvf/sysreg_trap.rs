// SPDX-License-Identifier: Apache-2.0

//! Handles `EC == 0x18` ("Trapped MSR, MRS, or System instruction
//! execution") exits: `hv_vcpu_set_trap_debug_reg_accesses(false)`
//! (called once, at vCPU setup) stops the framework from trapping the
//! specific debug registers its own docs name (`DBGBCRn_EL1` and
//! friends, `MDSCR_EL1`), but not every debug-adjacent system
//! register -- found the hard way booting a real kernel: every stock
//! distro kernel's own debug-monitor bring-up
//! (`kernel/debug/debug-monitors.c: reset_ctrl_regs`) unconditionally
//! writes `OSDLR_EL1` (the "OS Double Lock" register) during early
//! boot, and that specific access still traps regardless.
//!
//! This backend emulates none of the ARM debug architecture at all
//! (no `hvf::mmio`-style device model backs any of these registers),
//! so the only reasonable emulation is the trivial one: accept
//! writes without storing them anywhere (nothing reads this state
//! back through this path), answer reads with `0` and log that a
//! register this module didn't expect was read (so a future, real
//! need to answer one for real is visible rather than silently wrong)
//! -- then advance `PC` past the trapping instruction, the same as
//! `hvf::mmio`'s own Data Abort handling (a "Trapped System
//! instruction" exit's own reported `PC` is likewise the trapping
//! instruction's own address, not a return address, confirmed
//! directly while developing this against a real kernel boot).

use crate::hvf::error::HvError;
use crate::hvf::sys::{self, hv_reg_t};
use crate::hvf::vcpu::{Exception, Vcpu};

/// A decoded "Trapped System instruction" syndrome (`ESR_ELx.ISS`,
/// `EC == 0x18`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sys64Trap {
    /// The system register's `op0` field.
    pub op0: u8,
    /// The system register's `op1` field.
    pub op1: u8,
    /// The system register's `CRn` field.
    pub crn: u8,
    /// The system register's `CRm` field.
    pub crm: u8,
    /// The system register's `op2` field.
    pub op2: u8,
    /// The transfer register index (`0..=31`; `31` is `XZR`).
    pub rt: u32,
    /// `true` for `MRS` (read), `false` for `MSR` (write).
    pub read: bool,
}

/// The EC value for a "Trapped MSR, MRS, or System instruction
/// execution" exception (any encoding not covered by one of the
/// other, more specific EC values `0x00`-`0x17`).
pub const ESR_EC_SYS64: u64 = 0x18;

impl Sys64Trap {
    /// Decodes `syndrome` as a Sys64 trap. Returns `None` if this
    /// isn't a `EC == 0x18` exception at all.
    pub fn decode(syndrome: u64) -> Option<Self> {
        let ec = (syndrome >> 26) & 0x3f;
        if ec != ESR_EC_SYS64 {
            return None;
        }
        let iss = syndrome & 0x01ff_ffff;
        Some(Sys64Trap {
            op0: ((iss >> 20) & 0x3) as u8,
            op1: ((iss >> 14) & 0x7) as u8,
            crn: ((iss >> 10) & 0xf) as u8,
            crm: ((iss >> 1) & 0xf) as u8,
            op2: ((iss >> 17) & 0x7) as u8,
            rt: u32::try_from((iss >> 5) & 0x1f).unwrap(),
            read: (iss & 1) == 1,
        })
    }
}

/// Handles a trapped system register access with the trivial policy
/// documented at the top of this module (accept writes, answer reads
/// with `0`), then advances `PC` past it.
pub fn emulate(vcpu: &Vcpu, exception: &Exception) -> Result<(), HvError> {
    let trap = Sys64Trap::decode(exception.syndrome)
        .expect("emulate() called for a non-Sys64Trap exception");

    if trap.read && trap.rt != 31 {
        tracing::debug!(
            "hvf::sysreg_trap: unhandled MRS read (op0={} op1={} CRn={} CRm={} op2={}), \
             answering 0",
            trap.op0,
            trap.op1,
            trap.crn,
            trap.crm,
            trap.op2
        );
        vcpu.set_reg(reg_id(trap.rt), 0)?;
    }

    let pc = vcpu.get_reg(sys::HV_REG_PC)?;
    vcpu.set_reg(sys::HV_REG_PC, pc + 4)?;
    Ok(())
}

fn reg_id(rt: u32) -> hv_reg_t {
    sys::hv_reg_x(rt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_osdlr_el1_write() {
        // The real ESR_ELx value hit booting a real kernel: op0=2,
        // op1=0, CRn=1, CRm=3, op2=4 (OSDLR_EL1), Rt=31 (XZR), write.
        let syndrome = 0x622807e6_u64;
        let trap = Sys64Trap::decode(syndrome).unwrap();
        assert_eq!(trap.op0, 2);
        assert_eq!(trap.op1, 0);
        assert_eq!(trap.crn, 1);
        assert_eq!(trap.crm, 3);
        assert_eq!(trap.op2, 4);
        assert_eq!(trap.rt, 31);
        assert!(!trap.read);
    }

    #[test]
    fn rejects_a_non_sys64_ec() {
        // EC bits (31:26) = 0x25 ("Data Abort... without a change in
        // Exception level"), not 0x18 -- arbitrary ISS otherwise.
        let syndrome = 0x9600_0000_u64;
        let ec = (syndrome >> 26) & 0x3f;
        assert_ne!(ec, ESR_EC_SYS64);
        assert_eq!(Sys64Trap::decode(syndrome), None);
    }
}
