//! OCI runtime-spec execution engine — the heart of `ocirun`.
//!
//! **Status: stub** — implemented in milestone 3 (see `docs/design/`).
//!
//! Planned scope:
//! - container lifecycle per the OCI runtime spec: create, start, kill,
//!   delete, exec; state tracking and hooks (prestart/createRuntime/...)
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
