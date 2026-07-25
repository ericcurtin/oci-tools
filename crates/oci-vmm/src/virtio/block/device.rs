// Copyright 2020 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Portions Copyright 2017 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the THIRD-PARTY file.
// Ported into oci-vmm from Firecracker (src/vmm/src/devices/virtio/block/virtio/{device.rs,event_handler.rs}), trimmed of metrics/snapshot/rate-limiting/async.

//! The virtio block device: a host file exposed to the guest as a disk.

use std::cmp;
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use event_manager::{EventOps, Events, MutEventSubscriber};
use tracing::{error, warn};
use vm_memory::ByteValued;
use vmm_sys_util::epoll::EventSet;
use vmm_sys_util::eventfd::EventFd;

use super::io::SyncFileEngine;
use super::request::{FinishedRequest, Request};
use super::{BLOCK_QUEUE_SIZES, SECTOR_SHIFT, SECTOR_SIZE, VirtioBlockError};
use crate::mem::GuestMemoryMmap;
use crate::virtio::ActivateError;
use crate::virtio::device::{ActiveState, DeviceState, VirtioDevice, VirtioDeviceType};
use crate::virtio::generated::virtio_blk::{
    VIRTIO_BLK_F_FLUSH, VIRTIO_BLK_F_RO, VIRTIO_BLK_ID_BYTES,
};
use crate::virtio::generated::virtio_config::VIRTIO_F_VERSION_1;
use crate::virtio::generated::virtio_ring::VIRTIO_RING_F_EVENT_IDX;
use crate::virtio::queue::{InvalidAvailIdx, Queue};
use crate::virtio::transport::{VirtioInterrupt, VirtioInterruptType};

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
/// of which Firecracker exposes only the mandatory `capacity` field).
#[derive(Debug, Default, Clone, Copy, Eq, PartialEq)]
#[repr(C)]
pub struct ConfigSpace {
    /// Disk capacity in `SECTOR_SIZE` sectors, little-endian.
    pub capacity: u64,
}

// SAFETY: `ConfigSpace` contains only PODs in `repr(C)` or `repr(transparent)`, without padding.
#[allow(unsafe_code)]
unsafe impl ByteValued for ConfigSpace {}

/// Virtio device for exposing block level read/write operations on a host file.
#[derive(Debug)]
pub struct VirtioBlock {
    /// Features the device offers to the driver.
    pub avail_features: u64,
    /// Features the driver has acknowledged.
    pub acked_features: u64,
    /// The device configuration space.
    pub config_space: ConfigSpace,
    /// Eventfd the device signals on activation so the event loop can
    /// swap its registration from the activate event to the queue event.
    pub activate_evt: EventFd,

    /// The device's single virtio queue.
    pub queues: Vec<Queue>,
    /// Eventfds the driver signals when the queue has new buffers.
    pub queue_evts: [EventFd; 1],
    /// Whether the device has been activated, and the guest memory and
    /// interrupt it was activated with.
    pub device_state: DeviceState,

    /// Unique identifier of the drive; also its `VIRTIO_BLK_T_GET_ID` serial.
    pub id: String,
    /// Whether the drive is exposed read-only (`VIRTIO_BLK_F_RO`).
    pub read_only: bool,

    /// Host file and properties.
    pub disk: DiskProperties,
}

impl VirtioBlock {
    /// Create a new virtio block device that operates on the given file.
    ///
    /// The given file must be seekable and sizable. `disk_id` is the serial
    /// returned to the guest for `VIRTIO_BLK_T_GET_ID` requests; only its
    /// first `VIRTIO_BLK_ID_BYTES` (20) bytes are used.
    pub fn new(
        disk_image_path: PathBuf,
        is_read_only: bool,
        disk_id: String,
    ) -> Result<VirtioBlock, VirtioBlockError> {
        let disk_properties = DiskProperties::new(disk_image_path, is_read_only, &disk_id)?;

        // Always advertise the flush command: this device always opens the
        // backing file writeback and fsyncs on guest flush requests
        // (Firecracker's `CacheType::Writeback`).
        let mut avail_features = (1u64 << VIRTIO_F_VERSION_1)
            | (1u64 << VIRTIO_RING_F_EVENT_IDX)
            | (1u64 << VIRTIO_BLK_F_FLUSH);

        if is_read_only {
            avail_features |= 1u64 << VIRTIO_BLK_F_RO;
        };

        let queue_evts = [EventFd::new(libc::EFD_NONBLOCK).map_err(VirtioBlockError::EventFd)?];

        let queues = BLOCK_QUEUE_SIZES.iter().map(|&s| Queue::new(s)).collect();

        let config_space = ConfigSpace {
            capacity: disk_properties.nsectors.to_le(),
        };

        Ok(VirtioBlock {
            avail_features,
            acked_features: 0u64,
            config_space,
            activate_evt: EventFd::new(libc::EFD_NONBLOCK).map_err(VirtioBlockError::EventFd)?,

            queues,
            queue_evts,
            device_state: DeviceState::Inactive,

            id: disk_id,
            read_only: is_read_only,

            disk: disk_properties,
        })
    }

    /// Process a single event in the Virtio queue.
    ///
    /// This function is called by the event manager when the guest notifies us
    /// about new buffers in the queue.
    pub fn process_queue_event(&mut self) {
        if let Err(err) = self.queue_evts[0].read() {
            error!("Failed to get queue event: {err:?}");
        } else {
            self.process_virtio_queues().unwrap()
        }
    }

    /// Process device virtio queue(s).
    pub fn process_virtio_queues(&mut self) -> Result<(), InvalidAvailIdx> {
        self.process_queue(0)
    }

    /// Device specific function for peaking inside a queue and processing descriptors.
    pub fn process_queue(&mut self, queue_index: usize) -> Result<(), InvalidAvailIdx> {
        // This is safe since we checked in the event handler that the device is activated.
        let active_state = self.device_state.active_state().unwrap();

        let queue = &mut self.queues[queue_index];
        let mut used_any = false;

        while let Some(head) = queue.pop_or_enable_notification()? {
            let finished = match Request::parse(&head, &active_state.mem, self.disk.nsectors) {
                Ok(request) => request.process(&mut self.disk, head.index, &active_state.mem),
                Err(err) => {
                    error!("Failed to parse available descriptor chain: {err:?}");
                    FinishedRequest {
                        num_bytes_to_mem: 0,
                        desc_idx: head.index,
                    }
                }
            };

            used_any = true;
            queue
                .add_used(head.index, finished.num_bytes_to_mem)
                .unwrap_or_else(|err| {
                    error!(
                        "Failed to add available descriptor head {}: {}",
                        head.index, err
                    )
                });
        }
        queue.advance_used_ring_idx();

        if used_any && queue.prepare_kick() {
            active_state
                .interrupt
                .trigger(VirtioInterruptType::Queue(0))
                .unwrap_or_else(|err| {
                    error!("Failed to signal used queue: {err:?}");
                });
        }

        Ok(())
    }
}

impl VirtioDevice for VirtioBlock {
    fn const_device_type() -> VirtioDeviceType {
        VirtioDeviceType::Block
    }

    fn device_type(&self) -> VirtioDeviceType {
        Self::const_device_type()
    }

    fn id(&self) -> &str {
        &self.id
    }

    fn avail_features(&self) -> u64 {
        self.avail_features
    }

    fn acked_features(&self) -> u64 {
        self.acked_features
    }

    fn set_acked_features(&mut self, acked_features: u64) {
        self.acked_features = acked_features;
    }

    fn queues(&self) -> &[Queue] {
        &self.queues
    }

    fn queues_mut(&mut self) -> &mut [Queue] {
        &mut self.queues
    }

    fn queue_events(&self) -> &[EventFd] {
        &self.queue_evts
    }

    fn interrupt_trigger(&self) -> &dyn VirtioInterrupt {
        self.device_state
            .active_state()
            .expect("Device is not initialized")
            .interrupt
            .deref()
    }

    fn read_config(&self, offset: u64, data: &mut [u8]) {
        if let Some(config_space_bytes) = usize::try_from(offset)
            .ok()
            .and_then(|offset| self.config_space.as_slice().get(offset..))
        {
            let len = config_space_bytes.len().min(data.len());
            data[..len].copy_from_slice(&config_space_bytes[..len]);
        } else {
            error!("Failed to read config space");
        }
    }

    fn write_config(&mut self, offset: u64, data: &[u8]) {
        let config_space_bytes = self.config_space.as_mut_slice();
        let start = usize::try_from(offset).ok();
        let end = start.and_then(|s| s.checked_add(data.len()));
        let Some(dst) = start
            .zip(end)
            .and_then(|(start, end)| config_space_bytes.get_mut(start..end))
        else {
            error!("Failed to write config space");
            return;
        };

        dst.copy_from_slice(data);
    }

    fn activate(
        &mut self,
        mem: GuestMemoryMmap,
        interrupt: Arc<dyn VirtioInterrupt>,
    ) -> Result<(), ActivateError> {
        for q in self.queues.iter_mut() {
            q.initialize(&mem)
                .map_err(ActivateError::QueueMemoryError)?;
        }

        let event_idx = self.has_feature(u64::from(VIRTIO_RING_F_EVENT_IDX));
        if event_idx {
            for queue in &mut self.queues {
                queue.enable_notif_suppression();
            }
        }

        if self.activate_evt.write(1).is_err() {
            return Err(ActivateError::EventFd);
        }
        self.device_state = DeviceState::Activated(ActiveState { mem, interrupt });
        Ok(())
    }

    fn is_activated(&self) -> bool {
        self.device_state.is_activated()
    }
}

impl VirtioBlock {
    const PROCESS_ACTIVATE: u32 = 0;
    const PROCESS_QUEUE: u32 = 1;

    fn register_runtime_events(&self, ops: &mut EventOps) {
        if let Err(err) = ops.add(Events::with_data(
            &self.queue_evts[0],
            Self::PROCESS_QUEUE,
            EventSet::IN,
        )) {
            error!("Failed to register queue event: {err}");
        }
    }

    fn register_activate_event(&self, ops: &mut EventOps) {
        if let Err(err) = ops.add(Events::with_data(
            &self.activate_evt,
            Self::PROCESS_ACTIVATE,
            EventSet::IN,
        )) {
            error!("Failed to register activate event: {err}");
        }
    }

    fn process_activate_event(&self, ops: &mut EventOps) {
        if let Err(err) = self.activate_evt.read() {
            error!("Failed to consume block activate event: {err:?}");
        }
        self.register_runtime_events(ops);
        if let Err(err) = ops.remove(Events::with_data(
            &self.activate_evt,
            Self::PROCESS_ACTIVATE,
            EventSet::IN,
        )) {
            error!("Failed to un-register activate event: {err}");
        }
    }
}

impl MutEventSubscriber for VirtioBlock {
    // Handle an event for the queue.
    fn process(&mut self, event: Events, ops: &mut EventOps) {
        let source = event.data();
        let event_set = event.event_set();

        let supported_events = EventSet::IN;
        if !supported_events.contains(event_set) {
            warn!(
                "Block: Received unknown event: {:?} from source: {:?}",
                event_set, source
            );
            return;
        }

        if self.is_activated() {
            match source {
                Self::PROCESS_ACTIVATE => self.process_activate_event(ops),
                Self::PROCESS_QUEUE => self.process_queue_event(),
                _ => warn!("Block: Spurious event received: {:?}", source),
            }
        } else {
            warn!(
                "Block: The device is not yet activated. Spurious event received: {:?}",
                source
            );
        }
    }

    fn init(&mut self, ops: &mut EventOps) {
        // This function can be called during different points in the device lifetime:
        //  - shortly after device creation,
        //  - on device activation (is-activated already true at this point).
        if self.is_activated() {
            self.register_runtime_events(ops);
        } else {
            self.register_activate_event(ops);
        }
    }
}

impl Drop for VirtioBlock {
    fn drop(&mut self) {
        // Firecracker's `Writeback` drop path: sync data out to physical
        // media before the file is closed.
        if let Err(err) = self.disk.file_engine.flush() {
            error!("Failed to flush block data on drop: {err:?}");
        }
    }
}
