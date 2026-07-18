//! Mount plumbing shared by the engine, runtime, and boot stack.
//!
//! **Scope shipped so far**: [`options`] — translating OCI runtime-spec
//! mount option strings into kernel `MS_*` flags and leftover
//! filesystem-specific data. Pure logic; the actual `mount(2)`/
//! `pivot_root(2)` calls that consume this are a later increment.
//!
//! Planned scope (see `docs/design/`):
//! - loop device management (attach/detach, read-only, direct-io)
//! - overlayfs assembly (lowerdir stacks, upper/work dirs on ext4/xfs)
//! - bind mounts, recursive binds, remounts, tmpfs setup, `pivot_root`
//! - mount namespaces and propagation control
//! - idmapped mounts for rootless containers
//!
//! Consumers: `oci-runtime-core` (container rootfs), `ociman` (storage
//! mounts), `ociboot`/`ociboot-init` (deployment loop mounts, /etc overlay,
//! /var bind). Test strategy: pure logic unit-tested with fakes; privileged
//! integration tests gated behind an environment variable.

pub mod options;

pub use options::{ParsedMountOptions, parse_mount_options};
