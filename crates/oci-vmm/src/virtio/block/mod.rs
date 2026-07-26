// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
// Ported into oci-vmm from Firecracker (src/vmm/src/devices/virtio/block/{mod.rs,virtio/mod.rs}), trimmed of metrics/snapshot/rate-limiting/async.

//! Implements a virtio block device backed by a host file, sync I/O only.
//!
//! Compared to Firecracker's device, the `CacheType` knob is gone: the
//! device always advertises `VIRTIO_BLK_F_FLUSH` and honors guest flush
//! requests with `fsync` (Firecracker's `Writeback` behavior), because a
//! pet VM's one disk deserves its data.

// EventFd-based VirtioDevice impl -- see virtio::device's own doc
// comment on why this is Linux/x86_64-only; `disk`/`io`/`request` (the
// backing file, sync file I/O, and virtio-blk request parsing) have
// no such dependency and are shared with `hvf::virtio_mmio` directly.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub mod device;
pub mod disk;
pub mod io;
pub mod request;

use vm_memory::GuestMemoryError;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub use self::device::VirtioBlock;
pub use self::disk::{ConfigSpace, DiskProperties};
pub use self::request::*;

/// Sector shift for block device.
pub const SECTOR_SHIFT: u8 = 9;
/// Size of block sector.
pub const SECTOR_SIZE: u32 = (0x01_u32) << SECTOR_SHIFT;
/// The number of queues of block device.
pub const BLOCK_NUM_QUEUES: usize = 1;
/// Queue sizes of the block device (Firecracker's maximum queue size, 256).
pub const BLOCK_QUEUE_SIZES: [u16; BLOCK_NUM_QUEUES] = [256];

/// Errors the block device can trigger.
#[derive(Debug, thiserror::Error)]
pub enum VirtioBlockError {
    /// Guest gave us too few descriptors in a descriptor chain.
    #[error("Guest gave us too few descriptors in a descriptor chain.")]
    DescriptorChainTooShort,
    /// Guest gave us a descriptor that was too short to use.
    #[error("Guest gave us a descriptor that was too short to use.")]
    DescriptorLengthTooSmall,
    /// Guest gave us bad memory addresses.
    #[error("Guest gave us bad memory addresses: {0}")]
    GuestMemory(GuestMemoryError),
    /// The data length is invalid.
    #[error("The data length is invalid.")]
    InvalidDataLength,
    /// The requested operation would cause a seek beyond disk end.
    #[error("The requested operation would cause a seek beyond disk end.")]
    InvalidOffset,
    /// Guest gave us a read only descriptor that protocol says to write to.
    #[error("Guest gave us a read only descriptor that protocol says to write to.")]
    UnexpectedReadOnlyDescriptor,
    /// Guest gave us a write only descriptor that protocol says to read from.
    #[error("Guest gave us a write only descriptor that protocol says to read from.")]
    UnexpectedWriteOnlyDescriptor,
    /// Error manipulating the backing file.
    #[error("Error manipulating the backing file: {0} {1}")]
    BackingFile(std::io::Error, String),
    /// Error opening eventfd.
    #[error("Error opening eventfd: {0}")]
    EventFd(std::io::Error),
}
