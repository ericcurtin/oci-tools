// Copyright 2025 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright 2018 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE-BSD-3-Clause file.
//
// Copyright © 2019 Intel Corporation
//
// SPDX-License-Identifier: Apache-2.0 AND BSD-3-Clause

// Ported into oci-vmm from Firecracker (src/vmm/src/devices/virtio/transport/pci/common_config.rs), trimmed of metrics/snapshot/MMIO.

//! The virtio-pci common configuration structure: feature negotiation,
//! device status and virtqueue selection/configuration.

use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};

use tracing::warn;
use vm_memory::GuestAddress;

use crate::virtio::device::VirtioDevice;
use crate::virtio::queue::Queue;
use crate::virtio::transport::pci::common_config_offset::*;
use crate::virtio::transport::pci::device::VIRTQ_MSI_NO_VECTOR;
use crate::virtio::transport::pci::device_status::*;

/// Initial values for constructing a [`VirtioPciCommonConfig`].
#[derive(Debug, Clone)]
pub struct VirtioPciCommonConfigState {
    /// Device status written by the driver.
    pub driver_status: u8,
    /// Configuration generation counter.
    pub config_generation: u8,
    /// Selector for the device feature page.
    pub device_feature_select: u32,
    /// Selector for the driver feature page.
    pub driver_feature_select: u32,
    /// Currently selected virtqueue.
    pub queue_select: u16,
    /// MSI-X vector used for configuration change notifications.
    pub msix_config: u16,
    /// MSI-X vector per virtqueue.
    pub msix_queues: Vec<u16>,
}

/// Contains the data for reading and writing the common configuration structure of a virtio PCI
/// device.
#[derive(Debug)]
pub struct VirtioPciCommonConfig {
    /// Device status written by the driver.
    pub driver_status: u8,
    /// Configuration generation counter.
    pub config_generation: u8,
    /// Selector for the device feature page.
    pub device_feature_select: u32,
    /// Selector for the driver feature page.
    pub driver_feature_select: u32,
    /// Currently selected virtqueue.
    pub queue_select: u16,
    /// MSI-X vector used for configuration change notifications.
    pub msix_config: Arc<AtomicU16>,
    /// MSI-X vector per virtqueue.
    pub msix_queues: Arc<Mutex<Vec<u16>>>,
}

impl VirtioPciCommonConfig {
    /// Create a new [`VirtioPciCommonConfig`] from initial values.
    pub fn new(state: VirtioPciCommonConfigState) -> Self {
        VirtioPciCommonConfig {
            driver_status: state.driver_status,
            config_generation: state.config_generation,
            device_feature_select: state.device_feature_select,
            driver_feature_select: state.driver_feature_select,
            queue_select: state.queue_select,
            msix_config: Arc::new(AtomicU16::new(state.msix_config)),
            msix_queues: Arc::new(Mutex::new(state.msix_queues)),
        }
    }

    /// Handle a driver read from the common configuration region.
    pub fn read(&mut self, offset: u64, data: &mut [u8], device: Arc<Mutex<dyn VirtioDevice>>) {
        assert!(data.len() <= 8);

        match data.len() {
            1 => {
                let v = self.read_common_config_byte(offset);
                data[0] = v;
            }
            2 => {
                let v = self.read_common_config_word(offset, device.lock().unwrap().queues());
                data.copy_from_slice(&v.to_le_bytes());
            }
            4 => {
                let v = self.read_common_config_dword(offset, device);
                data.copy_from_slice(&v.to_le_bytes());
            }
            _ => warn!(
                "pci: invalid data length for virtio read: len {}",
                data.len()
            ),
        }
    }

    /// Handle a driver write to the common configuration region.
    pub fn write(
        &mut self,
        offset: u64,
        data: &[u8],
        device: Arc<Mutex<dyn VirtioDevice>>,
        device_activated: bool,
    ) {
        assert!(data.len() <= 8);

        match data.len() {
            1 => self.write_common_config_byte(offset, data[0], device_activated),
            2 => self.write_common_config_word(
                offset,
                u16::from_le_bytes(data.try_into().unwrap()),
                device.lock().unwrap().queues_mut(),
            ),
            4 => self.write_common_config_dword(
                offset,
                u32::from_le_bytes(data.try_into().unwrap()),
                device,
            ),
            _ => warn!(
                "pci: invalid data length for virtio write: len {}",
                data.len()
            ),
        }
    }

    fn read_common_config_byte(&self, offset: u64) -> u8 {
        // The driver is only allowed to do aligned, properly sized access.
        match offset {
            DEVICE_STATUS => self.driver_status,
            CONFIG_GENERATION => self.config_generation,
            _ => {
                warn!("pci: invalid virtio config byte read: 0x{offset:x}");
                0
            }
        }
    }

    fn write_common_config_byte(&mut self, offset: u64, value: u8, device_activated: bool) {
        match offset {
            DEVICE_STATUS => self.set_device_status(value, device_activated),
            _ => {
                warn!("pci: invalid virtio config byte write: 0x{offset:x}");
            }
        }
    }

    fn set_device_status(&mut self, status: u8, device_activated: bool) {
        /// Enforce the device status state machine per the virtio spec:
        ///   INIT -> ACKNOWLEDGE -> DRIVER -> FEATURES_OK -> DRIVER_OK
        /// https://docs.oasis-open.org/virtio/virtio/v1.3/csd01/virtio-v1.3-csd01.html#x1-1220001
        ///
        /// Each step sets exactly one new bit while preserving all previous bits.
        const VALID_TRANSITIONS: &[(u8, u8)] = &[
            (INIT, ACKNOWLEDGE),
            (ACKNOWLEDGE, ACKNOWLEDGE | DRIVER),
            (ACKNOWLEDGE | DRIVER, ACKNOWLEDGE | DRIVER | FEATURES_OK),
            (
                ACKNOWLEDGE | DRIVER | FEATURES_OK,
                ACKNOWLEDGE | DRIVER | FEATURES_OK | DRIVER_OK,
            ),
        ];

        if (status & FAILED) != 0 {
            // Something went wrong in the guest.
            //
            // https://docs.oasis-open.org/virtio/virtio/v1.3/csd01/virtio-v1.3-csd01.html#x1-110001
            // > FAILED (128)
            // >     Indicates that something went wrong in the guest, and it has given up on the
            // >     device.
            self.driver_status |= FAILED;
        } else if status == INIT {
            // Reset requested by the driver.
            //
            // https://docs.oasis-open.org/virtio/virtio/v1.3/csd01/virtio-v1.3-csd01.html#x1-1430001
            // > The device MUST reset when 0 is written to device_status, and present a 0 in
            // > device_status once that is done.
            //
            // https://docs.oasis-open.org/virtio/virtio/v1.3/csd01/virtio-v1.3-csd01.html#x1-1440002
            // > After writing 0 to device_status, the driver MUST wait for a read of device_status
            // > to return 0 before reinitializing the device.
            //
            // https://docs.oasis-open.org/virtio/virtio/v1.3/csd01/virtio-v1.3-csd01.html#x1-200001
            // > 2.4.1 Device Requirements: Device Reset
            // > A device MUST reinitialize device status to 0 after receiving a reset.
            //
            // Setting INIT (0) here before the actual reset completes in write_bar() may appear
            // racy - the driver could read 0 before the device is fully torn down.  But concurrent
            // access is serialized since VirtioPciDevice is accessed through Arc<Mutex<>>.
            self.driver_status = INIT;
        } else if VALID_TRANSITIONS
            .iter()
            .any(|&(from, to)| self.driver_status == from && status == to)
        {
            if !device_activated {
                self.driver_status = status;
            } else {
                // If the device doesn't implement reset(), the device is left activated.
                // Re-initialization against a still-live backend device MUST be rejected.
                warn!(
                    "pci: rejecting device status transition {:#x} -> {:#x}: \
                     previous reset did not complete successfully and device is still active",
                    self.driver_status, status
                );
            }
        } else {
            warn!(
                "pci: invalid virtio device status transition: {:#x} -> {:#x}",
                self.driver_status, status
            );
        }
    }

    fn read_common_config_word(&self, offset: u64, queues: &[Queue]) -> u16 {
        match offset {
            MSIX_CONFIG => self.msix_config.load(Ordering::Acquire),
            NUM_QUEUES => queues.len().try_into().unwrap(),
            QUEUE_SELECT => self.queue_select,
            QUEUE_SIZE => self.with_queue(queues, |q| q.size).unwrap_or(0),
            // If `queue_select` points to an invalid queue we should return NO_VECTOR.
            // Reading from here
            // https://docs.oasis-open.org/virtio/virtio/v1.1/csprd01/virtio-v1.1-csprd01.html#x1-1280005:
            //
            // > The device MUST return vector mapped to a given event, (NO_VECTOR if unmapped) on
            // > read of config_msix_vector/queue_msix_vector.
            QUEUE_MSIX_VECTOR => self
                .msix_queues
                .lock()
                .unwrap()
                .get(self.queue_select as usize)
                .copied()
                .unwrap_or(VIRTQ_MSI_NO_VECTOR),
            QUEUE_ENABLE => u16::from(self.with_queue(queues, |q| q.ready).unwrap_or(false)),
            QUEUE_NOTIFY_OFF => self.queue_select,
            _ => {
                warn!("pci: invalid virtio register word read: 0x{offset:x}");
                0
            }
        }
    }

    /// Guard queue configuration field writes based on device status.
    ///
    /// Per the virtio spec, the driver SHALL follow this sequence:
    ///   INIT -> ACKNOWLEDGE -> DRIVER -> FEATURES_OK -> DRIVER_OK
    /// https://docs.oasis-open.org/virtio/virtio/v1.3/csd01/virtio-v1.3-csd01.html#x1-1220001
    ///
    /// Queue configuration must only be done between FEATURES_OK and DRIVER_OK.
    fn update_queue_field<F: FnOnce(&mut Queue)>(&mut self, queues: &mut [Queue], f: F) {
        let status = self.driver_status;
        if status == (ACKNOWLEDGE | DRIVER | FEATURES_OK) {
            self.with_queue_mut(queues, f);
        } else {
            warn!("pci: queue config write not allowed in device status {status:#x}");
        }
    }

    fn write_common_config_word(&mut self, offset: u64, value: u16, queues: &mut [Queue]) {
        match offset {
            MSIX_CONFIG => {
                // Make sure that the guest doesn't select an invalid vector. We are offering
                // `num_queues + 1` vectors (plus one for configuration updates). If an invalid
                // vector has been selected, we just store the `NO_VECTOR` value.
                let msix_queues = self.msix_queues.lock().expect("Poisoned lock");
                let nr_vectors = msix_queues.len() + 1;

                if (value as usize) < nr_vectors {
                    self.msix_config.store(value, Ordering::Release);
                } else {
                    self.msix_config
                        .store(VIRTQ_MSI_NO_VECTOR, Ordering::Release);
                }
            }
            QUEUE_SELECT => self.queue_select = value,
            QUEUE_SIZE => self.update_queue_field(queues, |q| q.size = value),
            QUEUE_MSIX_VECTOR => {
                let mut msix_queues = self.msix_queues.lock().expect("Poisoned lock");
                let nr_vectors = msix_queues.len() + 1;
                // Make sure that `queue_select` points to a valid queue. If not, we won't do
                // anything here and subsequent reads at 0x1a will return `NO_VECTOR`.
                if let Some(queue) = msix_queues.get_mut(self.queue_select as usize) {
                    // Make sure that the guest doesn't select an invalid vector. We are offering
                    // `num_queues + 1` vectors (plus one for configuration updates). If an invalid
                    // vector has been selected, we just store the `NO_VECTOR` value.
                    if (value as usize) < nr_vectors {
                        *queue = value;
                    } else {
                        *queue = VIRTQ_MSI_NO_VECTOR;
                    }
                }
            }
            QUEUE_ENABLE => self.update_queue_field(queues, |q| {
                if value != 0 {
                    q.ready = value == 1;
                }
            }),
            _ => {
                warn!("pci: invalid virtio register word write: 0x{offset:x}");
            }
        }
    }

    fn read_common_config_dword(&self, offset: u64, device: Arc<Mutex<dyn VirtioDevice>>) -> u32 {
        match offset {
            DEVICE_FEATURE_SELECT => self.device_feature_select,
            DEVICE_FEATURE => {
                let locked_device = device.lock().unwrap();
                // Only 64 bits of features (2 pages) are defined for now, so limit
                // device_feature_select to avoid shifting by 64 or more bits.
                if self.device_feature_select < 2 {
                    ((locked_device.avail_features() >> (self.device_feature_select * 32))
                        & 0xffff_ffff) as u32
                } else {
                    0
                }
            }
            DRIVER_FEATURE_SELECT => self.driver_feature_select,
            QUEUE_DESC_LO => {
                let locked_device = device.lock().unwrap();
                self.with_queue(locked_device.queues(), |q| {
                    (q.desc_table_address.0 & 0xffff_ffff) as u32
                })
                .unwrap_or_default()
            }
            QUEUE_DESC_HI => {
                let locked_device = device.lock().unwrap();
                self.with_queue(locked_device.queues(), |q| {
                    (q.desc_table_address.0 >> 32) as u32
                })
                .unwrap_or_default()
            }
            QUEUE_AVAIL_LO => {
                let locked_device = device.lock().unwrap();
                self.with_queue(locked_device.queues(), |q| {
                    (q.avail_ring_address.0 & 0xffff_ffff) as u32
                })
                .unwrap_or_default()
            }
            QUEUE_AVAIL_HI => {
                let locked_device = device.lock().unwrap();
                self.with_queue(locked_device.queues(), |q| {
                    (q.avail_ring_address.0 >> 32) as u32
                })
                .unwrap_or_default()
            }
            QUEUE_USED_LO => {
                let locked_device = device.lock().unwrap();
                self.with_queue(locked_device.queues(), |q| {
                    (q.used_ring_address.0 & 0xffff_ffff) as u32
                })
                .unwrap_or_default()
            }
            QUEUE_USED_HI => {
                let locked_device = device.lock().unwrap();
                self.with_queue(locked_device.queues(), |q| {
                    (q.used_ring_address.0 >> 32) as u32
                })
                .unwrap_or_default()
            }
            _ => {
                warn!("pci: invalid virtio register dword read: 0x{offset:x}");
                0
            }
        }
    }

    fn write_common_config_dword(
        &mut self,
        offset: u64,
        value: u32,
        device: Arc<Mutex<dyn VirtioDevice>>,
    ) {
        fn hi(v: &mut GuestAddress, x: u32) {
            *v = (*v & 0xffff_ffff) | (u64::from(x) << 32)
        }

        fn lo(v: &mut GuestAddress, x: u32) {
            *v = (*v & !0xffff_ffff) | u64::from(x)
        }

        let mut locked_device = device.lock().unwrap();

        match offset {
            DEVICE_FEATURE_SELECT => self.device_feature_select = value,
            DRIVER_FEATURE_SELECT => self.driver_feature_select = value,
            DRIVER_FEATURE => {
                // Feature negotiation is only allowed in DRIVER state.
                // https://docs.oasis-open.org/virtio/virtio/v1.3/csd01/virtio-v1.3-csd01.html#x1-1220001
                if self.driver_status == (ACKNOWLEDGE | DRIVER) {
                    locked_device.ack_features_by_page(self.driver_feature_select, value);
                } else {
                    warn!(
                        "pci: feature negotiation not allowed in device state {:#x}",
                        self.driver_status
                    );
                }
            }
            QUEUE_DESC_LO => self.update_queue_field(locked_device.queues_mut(), |q| {
                lo(&mut q.desc_table_address, value)
            }),
            QUEUE_DESC_HI => self.update_queue_field(locked_device.queues_mut(), |q| {
                hi(&mut q.desc_table_address, value)
            }),
            QUEUE_AVAIL_LO => self.update_queue_field(locked_device.queues_mut(), |q| {
                lo(&mut q.avail_ring_address, value)
            }),
            QUEUE_AVAIL_HI => self.update_queue_field(locked_device.queues_mut(), |q| {
                hi(&mut q.avail_ring_address, value)
            }),
            QUEUE_USED_LO => self.update_queue_field(locked_device.queues_mut(), |q| {
                lo(&mut q.used_ring_address, value)
            }),
            QUEUE_USED_HI => self.update_queue_field(locked_device.queues_mut(), |q| {
                hi(&mut q.used_ring_address, value)
            }),
            _ => {
                warn!("pci: invalid virtio register dword write: 0x{offset:x}");
            }
        }
    }

    fn with_queue<U, F>(&self, queues: &[Queue], f: F) -> Option<U>
    where
        F: FnOnce(&Queue) -> U,
    {
        queues.get(self.queue_select as usize).map(f)
    }

    fn with_queue_mut<F: FnOnce(&mut Queue)>(&self, queues: &mut [Queue], f: F) {
        if let Some(queue) = queues.get_mut(self.queue_select as usize) {
            f(queue);
        }
    }
}
