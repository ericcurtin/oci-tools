// SPDX-License-Identifier: Apache-2.0

//! A small `hv_return_t` wrapper: every `hvf::sys` call returns one,
//! and almost all of `hvf::vm`/`hvf::vcpu`'s own methods just need to
//! turn a non-`HV_SUCCESS` code into a `Result`.

use crate::hvf::sys::{self, hv_return_t};

/// An error returned by a `Hypervisor.framework` call, still carrying
/// its raw `hv_return_t` code (the framework has no `strerror`-style
/// lookup; the small, fixed set of codes in `hv_error.h`/
/// `arm64/hv/hv_kern_types.h` is reproduced here by name).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HvError {
    /// `HV_ERROR`: unspecified error.
    #[error("HV_ERROR (unspecified)")]
    Error,
    /// `HV_BUSY`.
    #[error("HV_BUSY")]
    Busy,
    /// `HV_BAD_ARGUMENT`.
    #[error("HV_BAD_ARGUMENT")]
    BadArgument,
    /// `HV_ILLEGAL_GUEST_STATE`.
    #[error("HV_ILLEGAL_GUEST_STATE")]
    IllegalGuestState,
    /// `HV_NO_RESOURCES`.
    #[error("HV_NO_RESOURCES")]
    NoResources,
    /// `HV_NO_DEVICE`.
    #[error("HV_NO_DEVICE")]
    NoDevice,
    /// `HV_DENIED`: most commonly, the calling process isn't
    /// codesigned with the `com.apple.security.hypervisor`
    /// entitlement -- see `ci/codesign-ocivmm.sh` and
    /// `docs/design/0249`.
    #[error(
        "HV_DENIED (is this binary codesigned with the com.apple.security.hypervisor \
         entitlement? see ci/codesign-ocivmm.sh)"
    )]
    Denied,
    /// `HV_EXISTS`.
    #[error("HV_EXISTS")]
    Exists,
    /// `HV_UNSUPPORTED`.
    #[error("HV_UNSUPPORTED")]
    Unsupported,
    /// Any code not in the fixed set above (not expected, but the
    /// framework's own headers don't guarantee exhaustiveness).
    #[error("unrecognized hv_return_t: {0:#x}")]
    Unknown(hv_return_t),
}

impl HvError {
    /// Maps a raw `hv_return_t` to an `HvError`. Callers should check
    /// for `sys::HV_SUCCESS` themselves; this is only meaningful for
    /// non-success codes.
    fn from_raw(code: hv_return_t) -> Self {
        // These constants live in `arm64/hv/hv_kern_types.h`, not
        // exposed as named Rust constants in `sys` (only the
        // `HV_SUCCESS`/exit-reason ones `hvf::vm`/`hvf::vcpu` branch
        // on are) -- reproduced directly here instead, since this is
        // the one place that needs them.
        const HV_ERROR: hv_return_t = 0xfae9_4001_u32 as hv_return_t;
        const HV_BUSY: hv_return_t = 0xfae9_4002_u32 as hv_return_t;
        const HV_BAD_ARGUMENT: hv_return_t = 0xfae9_4003_u32 as hv_return_t;
        const HV_ILLEGAL_GUEST_STATE: hv_return_t = 0xfae9_4004_u32 as hv_return_t;
        const HV_NO_RESOURCES: hv_return_t = 0xfae9_4005_u32 as hv_return_t;
        const HV_NO_DEVICE: hv_return_t = 0xfae9_4006_u32 as hv_return_t;
        const HV_DENIED: hv_return_t = 0xfae9_4007_u32 as hv_return_t;
        const HV_EXISTS: hv_return_t = 0xfae9_4008_u32 as hv_return_t;
        const HV_UNSUPPORTED: hv_return_t = 0xfae9_400f_u32 as hv_return_t;

        match code {
            HV_ERROR => HvError::Error,
            HV_BUSY => HvError::Busy,
            HV_BAD_ARGUMENT => HvError::BadArgument,
            HV_ILLEGAL_GUEST_STATE => HvError::IllegalGuestState,
            HV_NO_RESOURCES => HvError::NoResources,
            HV_NO_DEVICE => HvError::NoDevice,
            HV_DENIED => HvError::Denied,
            HV_EXISTS => HvError::Exists,
            HV_UNSUPPORTED => HvError::Unsupported,
            other => HvError::Unknown(other),
        }
    }
}

/// Turns a raw `hv_return_t` into `Ok(())` (on `HV_SUCCESS`) or
/// `Err(HvError)`.
pub(crate) fn check(code: hv_return_t) -> Result<(), HvError> {
    if code == sys::HV_SUCCESS {
        Ok(())
    } else {
        Err(HvError::from_raw(code))
    }
}
