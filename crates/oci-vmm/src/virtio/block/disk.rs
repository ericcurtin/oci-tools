// Copyright 2020 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Portions Copyright 2017 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the THIRD-PARTY file.
// Ported into oci-vmm from Firecracker (src/vmm/src/devices/virtio/block/virtio/device.rs),
// trimmed of metrics/snapshot/rate-limiting/async, and split out of `device.rs` (which stayed
// Linux/x86_64-only for its EventFd/event_manager `VirtioDevice` impl) since this part -- the
// backing file and its `virtio_blk_config` space -- has no such dependency and is shared with
// `hvf::virtio_mmio`'s own block device directly.

//! The virtio block device's backing file and configuration space --
//! transport- and hypervisor-agnostic (only `std::fs`/`std::path` and
//! [`super::io::SyncFileEngine`]), unlike [`super::device::VirtioBlock`]
//! (the KVM/x86_64 PCI transport's own EventFd-driven wrapper around
//! this).

use std::cmp;
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom};
use std::path::{Path, PathBuf};

use tracing::warn;
use vm_memory::ByteValued;

use super::io::SyncFileEngine;
use super::{SECTOR_SHIFT, SECTOR_SIZE, VirtioBlockError};
use crate::virtio::generated::virtio_blk::VIRTIO_BLK_ID_BYTES;

/// Helper object for setting up all `Block` fields derived from its backing file.
#[derive(Debug)]
pub struct DiskProperties {
    /// The engine performing I/O against the backing file.
    pub file_engine: SyncFileEngine,
    /// Number of `SECTOR_SIZE` sectors the disk exposes to the guest.
    pub nsectors: u64,
    /// The serial returned for `VIRTIO_BLK_T_GET_ID` requests.
    pub image_id: [u8; VIRTIO_BLK_ID_BYTES as usize],
}

impl DiskProperties {
    // Helper function that opens the file with the proper access permissions
    fn open_file(
        disk_image_path: &Path,
        is_disk_read_only: bool,
    ) -> Result<File, VirtioBlockError> {
        OpenOptions::new()
            .read(true)
            .write(!is_disk_read_only)
            .open(disk_image_path)
            .map_err(|x| VirtioBlockError::BackingFile(x, disk_image_path.display().to_string()))
    }

    // Helper function that gets the size of the file
    fn file_size(disk_image_path: &Path, disk_image: &mut File) -> Result<u64, VirtioBlockError> {
        let disk_size = disk_image
            .seek(SeekFrom::End(0))
            .map_err(|x| VirtioBlockError::BackingFile(x, disk_image_path.display().to_string()))?;

        // We only support disk size, which uses the first two words of the configuration space.
        // If the image is not a multiple of the sector size, the tail bits are not exposed.
        if disk_size % u64::from(SECTOR_SIZE) != 0 {
            warn!(
                "Disk size {disk_size} is not a multiple of sector size {SECTOR_SIZE}; the \
                 remainder will not be visible to the guest."
            );
        }

        Ok(disk_size)
    }

    /// Create the disk properties from the backing file and the disk ID.
    pub fn new(
        disk_image_path: PathBuf,
        is_disk_read_only: bool,
        disk_id: &str,
    ) -> Result<Self, VirtioBlockError> {
        let mut disk_image = Self::open_file(&disk_image_path, is_disk_read_only)?;
        let disk_size = Self::file_size(&disk_image_path, &mut disk_image)?;

        Ok(Self {
            file_engine: SyncFileEngine::from_file(disk_image),
            nsectors: disk_size >> SECTOR_SHIFT,
            image_id: Self::build_disk_image_id(disk_id),
        })
    }

    fn build_disk_image_id(disk_id: &str) -> [u8; VIRTIO_BLK_ID_BYTES as usize] {
        let mut default_id = [0; VIRTIO_BLK_ID_BYTES as usize];
        // The kernel only knows to read a maximum of VIRTIO_BLK_ID_BYTES.
        // This will also zero out any leftover bytes.
        let disk_id = disk_id.as_bytes();
        let bytes_to_copy = cmp::min(disk_id.len(), VIRTIO_BLK_ID_BYTES as usize);
        default_id[..bytes_to_copy].copy_from_slice(&disk_id[..bytes_to_copy]);
        default_id
    }
}

/// The virtio block device configuration space (`struct virtio_blk_config`,
/// of which this project exposes only the mandatory `capacity` field).
#[derive(Debug, Default, Clone, Copy, Eq, PartialEq)]
#[repr(C)]
pub struct ConfigSpace {
    /// Disk capacity in `SECTOR_SIZE` sectors, little-endian.
    pub capacity: u64,
}

// SAFETY: `ConfigSpace` contains only PODs in `repr(C)` or `repr(transparent)`, without padding.
#[allow(unsafe_code)]
unsafe impl ByteValued for ConfigSpace {}
