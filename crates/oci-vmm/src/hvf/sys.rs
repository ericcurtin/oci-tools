// SPDX-License-Identifier: Apache-2.0

#![allow(non_camel_case_types)] // Mirrors the C API's own naming exactly, on purpose.
#![allow(unsafe_code)] // The whole file is the FFI boundary onto Hypervisor.framework itself.

//! Raw `extern "C"` bindings against Apple's `Hypervisor.framework`
//! (arm64 surface only -- the framework's x86_64 surface, `hv.h`, is a
//! different, older API this module doesn't touch). Hand-written
//! directly against the real headers shipped in the macOS SDK
//! (`Hypervisor/hv_vm.h`, `hv_vcpu.h`, `hv_vcpu_types.h`,
//! `hv_vm_types.h`, `arm64/hv/hv_kern_types.h`) rather than generated,
//! scoped to exactly the calls `hvf::vm`/`hvf::vcpu` use -- see
//! `docs/design/0249-ocivmm-macos-aarch64.md`.
//!
//! No existing, widely-used Rust crate wraps this framework at
//! register-level fidelity, so `oci-vmm` owns these bindings the same
//! way it owns its KVM ioctl usage on the other backend.

use std::ffi::c_void;

/// `hv_return_t`: `typedef mach_error_t hv_return_t;` and `mach_error_t`
/// is `kern_return_t`, a plain `int`.
pub type hv_return_t = i32;

/// `HV_SUCCESS` -- the only non-error `hv_return_t` value.
pub const HV_SUCCESS: hv_return_t = 0;

/// `hv_vcpu_t`: `typedef uint64_t hv_vcpu_t;` -- an opaque vCPU instance
/// ID scoped to the calling thread, not a file descriptor.
pub type hv_vcpu_t = u64;

/// `hv_ipa_t`/`hv_gpaddr_t`: a guest physical (intermediate physical)
/// address.
pub type hv_ipa_t = u64;

/// `hv_reg_t`: `OS_ENUM(hv_reg, uint32_t, ...)` -- general-purpose and
/// PC/FPCR/FPSR/CPSR register IDs for `hv_vcpu_{get,set}_reg`.
pub type hv_reg_t = u32;

/// `hv_sys_reg_t`: `OS_ENUM(hv_sys_reg, uint16_t, ...)` -- AArch64
/// system register IDs for `hv_vcpu_{get,set}_sys_reg`.
pub type hv_sys_reg_t = u16;

/// `hv_exit_reason_t`: `OS_ENUM(hv_exit_reason, uint32_t, ...)`.
pub type hv_exit_reason_t = u32;

/// `HV_EXIT_REASON_CANCELED`: asynchronous exit requested explicitly.
pub const HV_EXIT_REASON_CANCELED: hv_exit_reason_t = 0;
/// `HV_EXIT_REASON_EXCEPTION`: synchronous exception taken to a higher
/// EL, triggered by the guest (traps, `hvc`/`smc`, MMIO/data aborts on
/// unmapped IPAs, `wfi`/`wfe`, ...).
pub const HV_EXIT_REASON_EXCEPTION: hv_exit_reason_t = 1;
/// `HV_EXIT_REASON_VTIMER_ACTIVATED`: the ARM generic virtual timer
/// became pending.
pub const HV_EXIT_REASON_VTIMER_ACTIVATED: hv_exit_reason_t = 2;
/// `HV_EXIT_REASON_UNKNOWN`: "this should not happen under normal
/// operation" (the framework's own doc comment).
pub const HV_EXIT_REASON_UNKNOWN: hv_exit_reason_t = 3;

/// A subset of `hv_reg_t`'s enumerators -- the ones `hvf::vcpu` sets
/// during boot setup (general-purpose argument/entry registers, the
/// program counter, and CPSR). `X0`..`X30`'s numeric values are their
/// own register index (`HV_REG_X0 = 0`, ..., `HV_REG_X30 = 30`); `PC`,
/// `FPCR`, `FPSR`, `CPSR` follow immediately after in that header
/// order.
pub const HV_REG_X0: hv_reg_t = 0;
/// `PC`: the program counter, `HV_REG_X30 + 1`.
pub const HV_REG_PC: hv_reg_t = 31;
/// `CPSR`: the current program status register, `HV_REG_PC + 3`
/// (after `FPCR`/`FPSR`).
pub const HV_REG_CPSR: hv_reg_t = 34;

/// `HV_SYS_REG_MPIDR_EL1`: the register `hv_gic_create`'s own docs
/// say a vCPU must have its affinity set in before running, once a
/// GIC device exists (GICv3 uses affinity-based interrupt routing).
/// Framework-managed: writing it through `hv_vcpu_set_sys_reg` is the
/// documented mechanism, even though real hardware's `MPIDR_EL1` is
/// architecturally read-only from EL1 (the actual backing register is
/// EL2's `VMPIDR_EL2`, invisible to this API).
pub const HV_SYS_REG_MPIDR_EL1: hv_sys_reg_t = 0xc005;

/// The general-purpose register `HV_REG_X0 + n` (`n` in `0..=30`).
/// `hv_reg_t`'s `X0`..`X30` enumerators are contiguous, so this is
/// exactly what the framework's own headers do too (`HV_REG_FP =
/// HV_REG_X29`, `HV_REG_LR = HV_REG_X30`); there is no `HV_REG_X31`
/// enumerator at all -- register index 31 in an instruction encoding
/// means the zero register (`XZR`)/stack pointer depending on
/// context, never a real, readable `hv_reg_t`, and callers (e.g.
/// `hvf::mmio`'s data-abort decode) must special-case it themselves.
pub fn hv_reg_x(n: u32) -> hv_reg_t {
    debug_assert!(n <= 30, "x{n} is not a valid general-purpose register");
    HV_REG_X0 + n
}

/// Memory permission flags for `hv_vm_map`/`hv_vm_protect`
/// (`hv_memory_flags_t`, a `uint64_t` bitmask on arm64).
pub const HV_MEMORY_READ: u64 = 1 << 0;
/// Write permission.
pub const HV_MEMORY_WRITE: u64 = 1 << 1;
/// Execute permission.
pub const HV_MEMORY_EXEC: u64 = 1 << 2;

/// `hv_vcpu_exit_exception_t`: details of a synchronous exception exit
/// (`ESR_ELx`, `FAR_ELx`, and the faulting IPA, when applicable).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct hv_vcpu_exit_exception_t {
    /// `ESR_ELx` at the time of the exception.
    pub syndrome: u64,
    /// `FAR_ELx` at the time of the exception.
    pub virtual_address: u64,
    /// The faulting guest physical address, when this exception is a
    /// stage-2 (IPA) fault.
    pub physical_address: hv_ipa_t,
}

/// `hv_vcpu_exit_t`: written by the framework at a stable address
/// obtained from `hv_vcpu_create`'s own out-parameter, refreshed by
/// every `hv_vcpu_run` call on the owning thread.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct hv_vcpu_exit_t {
    /// Why the vCPU last exited back to the host.
    pub reason: hv_exit_reason_t,
    /// Populated when `reason == HV_EXIT_REASON_EXCEPTION`.
    pub exception: hv_vcpu_exit_exception_t,
}

#[link(name = "Hypervisor", kind = "framework")]
unsafe extern "C" {
    /// Creates the VM instance for the current process. `config` may
    /// be a null `hv_vm_config_t` for the default configuration --
    /// modeled here as `*mut c_void` since it's an opaque,
    /// ARC-managed-or-plain-pointer object type this crate never
    /// constructs (always passes null).
    pub fn hv_vm_create(config: *mut c_void) -> hv_return_t;

    /// Destroys the VM instance associated with the current process.
    /// Requires all vCPUs to have been destroyed first.
    pub fn hv_vm_destroy() -> hv_return_t;

    /// Maps `size` bytes at host virtual address `addr` (page-aligned)
    /// into the guest physical address space at `ipa` (page-aligned),
    /// with the given `HV_MEMORY_*` permission flags.
    pub fn hv_vm_map(addr: *mut c_void, ipa: hv_ipa_t, size: usize, flags: u64) -> hv_return_t;

    /// Creates a vCPU instance for the *current thread*. `exit` is an
    /// out-parameter: the framework writes a pointer to its own
    /// `hv_vcpu_exit_t`, valid for the vCPU's lifetime and refreshed
    /// by every `hv_vcpu_run` -- not a buffer the caller allocates.
    /// `config` may be null for the default configuration.
    pub fn hv_vcpu_create(
        vcpu: *mut hv_vcpu_t,
        exit: *mut *mut hv_vcpu_exit_t,
        config: *mut c_void,
    ) -> hv_return_t;

    /// Destroys the vCPU instance. Must be called by the owning
    /// thread.
    pub fn hv_vcpu_destroy(vcpu: hv_vcpu_t) -> hv_return_t;

    /// Reads a general/PC/FPCR/FPSR/CPSR register. Must be called by
    /// the owning thread.
    pub fn hv_vcpu_get_reg(vcpu: hv_vcpu_t, reg: hv_reg_t, value: *mut u64) -> hv_return_t;

    /// Writes a general/PC/FPCR/FPSR/CPSR register. Must be called by
    /// the owning thread.
    pub fn hv_vcpu_set_reg(vcpu: hv_vcpu_t, reg: hv_reg_t, value: u64) -> hv_return_t;

    /// Reads an AArch64 system register. Must be called by the
    /// owning thread.
    pub fn hv_vcpu_get_sys_reg(vcpu: hv_vcpu_t, reg: hv_sys_reg_t, value: *mut u64) -> hv_return_t;

    /// Writes an AArch64 system register. Must be called by the
    /// owning thread.
    pub fn hv_vcpu_set_sys_reg(vcpu: hv_vcpu_t, reg: hv_sys_reg_t, value: u64) -> hv_return_t;

    /// Runs the vCPU until the next exit. Blocks the calling thread.
    /// Must be called by the owning thread.
    pub fn hv_vcpu_run(vcpu: hv_vcpu_t) -> hv_return_t;

    // -- GIC (`Hypervisor/hv_gic.h`, `hv_gic_config.h`,
    // `hv_gic_parameters.h`) --------------------------------------
    //
    // `hv_gic_config_t` is an opaque, retain-counted `os_object_t` in
    // the real headers (`OS_OBJECT_DECL`); modeled here as `*mut
    // c_void` since this module only ever creates one, passes it
    // straight to `hv_gic_create`, and deliberately leaks it rather
    // than resolve the ObjC-runtime-vs-plain-C-symbol question for a
    // release call that would run at most once per process anyway --
    // see `hvf::gic`.

    /// Creates a GIC configuration object. Must be `os_release`d when
    /// no longer needed -- `hvf::gic` deliberately doesn't (see
    /// above).
    pub fn hv_gic_config_create() -> *mut c_void;

    /// Sets the GIC distributor region's guest physical base address
    /// on a not-yet-`hv_gic_create`d configuration.
    pub fn hv_gic_config_set_distributor_base(
        config: *mut c_void,
        distributor_base_address: hv_ipa_t,
    ) -> hv_return_t;

    /// Sets the GIC redistributor region's guest physical base
    /// address (covering every vCPU's own redistributor frame,
    /// contiguously) on a not-yet-`hv_gic_create`d configuration.
    pub fn hv_gic_config_set_redistributor_base(
        config: *mut c_void,
        redistributor_base_address: hv_ipa_t,
    ) -> hv_return_t;

    /// Creates the (single, process-wide) GICv3 device from `config`.
    /// Must be called after `hv_vm_create` but before any
    /// `hv_vcpu_create`.
    pub fn hv_gic_create(config: *mut c_void) -> hv_return_t;

    /// The GIC distributor region's fixed size, in bytes.
    pub fn hv_gic_get_distributor_size(distributor_size: *mut usize) -> hv_return_t;

    /// The required alignment, in bytes, of the distributor region's
    /// base address.
    pub fn hv_gic_get_distributor_base_alignment(
        distributor_base_alignment: *mut usize,
    ) -> hv_return_t;

    /// The total size, in bytes, of the redistributor region (every
    /// vCPU's own frame, contiguously).
    pub fn hv_gic_get_redistributor_region_size(
        redistributor_region_size: *mut usize,
    ) -> hv_return_t;

    /// The required alignment, in bytes, of the redistributor
    /// region's base address.
    pub fn hv_gic_get_redistributor_base_alignment(
        redistributor_base_alignment: *mut usize,
    ) -> hv_return_t;
}
