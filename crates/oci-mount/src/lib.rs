//! Mount plumbing shared by the engine, runtime, and boot stack.
//!
//! **Status: stub** — first pieces land with milestone 3 (overlayfs for
//! container rootfs), completed in milestone 5 (loop devices + boot mounts);
//! see `docs/design/`.
//!
//! Planned scope:
//! - loop device management (attach/detach, read-only, direct-io)
//! - overlayfs assembly (lowerdir stacks, upper/work dirs on ext4/xfs)
//! - bind mounts, recursive binds, remount flags, tmpfs setup
//! - mount namespaces and propagation control
//! - idmapped mounts for rootless containers
//!
//! Consumers: `oci-runtime-core` (container rootfs), `ociman` (storage
//! mounts), `ociboot`/`ociboot-init` (deployment loop mounts, /etc overlay,
//! /var bind). Test strategy: pure logic unit-tested with fakes; privileged
//! integration tests gated behind an environment variable.
