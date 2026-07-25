// Copyright 2025 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

// Ported into oci-vmm from Firecracker (src/vmm/src/devices/virtio/transport/mod.rs), trimmed of metrics/snapshot/MMIO.

//! Virtio transports. This VMM is PCI-only: the MMIO transport was dropped.

use std::sync::Arc;
use std::sync::atomic::AtomicU32;

use vmm_sys_util::eventfd::EventFd;

use crate::vstate::interrupts::InterruptError;

/// PCI transport for VirtIO devices
pub mod pci;

/// Represents the types of interrupts used by VirtIO devices
#[derive(Debug, Clone)]
pub enum VirtioInterruptType {
    /// Interrupt for VirtIO configuration changes
    Config,
    /// Interrupts for new events in a queue.
    Queue(u16),
}

/// API of interrupt types used by VirtIO devices
pub trait VirtioInterrupt: std::fmt::Debug + Send + Sync {
    /// Trigger a VirtIO interrupt.
    fn trigger(&self, interrupt_type: VirtioInterruptType) -> Result<(), InterruptError>;

    /// Trigger multiple Virtio interrupts for selected queues.
    /// The caller needs to ensure that [`queues`] does not include duplicate entries to
    /// avoid sending multiple interrupts for the same queue.
    /// This is to allow sending a single interrupt for implementations that don't
    /// distinguish different queues, like IrqTrigger, instead of sending multiple same
    /// interrupts.
    fn trigger_queues(&self, queues: &[u16]) -> Result<(), InterruptError> {
        queues
            .iter()
            .try_for_each(|&qidx| self.trigger(VirtioInterruptType::Queue(qidx)))
    }

    /// Get the `EventFd` (if any) that backs the underlying interrupt.
    fn notifier(&self, _interrupt_type: VirtioInterruptType) -> Option<&EventFd> {
        None
    }

    /// Get the current device interrupt status.
    fn status(&self) -> Arc<AtomicU32>;
}
