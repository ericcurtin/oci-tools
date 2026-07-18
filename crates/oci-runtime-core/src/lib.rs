//! OCI runtime-spec execution engine — the heart of `ocirun`.
//!
//! **Scope shipped so far**: [`state`] — the on-disk container state
//! model (`<root>/<id>/state.json`) and the `StateStore` directory
//! abstraction `create`/`start`/`kill`/`delete`/`state`/`list` all build
//! on. This is deliberately built and tested *before* actual container
//! creation: it has no idea how to start a container process yet.
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

pub mod state;
mod time;

pub use state::{PersistedState, StateError, StateStore, StateView, Status};
