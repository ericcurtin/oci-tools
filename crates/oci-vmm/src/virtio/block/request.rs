// Copyright 2020 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Portions Copyright 2017 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the THIRD-PARTY file.
// Ported into oci-vmm from Firecracker (src/vmm/src/devices/virtio/block/virtio/request.rs), trimmed of metrics/snapshot/rate-limiting/async.

//! Parsing and execution of virtio block requests.

use tracing::error;
use vm_memory::{ByteValued, Bytes, GuestAddress, GuestMemoryError};

use super::io::SyncIoError;
use super::{SECTOR_SHIFT, SECTOR_SIZE, VirtioBlockError};
use crate::mem::GuestMemoryMmap;
use crate::virtio::block::device::DiskProperties;
pub use crate::virtio::generated::virtio_blk::{
    VIRTIO_BLK_ID_BYTES, VIRTIO_BLK_S_IOERR, VIRTIO_BLK_S_OK, VIRTIO_BLK_S_UNSUPP,
    VIRTIO_BLK_T_FLUSH, VIRTIO_BLK_T_GET_ID, VIRTIO_BLK_T_IN, VIRTIO_BLK_T_OUT,
};
use crate::virtio::queue::DescriptorChain;

/// Errors executing a block request against the backing file.
#[derive(Debug, thiserror::Error)]
pub enum IoErr {
    /// Writing the device ID to guest memory failed.
    #[error("Get device ID: {0}")]
    GetId(GuestMemoryError),
    /// Fewer bytes than requested were transferred.
    #[error("Partial transfer: completed {completed} out of {expected} bytes")]
    PartialTransfer {
        /// Number of bytes actually transferred.
        completed: u32,
        /// Number of bytes the request asked for.
        expected: u32,
    },
    /// The sync file engine failed.
    #[error("File engine: {0}")]
    FileEngine(SyncIoError),
}

/// The type of a virtio block request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestType {
    /// Read from the disk into guest memory.
    In,
    /// Write from guest memory to the disk.
    Out,
    /// Flush the disk's caches.
    Flush,
    /// Read the device's serial (disk ID).
    GetDeviceID,
    /// Any request type we do not implement.
    Unsupported(u32),
}

impl From<u32> for RequestType {
    fn from(value: u32) -> Self {
        match value {
            VIRTIO_BLK_T_IN => RequestType::In,
            VIRTIO_BLK_T_OUT => RequestType::Out,
            VIRTIO_BLK_T_FLUSH => RequestType::Flush,
            VIRTIO_BLK_T_GET_ID => RequestType::GetDeviceID,
            t => RequestType::Unsupported(t),
        }
    }
}

/// Outcome of a fully executed request, ready for the used ring.
#[derive(Debug)]
pub struct FinishedRequest {
    /// Number of bytes the device wrote to guest memory (used ring `len`).
    pub num_bytes_to_mem: u32,
    /// Index of the descriptor chain head to return to the guest.
    pub desc_idx: u16,
}

#[derive(Debug)]
enum Status {
    Ok { num_bytes_to_mem: u32 },
    IoErr { num_bytes_to_mem: u32, err: IoErr },
    Unsupported { op: u32 },
}

impl Status {
    fn from_data(data_len: u32, transferred_data_len: u32, data_to_mem: bool) -> Status {
        let num_bytes_to_mem = match data_to_mem {
            true => transferred_data_len,
            false => 0,
        };

        match transferred_data_len == data_len {
            true => Status::Ok { num_bytes_to_mem },
            false => Status::IoErr {
                num_bytes_to_mem,
                err: IoErr::PartialTransfer {
                    completed: transferred_data_len,
                    expected: data_len,
                },
            },
        }
    }
}

/// A parsed request whose I/O has been issued but whose status byte and
/// used-ring entry are still owed to the guest.
#[derive(Debug)]
pub struct PendingRequest {
    r#type: RequestType,
    data_len: u32,
    status_addr: GuestAddress,
    desc_idx: u16,
}

impl PendingRequest {
    fn write_status_and_finish(self, status: &Status, mem: &GuestMemoryMmap) -> FinishedRequest {
        let (num_bytes_to_mem, status_code) = match status {
            Status::Ok { num_bytes_to_mem } => {
                (*num_bytes_to_mem, u8::try_from(VIRTIO_BLK_S_OK).unwrap())
            }
            Status::IoErr {
                num_bytes_to_mem,
                err,
            } => {
                error!(
                    "Failed to execute {:?} virtio block request: {:?}",
                    self.r#type, err
                );
                (*num_bytes_to_mem, u8::try_from(VIRTIO_BLK_S_IOERR).unwrap())
            }
            Status::Unsupported { op } => {
                error!("Received unsupported virtio block request: {op}");
                (0, u8::try_from(VIRTIO_BLK_S_UNSUPP).unwrap())
            }
        };

        let num_bytes_to_mem = mem
            .write_obj(status_code, self.status_addr)
            .map(|_| {
                // Account for the status byte
                num_bytes_to_mem + 1
            })
            .unwrap_or_else(|err| {
                error!("Failed to write virtio block status: {err:?}");
                // If we can't write the status, discard the virtio descriptor
                0
            });

        FinishedRequest {
            num_bytes_to_mem,
            desc_idx: self.desc_idx,
        }
    }

    /// Write the request's status byte to guest memory and produce the
    /// used-ring entry, according to the I/O result `res`.
    pub fn finish(self, mem: &GuestMemoryMmap, res: Result<u32, IoErr>) -> FinishedRequest {
        let status = match (res, self.r#type) {
            (Ok(transferred_data_len), RequestType::In) => {
                Status::from_data(self.data_len, transferred_data_len, true)
            }
            (Ok(transferred_data_len), RequestType::Out) => {
                Status::from_data(self.data_len, transferred_data_len, false)
            }
            (Ok(_), RequestType::Flush) => Status::Ok {
                num_bytes_to_mem: 0,
            },
            (Ok(transferred_data_len), RequestType::GetDeviceID) => {
                Status::from_data(self.data_len, transferred_data_len, true)
            }
            (_, RequestType::Unsupported(op)) => Status::Unsupported { op },
            (Err(err), _) => Status::IoErr {
                num_bytes_to_mem: 0,
                err,
            },
        };

        self.write_status_and_finish(&status, mem)
    }
}

/// The request header represents the mandatory fields of each block device request.
///
/// A request header contains the following fields:
///   * request_type: an u32 value mapping to a read, write or flush operation.
///   * reserved: 32 bits are reserved for future extensions of the Virtio Spec.
///   * sector: an u64 value representing the offset where a read/write is to occur.
///
/// The header simplifies reading the request from memory as all request follow
/// the same memory layout.
#[derive(Debug, Copy, Clone, Default)]
#[repr(C)]
pub struct RequestHeader {
    request_type: u32,
    _reserved: u32,
    sector: u64,
}

// SAFETY: Safe because RequestHeader only contains plain data.
#[allow(unsafe_code)]
unsafe impl ByteValued for RequestHeader {}

impl RequestHeader {
    /// Build a request header.
    pub fn new(request_type: u32, sector: u64) -> RequestHeader {
        RequestHeader {
            request_type,
            _reserved: 0,
            sector,
        }
    }

    /// Reads the request header from GuestMemoryMmap starting at `addr`.
    ///
    /// Virtio 1.0 specifies that the data is transmitted by the driver in little-endian
    /// format. Firecracker currently runs only on little endian platforms so we don't
    /// need to do an explicit little endian read as all reads are little endian by default.
    /// When running on a big endian platform, this code should not compile, and support
    /// for explicit little endian reads is required.
    #[cfg(target_endian = "little")]
    fn read_from(memory: &GuestMemoryMmap, addr: GuestAddress) -> Result<Self, VirtioBlockError> {
        let request_header: RequestHeader = memory
            .read_obj(addr)
            .map_err(VirtioBlockError::GuestMemory)?;
        Ok(request_header)
    }
}

/// A parsed virtio block request.
#[derive(Debug, PartialEq, Eq)]
pub struct Request {
    /// The type of the request.
    pub r#type: RequestType,
    /// Length of the data descriptor, in bytes.
    pub data_len: u32,
    /// Guest address of the status byte.
    pub status_addr: GuestAddress,
    sector: u64,
    data_addr: GuestAddress,
}

impl Request {
    /// Parse a descriptor chain into a block request, validating descriptor
    /// directions, data length, and disk bounds.
    pub fn parse(
        avail_desc: &DescriptorChain,
        mem: &GuestMemoryMmap,
        num_disk_sectors: u64,
    ) -> Result<Request, VirtioBlockError> {
        // The head contains the request type which MUST be readable.
        if avail_desc.is_write_only() {
            return Err(VirtioBlockError::UnexpectedWriteOnlyDescriptor);
        }

        let request_header = RequestHeader::read_from(mem, avail_desc.addr)?;
        let mut req = Request {
            r#type: RequestType::from(request_header.request_type),
            sector: request_header.sector,
            data_addr: GuestAddress(0),
            data_len: 0,
            status_addr: GuestAddress(0),
        };

        let data_desc;
        let status_desc;
        let desc = avail_desc
            .next_descriptor()
            .ok_or(VirtioBlockError::DescriptorChainTooShort)?;

        if !desc.has_next() {
            status_desc = desc;
            // Only flush requests are allowed to skip the data descriptor.
            if req.r#type != RequestType::Flush {
                return Err(VirtioBlockError::DescriptorChainTooShort);
            }
        } else {
            data_desc = desc;
            status_desc = data_desc
                .next_descriptor()
                .ok_or(VirtioBlockError::DescriptorChainTooShort)?;

            if data_desc.is_write_only() && req.r#type == RequestType::Out {
                return Err(VirtioBlockError::UnexpectedWriteOnlyDescriptor);
            }
            if !data_desc.is_write_only() && req.r#type == RequestType::In {
                return Err(VirtioBlockError::UnexpectedReadOnlyDescriptor);
            }
            if !data_desc.is_write_only() && req.r#type == RequestType::GetDeviceID {
                return Err(VirtioBlockError::UnexpectedReadOnlyDescriptor);
            }

            req.data_addr = data_desc.addr;
            req.data_len = data_desc.len;
        }

        // check request validity
        match req.r#type {
            RequestType::In | RequestType::Out => {
                // Check that the data length is a multiple of 512 as specified in the virtio
                // standard.
                if !req.data_len.is_multiple_of(SECTOR_SIZE) {
                    return Err(VirtioBlockError::InvalidDataLength);
                }
                let top_sector = req
                    .sector
                    .checked_add(u64::from(req.data_len) >> SECTOR_SHIFT)
                    .ok_or(VirtioBlockError::InvalidOffset)?;
                if top_sector > num_disk_sectors {
                    return Err(VirtioBlockError::InvalidOffset);
                }
            }
            RequestType::GetDeviceID if req.data_len < VIRTIO_BLK_ID_BYTES => {
                return Err(VirtioBlockError::InvalidDataLength);
            }
            _ => {}
        }

        // The status MUST always be writable.
        if !status_desc.is_write_only() {
            return Err(VirtioBlockError::UnexpectedReadOnlyDescriptor);
        }

        if status_desc.len < 1 {
            return Err(VirtioBlockError::DescriptorLengthTooSmall);
        }

        req.status_addr = status_desc.addr;

        Ok(req)
    }

    fn offset(&self) -> u64 {
        self.sector << SECTOR_SHIFT
    }

    fn to_pending_request(&self, desc_idx: u16) -> PendingRequest {
        PendingRequest {
            r#type: self.r#type,
            data_len: self.data_len,
            status_addr: self.status_addr,
            desc_idx,
        }
    }

    /// Execute the request against the disk's sync file engine.
    ///
    /// The engine is synchronous, so the request always executes to
    /// completion here (there is no submission/throttling state).
    pub(crate) fn process(
        self,
        disk: &mut DiskProperties,
        desc_idx: u16,
        mem: &GuestMemoryMmap,
    ) -> FinishedRequest {
        let pending = self.to_pending_request(desc_idx);
        let res = match self.r#type {
            RequestType::In => disk
                .file_engine
                .read(self.offset(), mem, self.data_addr, self.data_len)
                .map_err(IoErr::FileEngine),
            RequestType::Out => disk
                .file_engine
                .write(self.offset(), mem, self.data_addr, self.data_len)
                .map_err(IoErr::FileEngine),
            RequestType::Flush => disk
                .file_engine
                .flush()
                .map(|_| 0)
                .map_err(IoErr::FileEngine),
            RequestType::GetDeviceID => mem
                .write_slice(&disk.image_id, self.data_addr)
                .map(|_| VIRTIO_BLK_ID_BYTES)
                .map_err(IoErr::GetId),
            RequestType::Unsupported(_) => Ok(0),
        };

        pending.finish(mem, res)
    }
}
