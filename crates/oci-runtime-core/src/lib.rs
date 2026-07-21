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
//! - [`namespaces`] — runtime-spec namespace list -> `unshare(2)` flag
//!   computation, the `unshare` wrapper itself, and the rootless
//!   user-namespace ID-mapping dance (`/proc/<pid>/{uid,gid}_map`,
//!   `setgroups`).
//! - [`cgroups`] — `LinuxResources` (memory/cpu/pids) -> cgroup v2
//!   interface file writes, resolving `linux.cgroupsPath` to a real
//!   cgroup v2 directory (`cgroupfs` driver only), and migrating the
//!   container process into it.
//! - [`rootfs`] — planning the ordered sequence of mount/pivot_root/
//!   hostname operations a bundle's `config.json` calls for (the
//!   "sequencing" piece `oci_mount::syscalls` explicitly left for this
//!   crate).
//! - [`process`] — `fork(2)`/`waitpid(2)`, the one syscall `rustix`
//!   deliberately never wraps.
//! - [`identity`] — dropping from "root in the new namespaces" to the
//!   spec's declared `process.user`, capability sets, and
//!   `no_new_privileges`, in the exact kernel-required order.
//! - [`rlimits`] — `process.rlimits` -> `setrlimit(2)`.
//! - [`seccomp`] — `linux.seccomp` -> a compiled, installed
//!   `seccomp(2)` BPF filter (single-shared-action profiles only — see
//!   its own doc comment for the real, verified reason full multi-
//!   action profiles need more work).
//! - [`exec_fifo`] — the two-sided blocking-FIFO handshake `create`/
//!   `start` use to keep the container's init process waiting in
//!   between the two, `pivot_root`-safe (ported from real `runc`'s own
//!   mechanism, ID'd bugs and all — see its own doc comment).
//! - [`signal`] — parsing a `kill` signal argument (number or name)
//!   the way real `runc kill` does.
//! - [`launch`] — assembling all of the above (plus `oci_mount`) into
//!   either a `create`-and-`start`-in-one-step container run (`ocirun
//!   run`), or the separate `create` half of the two-phase lifecycle
//!   (`ocirun create`/`start`/`kill`/`delete`), left blocked on
//!   [`exec_fifo`] until `start` unblocks it.
//!
//! All of this is deliberately built and tested *before* actual container
//! creation: nothing here does the one truly risky thing yet — actually
//! forking/cloning the container's init process and calling `unshare`
//! against a real running process ([`namespaces`]'s doc comment explains
//! why that specific step isn't covered by an automated test yet).
//!
//! Planned (rest of milestone 3):
//! - `exec` (running an *additional* process inside an already-running
//!   container) and lifecycle hooks (prestart/createRuntime/
//!   startContainer/...) — create/start/kill/delete are done, see
//!   [`launch::create`]
//! - namespaces (user, mount, pid, net, uts, ipc, cgroup, time), rootless
//!   user-namespace setup with uid/gid mappings
//! - the systemd cgroup driver and automatic rootless delegated-subtree
//!   discovery (the `cgroupfs` driver and manual directory creation +
//!   process migration are done — see [`cgroups`])
//! - full multi-action seccomp profiles (single-shared-action profiles
//!   are done — see [`seccomp`]); uid/gid, capability sets,
//!   `no_new_privileges`, and POSIX rlimits are done too (see
//!   [`identity`] and [`rlimits`])
//! - pivot_root into the prepared rootfs (via `oci-mount`)
//! - terminal handling: PTY allocation, console socket protocol (a real,
//!   documented gap for `create`'s backgrounded container process —
//!   see `docs/design/0017`)
//!
//! Exactly one runtime implementation exists in the workspace: `ocirun` is a
//! thin runc-compatible CLI over this crate, and `ociman`/`ocicri` execute
//! containers through it as a library (never by exec'ing `ocirun`).
//! Prior art: youki, crun — concepts borrowed, code original.

pub mod bundle;
pub mod cgroups;
pub mod exec;
pub mod exec_fifo;
pub mod hooks;
pub mod identity;
pub mod launch;
pub mod namespaces;
pub mod nsenter;
pub mod overlay;
pub mod process;
pub mod rlimits;
pub mod rootfs;
pub mod seccomp;
pub mod signal;
pub mod state;
pub mod systemd_cgroup;
pub mod validate;

pub use bundle::{Bundle, BundleError};
pub use process::{exit_code_from_wait_status, fork_and_wait};
pub use rootfs::{RootfsAction, plan_rootfs_setup};
pub use state::{PersistedState, StateError, StateStore, StateView, Status};
pub use validate::ValidateError;
