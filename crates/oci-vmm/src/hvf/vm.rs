// SPDX-License-Identifier: Apache-2.0

//! The Hypervisor.framework VM instance.
//!
//! Unlike KVM's `Vm` (`crate::vstate::vm::Vm`, one `VmFd` per VM,
//! arbitrarily many per process), `hv_vm_create`/`hv_vm_destroy`
//! operate on *the current process* directly -- there is no VM
//! handle/fd at all. Exactly one `Vm` may exist per `ocivmm` process
//! at a time (matching how this project already runs one pet VM per
//! process on the KVM backend, so this isn't a new constraint in
//! practice); enforced here with a process-wide guard so a second
//! `Vm::create` fails loudly instead of silently reusing the first
//! VM's state.

use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::hvf::error::{HvError, check};
use crate::hvf::sys;

static VM_EXISTS: AtomicBool = AtomicBool::new(false);

/// The Hypervisor.framework VM instance for this process. Dropping it
/// calls `hv_vm_destroy` (which itself requires every `Vcpu` to have
/// been dropped/destroyed first, same as the framework's own
/// documented requirement).
#[derive(Debug)]
pub struct Vm {
    _private: (),
}

impl Vm {
    /// Creates the VM instance for the current process, with the
    /// framework's default configuration.
    #[allow(unsafe_code)] // hv_vm_create: precondition enforced by VM_EXISTS just above the call.
    pub fn create() -> Result<Vm, HvError> {
        if VM_EXISTS.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire) != Ok(false)
        {
            return Err(HvError::Exists);
        }

        // SAFETY: hv_vm_create's only precondition is "no VM already
        // exists for this process", enforced immediately above by
        // `VM_EXISTS`. Passing null selects the framework's own
        // default `hv_vm_config_t`, explicitly documented as valid.
        let rc = unsafe { sys::hv_vm_create(ptr::null_mut()) };
        if let Err(e) = check(rc) {
            VM_EXISTS.store(false, Ordering::Release);
            return Err(e);
        }

        Ok(Vm { _private: () })
    }

    /// Maps `size` bytes of already-allocated, page-aligned host
    /// memory at `host_addr` into the guest physical address space at
    /// `guest_addr` (also page-aligned), with the given
    /// read/write/execute permissions.
    ///
    /// # Safety
    /// `host_addr` must point to at least `size` bytes of valid,
    /// page-aligned host memory that outlives every guest access to
    /// this mapping (i.e. the whole lifetime of this `Vm` and every
    /// `Vcpu` created against it, unless explicitly unmapped first --
    /// `hv_vm_unmap` isn't wrapped yet, see `docs/design/0249` phase
    /// 3).
    #[allow(unsafe_code)] // hv_vm_map needs a valid, caller-owned host mapping; documented above.
    pub unsafe fn map(
        &self,
        host_addr: *mut u8,
        guest_addr: u64,
        size: usize,
        read: bool,
        write: bool,
        exec: bool,
    ) -> Result<(), HvError> {
        let mut flags = 0u64;
        if read {
            flags |= sys::HV_MEMORY_READ;
        }
        if write {
            flags |= sys::HV_MEMORY_WRITE;
        }
        if exec {
            flags |= sys::HV_MEMORY_EXEC;
        }

        // SAFETY: forwarded to the caller's own safety obligations
        // documented above; `host_addr as *mut c_void` is a plain
        // pointer reinterpretation, no aliasing/lifetime assumption
        // introduced by this cast itself.
        let rc = unsafe { sys::hv_vm_map(host_addr.cast::<c_void>(), guest_addr, size, flags) };
        check(rc)
    }
}

impl Drop for Vm {
    #[allow(unsafe_code)] // hv_vm_destroy: no arguments, only precondition is documented at the type.
    fn drop(&mut self) {
        // SAFETY: `hv_vm_destroy` requires all vCPUs already
        // destroyed; `Vcpu::drop` (which must run first, since a
        // `Vcpu` can't outlive the `Vm` it borrowed to create -- see
        // `hvf::vcpu`) upholds that. A failure here (e.g. a vCPU
        // still alive due to a bug elsewhere) is reported via
        // `tracing`, not a panic in a destructor.
        let rc = unsafe { sys::hv_vm_destroy() };
        if let Err(e) = check(rc) {
            tracing::error!("hv_vm_destroy failed: {e}");
        }
        VM_EXISTS.store(false, Ordering::Release);
    }
}
