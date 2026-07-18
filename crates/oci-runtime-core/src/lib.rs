//! OCI runtime-spec execution engine — the heart of `ocirun`.
//!
//! **Scope shipped so far**:
//! - [`state`] — the on-disk container state model
//!   (`<root>/<id>/state.json`) and the `StateStore` directory
//!   abstraction `create`/`start`/`kill`/`delete`/`state`/`list` all build
//!   on.
//! - [`bundle`] — reading and parsing `config.json` out of a bundle
//!   directory.
//! - [`validate`] — config sanity checks (rootfs exists, namespace/ID-
//!   mapping consistency, ...) that must pass before a bundle can be
//!   created.
//!
//! All of this is deliberately built and tested *before* actual container
//! creation: none of it needs privilege, namespaces, or a running process.
//!
//! Planned (rest of milestone 3):
//! - container lifecycle per the OCI runtime spec: create, start, kill,
//!   delete, exec; hooks (prestart/createRuntime/...)
//! - namespaces (user, mount, pid, net, uts, ipc, cgroup, time), rootless
//!   user-namespace setup with uid/gid mappings
//! - cgroups v2 with both systemd and cgroupfs drivers
//! - seccomp profiles, capability sets, rlimits, no_new_privs
//! - pivot_root into the prepared rootfs (via `oci-mount`)
//! - terminal handling: PTY allocation, console socket protocol
//!
//! Exactly one runtime implementation exists in the workspace: `ocirun` is a
//! thin runc-compatible CLI over this crate, and `ociman`/`ocicri` execute
//! containers through it as a library (never by exec'ing `ocirun`).
//! Prior art: youki, crun — concepts borrowed, code original.

pub mod bundle;
pub mod state;
mod time;
pub mod validate;

pub use bundle::{Bundle, BundleError};
pub use state::{PersistedState, StateError, StateStore, StateView, Status};
pub use validate::ValidateError;
