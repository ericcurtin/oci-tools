// SPDX-License-Identifier: Apache-2.0

//! A virtio-blk device for [`crate::hvf::virtio_mmio`] -- the
//! device-specific half; request parsing/execution against the
//! backing file is [`crate::virtio::block::request`]/
//! [`crate::virtio::block::disk`], shared verbatim with the KVM/
//! x86_64 backend's own virtio-blk device (see `virtio_mmio`'s own
//! module docs on why the *transport* isn't shared, but this
//! genuinely transport-agnostic protocol logic is).
//!
//! Deliberately minimal compared to `virtio::block::device::
//! VirtioBlock`: no `VIRTIO_RING_F_EVENT_IDX` (notification
//! suppression) is offered, so [`crate::virtio::queue::Queue::
//! prepare_kick`] always returns `true` and every `QueueNotify` that
//! touched the used ring raises an interrupt -- simpler, and correct,
//! at the cost of not suppressing notifications a real driver would
//! otherwise elide.

use std::path::PathBuf;

use crate::hvf::virtio_mmio::MmioVirtioDevice;
use crate::mem::GuestMemoryMmap;
use crate::virtio::block::disk::{ConfigSpace, DiskProperties};
use crate::virtio::block::request::{FinishedRequest, Request};
use crate::virtio::block::{BLOCK_QUEUE_SIZES, VirtioBlockError};
use crate::virtio::generated::virtio_blk::VIRTIO_BLK_F_FLUSH;
use crate::virtio::generated::virtio_config::VIRTIO_F_VERSION_1;
use crate::virtio::generated::virtio_ids::VIRTIO_ID_BLOCK;
use crate::virtio::queue::Queue;
use vm_memory::ByteValued;

/// A virtio-blk device backed by a host file, for
/// [`crate::hvf::virtio_mmio::VirtioMmioTransport`].
#[derive(Debug)]
pub struct VirtioBlkMmio {
    disk: DiskProperties,
    config_space: ConfigSpace,
    read_only: bool,
}

impl VirtioBlkMmio {
    /// Opens `disk_image_path` as this device's backing file.
    /// `disk_id` is the serial returned for `VIRTIO_BLK_T_GET_ID`
    /// requests (only its first 20 bytes are used).
    pub fn new(
        disk_image_path: PathBuf,
        read_only: bool,
        disk_id: &str,
    ) -> Result<Self, VirtioBlockError> {
        let disk = DiskProperties::new(disk_image_path, read_only, disk_id)?;
        let config_space = ConfigSpace {
            capacity: disk.nsectors.to_le(),
        };
        Ok(VirtioBlkMmio {
            disk,
            config_space,
            read_only,
        })
    }
}

impl MmioVirtioDevice for VirtioBlkMmio {
    fn device_id(&self) -> u32 {
        VIRTIO_ID_BLOCK
    }

    fn avail_features(&self) -> u64 {
        // Always advertise flush: this device always opens the
        // backing file writeback and fsyncs on guest flush requests
        // (matching virtio::block::device::VirtioBlock's own choice).
        let mut features = (1u64 << VIRTIO_F_VERSION_1) | (1u64 << VIRTIO_BLK_F_FLUSH);
        if self.read_only {
            features |= 1u64 << crate::virtio::generated::virtio_blk::VIRTIO_BLK_F_RO;
        }
        features
    }

    fn num_queues(&self) -> usize {
        1
    }

    fn queue_max_size(&self, _index: usize) -> u16 {
        BLOCK_QUEUE_SIZES[0]
    }

    fn read_config(&self, offset: u64, data: &mut [u8]) {
        if let Some(bytes) = usize::try_from(offset)
            .ok()
            .and_then(|offset| self.config_space.as_slice().get(offset..))
        {
            let len = bytes.len().min(data.len());
            data[..len].copy_from_slice(&bytes[..len]);
        }
    }

    fn write_config(&mut self, _offset: u64, _data: &[u8]) {
        // The block device's own configuration space (just `capacity`)
        // is read-only for the driver; nothing to accept here.
    }

    fn process_queue(&mut self, _index: usize, queue: &mut Queue, mem: &GuestMemoryMmap) -> bool {
        let mut used_any = false;

        loop {
            let head = match queue.pop_or_enable_notification() {
                Ok(Some(head)) => head,
                Ok(None) => break,
                Err(err) => {
                    tracing::error!("hvf::virtio_blk: invalid avail idx: {err}");
                    break;
                }
            };

            let finished = match Request::parse(&head, mem, self.disk.nsectors) {
                Ok(request) => request.process(&mut self.disk, head.index, mem),
                Err(err) => {
                    tracing::error!("hvf::virtio_blk: failed to parse descriptor chain: {err}");
                    FinishedRequest {
                        num_bytes_to_mem: 0,
                        desc_idx: head.index,
                    }
                }
            };

            used_any = true;
            if let Err(err) = queue.add_used(finished.desc_idx, finished.num_bytes_to_mem) {
                tracing::error!("hvf::virtio_blk: failed to add used descriptor: {err}");
            }
        }

        queue.advance_used_ring_idx();
        used_any && queue.prepare_kick()
    }
}
