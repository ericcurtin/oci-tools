// SPDX-License-Identifier: Apache-2.0

#![allow(unsafe_code)] // Every method here is a thin, individually-documented FFI call onto hvf::sys.

//! A Hypervisor.framework vCPU.
//!
//! Every `hv_vcpu_*` call is documented as "must be called by the
//! owning thread" -- the thread that called `hv_vcpu_create`. `Vcpu`
//! holds a raw `*mut sys::hv_vcpu_exit_t` (the framework's own exit-
//! info memory, refreshed by every `run()`), which makes it `!Send`/
//! `!Sync` automatically (raw pointers are neither by default) --
//! enforcing that constraint at compile time rather than just in a
//! doc comment, without needing an explicit negative impl.

use std::marker::PhantomData;
use std::ptr;

use crate::hvf::error::{HvError, check};
use crate::hvf::sys::{self, hv_reg_t, hv_sys_reg_t};
use crate::hvf::vm::Vm;

/// Why a vCPU last returned from [`Vcpu::run`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    /// `HV_EXIT_REASON_CANCELED`.
    Canceled,
    /// `HV_EXIT_REASON_EXCEPTION`: see the accompanying
    /// [`Exception`].
    Exception(Exception),
    /// `HV_EXIT_REASON_VTIMER_ACTIVATED`.
    VtimerActivated,
    /// Any other raw reason code (`HV_EXIT_REASON_UNKNOWN`, or a
    /// value not yet in this enum).
    Other(u32),
}

/// Details of a synchronous exception exit (`reason ==
/// HV_EXIT_REASON_EXCEPTION`) -- the raw `ESR_ELx`/`FAR_ELx`/faulting
/// IPA, undecoded. Decoding `syndrome`'s `EC` field into e.g. "guest
/// executed HVC" is deferred to whichever later phase first needs it
/// (arm64 boot/virtio-mmio, `docs/design/0249` phases 3-4); this
/// foundation only needs to prove the raw exit round-trips correctly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exception {
    /// `ESR_ELx` at the time of the exception.
    pub syndrome: u64,
    /// `FAR_ELx` at the time of the exception.
    pub virtual_address: u64,
    /// The faulting guest physical address (only meaningful for
    /// stage-2/IPA faults; ignored by every current caller).
    pub physical_address: u64,
}

/// A vCPU created for -- and only ever driven from -- the thread that
/// called [`Vcpu::create`]. Borrows the owning [`Vm`] so a `Vcpu`
/// can't outlive it (required: `hv_vm_destroy` demands every vCPU be
/// destroyed first).
#[derive(Debug)]
pub struct Vcpu<'vm> {
    id: sys::hv_vcpu_t,
    exit: *mut sys::hv_vcpu_exit_t,
    _vm: PhantomData<&'vm Vm>,
}

impl<'vm> Vcpu<'vm> {
    /// Creates a vCPU for the current thread, with the framework's
    /// default configuration. Must be called on the thread that will
    /// exclusively own this `Vcpu` for its whole lifetime.
    pub fn create(_vm: &'vm Vm) -> Result<Self, HvError> {
        let mut id: sys::hv_vcpu_t = 0;
        let mut exit: *mut sys::hv_vcpu_exit_t = ptr::null_mut();

        // SAFETY: `&mut id`/`&mut exit` are valid, uniquely-owned
        // out-parameters for the duration of this call; null selects
        // the framework's own default `hv_vcpu_config_t`, explicitly
        // documented as valid.
        let rc = unsafe { sys::hv_vcpu_create(&mut id, &mut exit, ptr::null_mut()) };
        check(rc)?;

        Ok(Vcpu {
            id,
            exit,
            _vm: PhantomData,
        })
    }

    /// Reads a general-purpose/PC/FPCR/FPSR/CPSR register.
    pub fn get_reg(&self, reg: hv_reg_t) -> Result<u64, HvError> {
        let mut value = 0u64;
        // SAFETY: `&mut value` is a valid out-parameter; `self.id`
        // was returned by a successful `hv_vcpu_create` and this
        // method (like every `hv_vcpu_*` call) runs on the owning
        // thread, since `Vcpu` is `!Send`/`!Sync`.
        let rc = unsafe { sys::hv_vcpu_get_reg(self.id, reg, &mut value) };
        check(rc)?;
        Ok(value)
    }

    /// Writes a general-purpose/PC/FPCR/FPSR/CPSR register.
    pub fn set_reg(&self, reg: hv_reg_t, value: u64) -> Result<(), HvError> {
        // SAFETY: see `get_reg`.
        let rc = unsafe { sys::hv_vcpu_set_reg(self.id, reg, value) };
        check(rc)
    }

    /// Reads an AArch64 system register.
    pub fn get_sys_reg(&self, reg: hv_sys_reg_t) -> Result<u64, HvError> {
        let mut value = 0u64;
        // SAFETY: see `get_reg`.
        let rc = unsafe { sys::hv_vcpu_get_sys_reg(self.id, reg, &mut value) };
        check(rc)?;
        Ok(value)
    }

    /// Writes an AArch64 system register.
    pub fn set_sys_reg(&self, reg: hv_sys_reg_t, value: u64) -> Result<(), HvError> {
        // SAFETY: see `get_reg`.
        let rc = unsafe { sys::hv_vcpu_set_sys_reg(self.id, reg, value) };
        check(rc)
    }

    /// Runs the vCPU until the next exit, then returns why it exited.
    /// Blocks the calling thread.
    pub fn run(&self) -> Result<ExitReason, HvError> {
        // SAFETY: see `get_reg`.
        let rc = unsafe { sys::hv_vcpu_run(self.id) };
        check(rc)?;

        // SAFETY: `self.exit` was written by `hv_vcpu_create` and is
        // valid and refreshed for the lifetime of this vCPU (the
        // framework's own documented contract); `hv_vcpu_run` just
        // returned successfully, so it has just been refreshed.
        let exit = unsafe { &*self.exit };
        Ok(match exit.reason {
            sys::HV_EXIT_REASON_CANCELED => ExitReason::Canceled,
            sys::HV_EXIT_REASON_EXCEPTION => ExitReason::Exception(Exception {
                syndrome: exit.exception.syndrome,
                virtual_address: exit.exception.virtual_address,
                physical_address: exit.exception.physical_address,
            }),
            sys::HV_EXIT_REASON_VTIMER_ACTIVATED => ExitReason::VtimerActivated,
            other => ExitReason::Other(other),
        })
    }
}

impl Drop for Vcpu<'_> {
    fn drop(&mut self) {
        // SAFETY: `self.id` was returned by a successful
        // `hv_vcpu_create`, and destruction is documented as required
        // to happen on the owning thread -- guaranteed here since
        // `Vcpu` is `!Send`/`!Sync`.
        let rc = unsafe { sys::hv_vcpu_destroy(self.id) };
        if let Err(e) = check(rc) {
            tracing::error!("hv_vcpu_destroy failed: {e}");
        }
    }
}
