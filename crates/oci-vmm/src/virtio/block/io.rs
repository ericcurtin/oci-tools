// Copyright 2021 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
// Ported into oci-vmm from Firecracker (src/vmm/src/devices/virtio/block/virtio/io/{mod.rs,sync_io.rs}), trimmed of metrics/snapshot/rate-limiting/async.

//! Synchronous file engine for the virtio block device: blocking
//! `read_exact_volatile`/`write_all_volatile` at byte offsets against a
//! `std::fs::File`, `fsync` on flush. The io_uring engine was not ported.

use std::fs::File;
use std::io::{Seek, SeekFrom, Write};

use vm_memory::{GuestAddress, GuestMemory, GuestMemoryError, ReadVolatile, WriteVolatile};

use crate::mem::GuestMemoryMmap;

/// Errors from the synchronous IO engine.
#[derive(Debug, thiserror::Error)]
pub enum SyncIoError {
    /// Flushing the file's Rust-side buffers failed.
    #[error("Flush: {0}")]
    Flush(std::io::Error),
    /// Seeking to the request's offset failed.
    #[error("Seek: {0}")]
    Seek(std::io::Error),
    /// `fsync` failed.
    #[error("SyncAll: {0}")]
    SyncAll(std::io::Error),
    /// Transferring data between the file and guest memory failed.
    #[error("Transfer: {0}")]
    Transfer(GuestMemoryError),
}

/// A file engine based on blocking system calls.
#[derive(Debug)]
pub struct SyncFileEngine {
    file: File,
}

impl SyncFileEngine {
    /// Wrap `file` in a sync file engine.
    pub fn from_file(file: File) -> SyncFileEngine {
        SyncFileEngine { file }
    }

    /// Get a reference to the backing file.
    pub fn file(&self) -> &File {
        &self.file
    }

    /// Read `count` bytes from the file at `offset` into guest memory at `addr`.
    pub fn read(
        &mut self,
        offset: u64,
        mem: &GuestMemoryMmap,
        addr: GuestAddress,
        count: u32,
    ) -> Result<u32, SyncIoError> {
        self.file
            .seek(SeekFrom::Start(offset))
            .map_err(SyncIoError::Seek)?;
        mem.get_slice(addr, count as usize)
            .and_then(|mut slice| Ok(self.file.read_exact_volatile(&mut slice)?))
            .map_err(SyncIoError::Transfer)?;
        Ok(count)
    }

    /// Write `count` bytes from guest memory at `addr` into the file at `offset`.
    pub fn write(
        &mut self,
        offset: u64,
        mem: &GuestMemoryMmap,
        addr: GuestAddress,
        count: u32,
    ) -> Result<u32, SyncIoError> {
        self.file
            .seek(SeekFrom::Start(offset))
            .map_err(SyncIoError::Seek)?;
        mem.get_slice(addr, count as usize)
            .and_then(|slice| Ok(self.file.write_all_volatile(&slice)?))
            .map_err(SyncIoError::Transfer)?;
        Ok(count)
    }

    /// Flush any buffered data and sync it out to physical media.
    pub fn flush(&mut self) -> Result<(), SyncIoError> {
        // flush() first to force any cached data out of rust buffers.
        self.file.flush().map_err(SyncIoError::Flush)?;
        // Sync data out to physical media on host.
        self.file.sync_all().map_err(SyncIoError::SyncAll)
    }
}
