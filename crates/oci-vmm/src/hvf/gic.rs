// SPDX-License-Identifier: Apache-2.0

//! The GICv3 interrupt controller.
//!
//! Unlike x86_64/KVM (`crate::arch::interrupts`, `crate::vstate::vm`'s
//! own in-kernel PIC/IOAPIC + GSI routing table this crate builds up
//! device by device), Hypervisor.framework provides the whole GIC as
//! a single built-in device: once [`create`] configures its
//! distributor/redistributor base addresses, the guest's own
//! accesses to those regions are handled by the framework directly --
//! no `hvf::mmio` trap-and-emulate, no per-register state this crate
//! owns, the way [`crate::hvf::pl011`] needs. This module is only
//! responsible for the one-time setup: querying the framework's own
//! required sizes/alignments, creating the device before any vCPU
//! exists (a documented ordering requirement -- GIC CPU-interface
//! resources are allocated per vCPU at vCPU-creation time), and
//! reporting the region layout back so a device tree can describe it.
//!
//! `hv_gic_set_spi`/interrupt injection aren't wrapped yet: nothing
//! in this backend raises a guest interrupt so far (see
//! `crate::hvf::pl011`'s own module docs on why it doesn't need to).

use crate::hvf::error::{HvError, check};
use crate::hvf::sys;
use crate::hvf::vm::Vm;

/// The guest-physical layout of a created GIC's distributor and
/// redistributor regions -- everything a device tree's
/// `interrupt-controller` node needs to describe it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GicLayout {
    /// Guest physical base address of the distributor region (as
    /// passed to [`create`]).
    pub distributor_base: u64,
    /// The distributor region's fixed size, in bytes
    /// (`hv_gic_get_distributor_size`).
    pub distributor_size: u64,
    /// Guest physical base address of the redistributor region (as
    /// passed to [`create`]).
    pub redistributor_base: u64,
    /// The redistributor region's total size, in bytes -- every
    /// vCPU's own frame, contiguously
    /// (`hv_gic_get_redistributor_region_size`).
    pub redistributor_size: u64,
}

/// Creates the VM's one GICv3 device, with the distributor and
/// redistributor regions based at `distributor_base`/
/// `redistributor_base` (each must already satisfy the framework's
/// own alignment requirements -- queried here and checked with a
/// `debug_assert`, since a release build would just get a clear
/// `HvError::BadArgument` from `hv_gic_create` itself instead).
///
/// Must be called after `vm` was created and before any `Vcpu` is
/// created against it (enforced by the framework itself, not by this
/// function -- an out-of-order call surfaces as an ordinary
/// `HvError`).
#[allow(unsafe_code)] // Every call here is documented at the point it happens.
pub fn create(
    _vm: &Vm,
    distributor_base: u64,
    redistributor_base: u64,
) -> Result<GicLayout, HvError> {
    let mut distributor_size: usize = 0;
    let mut distributor_align: usize = 0;
    let mut redistributor_size: usize = 0;
    let mut redistributor_align: usize = 0;

    // SAFETY: each out-parameter is a valid, uniquely-owned pointer
    // to a local for the duration of its call; none of these take a
    // guest-memory or vCPU argument at all.
    unsafe {
        check(sys::hv_gic_get_distributor_size(&mut distributor_size))?;
        check(sys::hv_gic_get_distributor_base_alignment(
            &mut distributor_align,
        ))?;
        check(sys::hv_gic_get_redistributor_region_size(
            &mut redistributor_size,
        ))?;
        check(sys::hv_gic_get_redistributor_base_alignment(
            &mut redistributor_align,
        ))?;
    }

    debug_assert_eq!(
        distributor_base % distributor_align as u64,
        0,
        "distributor base must be aligned to {distributor_align:#x}"
    );
    debug_assert_eq!(
        redistributor_base % redistributor_align as u64,
        0,
        "redistributor base must be aligned to {redistributor_align:#x}"
    );

    // SAFETY: `hv_gic_config_create` returns a fresh, owned object;
    // this function passes it to exactly the calls its own header
    // documents (set the two base addresses, then hand it to
    // `hv_gic_create`, which consumes it), all on the object's own
    // very first (and only) use. It is deliberately never
    // `os_release`d -- see `hvf::sys`'s own note on why -- a
    // one-time, process-lifetime leak, the same tradeoff already
    // documented for the mmap'd guest-memory pages in this module's
    // own tests.
    let config = unsafe { sys::hv_gic_config_create() };
    if config.is_null() {
        return Err(HvError::NoResources);
    }

    unsafe {
        check(sys::hv_gic_config_set_distributor_base(
            config,
            distributor_base,
        ))?;
        check(sys::hv_gic_config_set_redistributor_base(
            config,
            redistributor_base,
        ))?;
        check(sys::hv_gic_create(config))?;
    }

    Ok(GicLayout {
        distributor_base,
        distributor_size: distributor_size as u64,
        redistributor_base,
        redistributor_size: redistributor_size as u64,
    })
}

/// Asserts (or, for a level-triggered interrupt, deasserts) SPI
/// `intid` (a full GIC INTID, i.e. already offset by
/// [`crate::hvf::layout::GIC_SPI_BASE`], not a bare SPI index).
#[allow(unsafe_code)] // hv_gic_set_spi takes no guest-memory/vCPU argument at all.
pub fn set_spi(intid: u32, level: bool) -> Result<(), HvError> {
    check(unsafe { sys::hv_gic_set_spi(intid, level) })
}

#[cfg(test)]
mod tests {
    //! Run for real on Apple Silicon hardware (requires the
    //! hypervisor entitlement, same as every other `hvf` hardware
    //! test -- see `ci/codesign-ocivmm.sh`).

    use super::*;
    use crate::hvf::sys::HV_SYS_REG_MPIDR_EL1;
    use crate::hvf::vcpu::Vcpu;

    /// 1 MiB: generously larger than either region's real size
    /// (a distributor is a few KiB; a single-vCPU redistributor
    /// region is two 64 KiB frames) and its real alignment
    /// requirement, so this test doesn't need to hardcode either.
    const ONE_MIB: u64 = 1 << 20;

    #[test]
    #[ignore = "needs real Hypervisor.framework hardware support (hv_vm_create) plus this test \
                binary codesigned with the com.apple.security.hypervisor entitlement -- run \
                locally on real Apple Silicon (ci/codesign-ocivmm.sh, then `cargo test ... -- \
                --ignored --test-threads=1`); GitHub-hosted macOS runners have no hv_support at \
                all on any macOS version, so this can never pass there regardless of signing -- \
                see docs/design/0249 phase 7"]
    fn create_gic_then_a_vcpu_with_affinity_set() {
        let vm = Vm::create().expect("hv_vm_create");

        let layout = create(&vm, ONE_MIB, 2 * ONE_MIB).expect("hv_gic_create");
        assert_eq!(layout.distributor_base, ONE_MIB);
        assert_eq!(layout.redistributor_base, 2 * ONE_MIB);
        assert!(layout.distributor_size > 0);
        assert!(layout.redistributor_size > 0);

        // hv_gic_create's own docs: vCPUs must be created *after* the
        // GIC, and must set their MPIDR_EL1 affinity before running.
        let vcpu = Vcpu::create(&vm).expect("hv_vcpu_create (after hv_gic_create)");
        const AFFINITY_0: u64 = 0x8000_0000; // bit 31 (RES1) | Aff0..3 = 0.
        vcpu.set_sys_reg(HV_SYS_REG_MPIDR_EL1, AFFINITY_0)
            .expect("set MPIDR_EL1");
        assert_eq!(vcpu.get_sys_reg(HV_SYS_REG_MPIDR_EL1).unwrap(), AFFINITY_0);
    }
}
