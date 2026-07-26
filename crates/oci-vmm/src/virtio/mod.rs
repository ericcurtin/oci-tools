// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Portions Copyright 2017 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the THIRD-PARTY file.

// Ported into oci-vmm from Firecracker (src/vmm/src/devices/virtio/mod.rs), trimmed of metrics/snapshot/MMIO.

//! Implements virtio devices, queues, and transport mechanisms.

use std::any::Any;

use self::queue::QueueError;

pub mod block;
// EventFd/event_manager-driven: the interface the KVM/x86_64 PCI
// transport (`transport::pci`) drives a device through. `hvf`'s own
// virtio-mmio transport (docs/design/0249 phase 4) is a synchronous,
// dispatched-from-the-vCPU-exit-loop design instead (matching every
// other `hvf` device, e.g. `hvf::pl011`) and has no equivalent trait
// of its own to share -- see `hvf::virtio_mmio`.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub mod device;
pub mod generated;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub mod net;
pub mod queue;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub mod transport;

/// Errors triggered when activating a VirtioDevice.
#[derive(Debug, thiserror::Error)]
pub enum ActivateError {
    /// Wrong number of queues for the virtio device.
    #[error("Wrong number of queue for virtio device: expected {expected}, got {got}")]
    QueueMismatch {
        /// The number of queues the device expects.
        expected: usize,
        /// The number of queues the driver configured.
        got: usize,
    },
    /// Failed to write to activate eventfd.
    #[error("Failed to write to activate eventfd")]
    EventFd,
    /// Error setting pointers in the queue.
    #[error("Error setting pointers in the queue: {0}")]
    QueueMemoryError(QueueError),
    /// The driver didn't acknowledge a required feature.
    #[error("The driver didn't acknowledge a required feature: {0}")]
    RequiredFeatureNotAcked(&'static str),
}

/// Trait that helps in upcasting an object to Any
pub trait AsAny {
    /// Return the immutable any encapsulated object.
    fn as_any(&self) -> &dyn Any;

    /// Return the mutable encapsulated any object.
    fn as_mut_any(&mut self) -> &mut dyn Any;
}

impl<T: Any> AsAny for T {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_mut_any(&mut self) -> &mut dyn Any {
        self
    }
}
