//! Mount plumbing shared by the engine, runtime, and boot stack.
//!
//! **Scope shipped so far**:
//! - [`options`] — translating OCI runtime-spec mount option strings
//!   into kernel `MS_*` flags and leftover filesystem-specific data.
//! - [`syscalls`] — the `mount(2)`/`pivot_root(2)` wrappers that consume
//!   those flags: one call each, correctly dispatched to the right
//!   `mount(2)` mode. Building the *sequence* of calls a container's
//!   full mount setup needs (bind-then-remount for read-only binds,
//!   masked paths, the rootfs pivot itself) is `create`'s job, not this
//!   crate's — see `syscalls`' module docs.
//!
//! Planned scope (see `docs/design/`):
//! - loop device management (attach/detach, read-only, direct-io)
//! - overlayfs assembly (lowerdir stacks, upper/work dirs on ext4/xfs)
//! - mount namespaces and propagation control
//! - idmapped mounts for rootless containers
//!
//! Consumers: `oci-runtime-core` (container rootfs), `ociman` (storage
//! mounts), `ociboot`/`ociboot-init` (deployment loop mounts, /etc overlay,
//! /var bind). Test strategy: pure logic unit-tested with fakes; privileged
//! integration tests gated behind an environment variable.

pub mod options;
pub mod syscalls;

pub use options::{ParsedMountOptions, parse_mount_options};
pub use syscalls::{MountPlan, mount, pivot_root};
