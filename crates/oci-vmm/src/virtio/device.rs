// Copyright 2020 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Portions Copyright 2017 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the THIRD-PARTY file.

// Ported into oci-vmm from Firecracker (src/vmm/src/devices/virtio/device.rs), trimmed of metrics/snapshot/MMIO.

//! The [`VirtioDevice`] trait: the interface between a virtio device
//! implementation and the transport that drives it.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::AtomicU32;

use event_manager::MutEventSubscriber;
use tracing::{error, info, warn};
use vmm_sys_util::eventfd::EventFd;

use super::ActivateError;
use super::queue::Queue;
use super::transport::VirtioInterrupt;
use crate::mem::GuestMemoryMmap;
use crate::virtio::AsAny;
use crate::virtio::generated::virtio_ids;

/// State of an active VirtIO device
#[derive(Debug, Clone)]
pub struct ActiveState {
    /// Guest memory attached to the device.
    pub mem: GuestMemoryMmap,
    /// Interrupt the device uses to notify the guest.
    pub interrupt: Arc<dyn VirtioInterrupt>,
}

/// Enum that indicates if a VirtioDevice is inactive or has been activated
/// and memory attached to it.
#[derive(Debug)]
pub enum DeviceState {
    /// The device is not yet activated.
    Inactive,
    /// The device has been activated by the driver.
    Activated(ActiveState),
}

impl DeviceState {
    /// Checks if the device is activated.
    pub fn is_activated(&self) -> bool {
        match self {
            DeviceState::Inactive => false,
            DeviceState::Activated(_) => true,
        }
    }

    /// Gets the memory and interrupt attached to the device if it is activated.
    pub fn active_state(&self) -> Option<&ActiveState> {
        match self {
            DeviceState::Activated(state) => Some(state),
            DeviceState::Inactive => None,
        }
    }
}

/// Type of a virtio device
/// Represent it as u8 to give it a known size.
/// All used types fit in u8.
#[allow(clippy::cast_possible_truncation, missing_docs)]
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VirtioDeviceType {
    Net = virtio_ids::VIRTIO_ID_NET as u8,
    Block = virtio_ids::VIRTIO_ID_BLOCK as u8,
    Rng = virtio_ids::VIRTIO_ID_RNG as u8,
    Balloon = virtio_ids::VIRTIO_ID_BALLOON as u8,
    Vsock = virtio_ids::VIRTIO_ID_VSOCK as u8,
    Mem = virtio_ids::VIRTIO_ID_MEM as u8,
    Pmem = virtio_ids::VIRTIO_ID_PMEM as u8,
}

/// Unique identifier for a virtio device: its type and string ID.
pub type VirtioDeviceId = (VirtioDeviceType, String);

/// Trait for virtio devices to be driven by a virtio transport.
///
/// The lifecycle of a virtio device is to be moved to a virtio transport, which will then query the
/// device. The virtio devices needs to create queues, events and event fds for interrupts and
/// expose them to the transport via get_queues/get_queue_events/get_interrupt/get_interrupt_status
/// fns.
pub trait VirtioDevice: AsAny + MutEventSubscriber + Send {
    /// Get the available features offered by device.
    fn avail_features(&self) -> u64;

    /// Get acknowledged features of the driver.
    fn acked_features(&self) -> u64;

    /// Set acknowledged features of the driver.
    /// This function must maintain the following invariant:
    /// - self.avail_features() & self.acked_features() = self.get_acked_features()
    fn set_acked_features(&mut self, acked_features: u64);

    /// Check if virtio device has negotiated given feature.
    fn has_feature(&self, feature: u64) -> bool {
        (self.acked_features() & (1 << feature)) != 0
    }

    /// The virtio device type (as a constant of the struct).
    fn const_device_type() -> VirtioDeviceType
    where
        Self: Sized;

    /// The virtio device type.
    ///
    /// It should be the same as returned by Self::const_device_type().
    fn device_type(&self) -> VirtioDeviceType;

    /// Returns unique device id
    fn id(&self) -> &str;

    /// Returns the device queues.
    fn queues(&self) -> &[Queue];

    /// Returns a mutable reference to the device queues.
    fn queues_mut(&mut self) -> &mut [Queue];

    /// Returns the device queues event fds.
    fn queue_events(&self) -> &[EventFd];

    /// Returns the current device interrupt status.
    fn interrupt_status(&self) -> Arc<AtomicU32> {
        self.interrupt_trigger().status()
    }

    /// Returns the interrupt the device uses to notify the guest.
    fn interrupt_trigger(&self) -> &dyn VirtioInterrupt;

    /// The set of feature bits shifted by `page * 32`.
    fn avail_features_by_page(&self, page: u32) -> u32 {
        let avail_features = self.avail_features();
        match page {
            // Get the lower 32-bits of the features bitfield.
            0 => (avail_features & 0xFFFFFFFF) as u32,
            // Get the upper 32-bits of the features bitfield.
            1 => (avail_features >> 32) as u32,
            _ => {
                warn!("Received request for unknown features page.");
                0u32
            }
        }
    }

    /// Acknowledges that this set of features should be enabled.
    fn ack_features_by_page(&mut self, page: u32, value: u32) {
        let mut v = match page {
            0 => u64::from(value),
            1 => u64::from(value) << 32,
            _ => {
                warn!("Cannot acknowledge unknown features page: {page}");
                0u64
            }
        };

        // Check if the guest is ACK'ing a feature that we didn't claim to have.
        let avail_features = self.avail_features();
        let unrequested_features = v & !avail_features;
        if unrequested_features != 0 {
            warn!("Received acknowledge request for unknown feature: {v:#x}");
            // Don't count these features as acked.
            v &= !unrequested_features;
        }
        self.set_acked_features(self.acked_features() | v);
    }

    /// Reads this device configuration space at `offset`.
    fn read_config(&self, offset: u64, data: &mut [u8]);

    /// Writes to this device configuration space at `offset`.
    fn write_config(&mut self, offset: u64, data: &[u8]);

    /// Performs the formal activation for a device, which can be verified also with `is_activated`.
    fn activate(
        &mut self,
        mem: GuestMemoryMmap,
        interrupt: Arc<dyn VirtioInterrupt>,
    ) -> Result<(), ActivateError>;

    /// Checks if the resources of this device are activated.
    fn is_activated(&self) -> bool;

    /// Optionally deactivates this device and returns ownership of the guest memory map, interrupt
    /// event, and queue events.
    fn reset(&mut self) -> Option<(Arc<dyn VirtioInterrupt>, Vec<EventFd>)> {
        None
    }

    /// Notify all queues by writing to the eventfds.
    fn notify_queue_events(&mut self) {
        info!("[{:?}:{}] notifying queues", self.device_type(), self.id());
        for (i, eventfd) in self.queue_events().iter().enumerate() {
            if let Err(err) = eventfd.write(1) {
                error!(
                    "[{:?}:{}] error notifying queue {i}: {err}",
                    self.device_type(),
                    self.id(),
                );
            }
        }
    }

    /// Kick the device, as if it had received external events.
    fn kick(&mut self) {
        if self.is_activated() {
            self.notify_queue_events();
        }
    }
}

impl fmt::Debug for dyn VirtioDevice {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "VirtioDevice type {:?}", self.device_type())
    }
}

/// Utility to define both const_device_type and device_type with a u32 constant
#[macro_export]
macro_rules! impl_device_type {
    ($const_type:expr) => {
        fn const_device_type() -> VirtioDeviceType {
            $const_type
        }

        fn device_type(&self) -> VirtioDeviceType {
            Self::const_device_type()
        }
    };
}
