// SPDX-License-Identifier: Apache-2.0

//! `ocivmm`'s macOS/aarch64 backend: raw `Hypervisor.framework`
//! bindings ([`sys`]) plus a thin, safe [`vm::Vm`]/[`vcpu::Vcpu`]
//! layer on top -- this crate's second, independent hypervisor
//! backend alongside the KVM/x86_64 one (`crate::vstate`), sharing no
//! code with it (different API, different architecture entirely).
//!
//! This module started as phase 2 of `docs/design/0249-ocivmm-macos-
//! aarch64.md` (VM/vCPU creation, register access, running a guest
//! instruction and observing its exit -- see the smoke test at the
//! bottom of this file, which really executes this on real Apple
//! Silicon hardware, not just compiles it) and is growing into phase
//! 3 (arm64 boot + GIC + console): [`mmio`]'s Data-Abort trap-and-
//! emulate is the mechanism the PL011 console (and, phase 4,
//! virtio-mmio) both need, since AArch64 has no port I/O for the
//! architecture to trap directly the way x86_64 does. See that design
//! note for the full plan and current status.
//!
//! ## Entitlement required
//! Every test/binary that actually calls into this module must be
//! codesigned with the `com.apple.security.hypervisor` entitlement
//! first (`ci/codesign-ocivmm.sh` /
//! `packaging/macos/ocivmm.entitlements`) -- otherwise `Vm::create`
//! fails with `HvError::Denied`, even running as root.
//!
//! ## Real-hardware tests are `#[ignore]`d
//! Every unit test that actually calls `Vm::create` is `#[ignore]`d
//! (run them with `cargo test ... -- --ignored --test-threads=1` on
//! real Apple Silicon hardware, after codesigning per above) -- not
//! only for the entitlement reason above, but because GitHub-hosted
//! macOS CI runners have no `hv_support` at all, on any macOS version
//! (confirmed directly, see `.github/workflows/ci.yml`'s own
//! `hvf-build` job and `docs/design/0249`'s phase 7), so no amount of
//! codesigning would ever make them pass there. `--test-threads=1`
//! specifically (not just leaving it to the default parallel runner)
//! matters here too: `Vm::create` enforces one real VM per process at
//! a time (see `vm::Vm`'s own docs) with a hard, loud error if a
//! second call races the first, rather than a hang or silent reuse --
//! exactly what running these tests in parallel would trigger.

pub mod boot;
pub mod error;
pub mod fdt;
pub mod gic;
pub mod layout;
pub mod machine;
pub mod mmio;
pub mod pl011;
pub mod sys;
pub mod sysreg_trap;
pub mod vcpu;
pub mod virtio_blk;
pub mod virtio_mmio;
pub mod vm;

pub use boot::{ImageError, ImageHeader, load_image, unwrap_efi_zboot};
pub use error::HvError;
pub use fdt::FdtWriter;
pub use gic::GicLayout;
pub use mmio::{DataAbort, MmioDevice, MmioError};
pub use pl011::Pl011;
pub use sysreg_trap::Sys64Trap;
pub use vcpu::{Exception, ExitReason, Vcpu};
pub use virtio_blk::VirtioBlkMmio;
pub use virtio_mmio::VirtioMmioTransport;
pub use vm::Vm;

#[cfg(test)]
#[allow(unsafe_code)] // mmap + Vm::map: safety documented at each call site below.
mod tests {
    //! Proves the FFI layer end to end: create a VM, map one
    //! page of host memory holding two raw AArch64 instructions,
    //! create a vCPU, point it at that page, run it, and check
    //! that the guest's own `hvc #0` produced exactly the exit this
    //! module claims it would.
    //!
    //! `hvc #0` (not e.g. `wfi`) is used as the deterministic
    //! trap-to-host instruction: an EL1 guest's `HVC` always traps to
    //! EL2 unless the hypervisor explicitly disables it
    //! (`HCR_EL2.HCD`), which `Hypervisor.framework` doesn't do by
    //! default -- the same idiom used to bootstrap most third-party
    //! arm64 Hypervisor.framework VMMs.
    //!
    //! Running this test requires the compiled test binary itself be
    //! codesigned with the hypervisor entitlement first -- `cargo
    //! test` alone does not do this. See `ci/codesign-ocivmm.sh`; for
    //! local iteration:
    //! ```sh
    //! cargo test -p oci-vmm --lib --no-run
    //! bin=$(cargo test -p oci-vmm --lib --no-run --message-format=json \
    //!   | jq -r 'select(.profile.test == true) | .filenames[0]')
    //! ci/codesign-ocivmm.sh "$bin"
    //! "$bin" hvf::tests::vcpu_runs_one_instruction_and_exits_via_hvc --nocapture
    //! ```

    use crate::hvf::sys::{HV_REG_CPSR, HV_REG_PC};
    use crate::hvf::vcpu::{ExitReason, Vcpu};
    use crate::hvf::vm::Vm;

    /// `hvc #0`, little-endian.
    const HVC_0: [u8; 4] = 0xd400_0002_u32.to_le_bytes();
    /// `b .` (branch to self) -- never reached; a defensive fallback
    /// in case some future macOS build stops trapping bare `hvc`, so
    /// this test hangs (visibly, in a debugger/timeout) instead of
    /// silently executing past guest memory it doesn't own.
    const B_SELF: [u8; 4] = 0x1400_0000_u32.to_le_bytes();

    /// `CPSR` value selecting AArch64 EL1h with all four DAIF
    /// exception-mask bits set -- the same reset state a real boot
    /// path (phase 3) will also need to establish before entering
    /// guest code.
    const CPSR_EL1H_MASKED: u64 = 0x3c5;

    /// The EC (exception class) field of `ESR_ELx`, bits [31:26]:
    /// `0b010110` for "HVC instruction execution in AArch64 state".
    const ESR_EC_HVC64: u64 = 0x16;

    #[test]
    #[ignore = "needs real Hypervisor.framework hardware support (hv_vm_create) plus this test \
                binary codesigned with the com.apple.security.hypervisor entitlement -- run \
                locally on real Apple Silicon (ci/codesign-ocivmm.sh, then `cargo test ... -- \
                --ignored --test-threads=1`); GitHub-hosted macOS runners have no hv_support at \
                all on any macOS version, so this can never pass there regardless of signing -- \
                see docs/design/0249 phase 7"]
    fn vcpu_runs_one_instruction_and_exits_via_hvc() {
        // Apple Silicon's own host page size is 16 KiB, not the
        // 4 KiB one might assume -- confirmed directly on this
        // hardware (`sysctl hw.pagesize`) -- and hv_vm_map requires
        // both the host address and size to already be page-aligned
        // multiples of it (`HV_BAD_ARGUMENT` otherwise, hit while
        // developing this test against a hardcoded 4 KiB assumption).
        const PAGE_SIZE: usize = 16384;
        const GUEST_ADDR: u64 = PAGE_SIZE as u64;

        // SAFETY: a fresh, anonymous, private mapping -- not backed
        // by a file, not shared with any other process -- sized and
        // aligned to a full page (mmap's own guarantee for anonymous
        // mappings), which is exactly what `Vm::map`'s safety
        // contract requires.
        //
        // PROT_READ | PROT_WRITE only, deliberately no PROT_EXEC: the
        // *host* mapping is never executed by this process (macOS
        // enforces W^X on the host's own executable pages and denies
        // an RWX anonymous mmap outright, confirmed directly on this
        // hardware); the guest's execute permission is a completely
        // separate stage-2 permission, granted below via
        // `Vm::map`'s own `exec` flag, not this host-side mapping.
        let host_ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                PAGE_SIZE,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };
        assert_ne!(
            host_ptr,
            libc::MAP_FAILED,
            "mmap failed: {}",
            std::io::Error::last_os_error()
        );
        let host_ptr = host_ptr.cast::<u8>();

        // SAFETY: `host_ptr` points at `PAGE_SIZE` freshly mmap'd,
        // writable bytes; the two 4-byte instruction slices fit
        // entirely within the first 8 of them.
        unsafe {
            host_ptr.copy_from_nonoverlapping(HVC_0.as_ptr(), 4);
            host_ptr.add(4).copy_from_nonoverlapping(B_SELF.as_ptr(), 4);
        }

        let vm = Vm::create().expect("hv_vm_create (codesigned with the hypervisor entitlement?)");

        // SAFETY: `host_ptr`/`PAGE_SIZE` describe the mmap above,
        // which outlives this whole test (never munmap'd, leaked
        // deliberately -- a four-KiB one-time leak in a test).
        unsafe {
            vm.map(host_ptr, GUEST_ADDR, PAGE_SIZE, true, true, true)
                .expect("hv_vm_map");
        }

        let vcpu = Vcpu::create(&vm).expect("hv_vcpu_create");
        vcpu.set_reg(HV_REG_PC, GUEST_ADDR).expect("set PC");
        vcpu.set_reg(HV_REG_CPSR, CPSR_EL1H_MASKED)
            .expect("set CPSR");

        let reason = vcpu.run().expect("hv_vcpu_run");
        match reason {
            ExitReason::Exception(exception) => {
                let ec = (exception.syndrome >> 26) & 0x3f;
                assert_eq!(
                    ec, ESR_EC_HVC64,
                    "expected an HVC exception (EC={ESR_EC_HVC64:#x}), got ESR_ELx={:#x} (EC={ec:#x})",
                    exception.syndrome
                );
            }
            other => {
                panic!("expected ExitReason::Exception(..) from the guest's hvc, got {other:?}")
            }
        }

        // `hvc` is architecturally a synchronous call, not a fault:
        // the reported PC is the *return* address (the instruction
        // after the `hvc`), matching real AArch64 exception-level
        // semantics -- confirmed directly here rather than assumed.
        // It should be exactly one instruction past the `hvc`, not
        // however far into (or past) the `b .` safety net.
        let pc = vcpu.get_reg(HV_REG_PC).expect("get PC");
        assert_eq!(
            pc,
            GUEST_ADDR + 4,
            "PC should point one instruction past the hvc (its return address)"
        );
    }
}
