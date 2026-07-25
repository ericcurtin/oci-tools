// Copyright 2021 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Portions Copyright 2017 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the THIRD-PARTY file.
// Ported into oci-vmm from Firecracker (src/vmm/src/devices/legacy/serial.rs), trimmed of metrics/snapshot/ACPI.

//! Implements a wrapper over an UART serial device: a `vm_superio` 16550
//! writing to stdout, with an optional readable input file whose bytes the
//! VMM event loop feeds into the FIFO.

use std::fs::File;
use std::io::{self, Read, Stdout};
use std::ops::Deref;

use tracing::{debug, error};
use vm_superio::serial::{Error as SerialError, NoEvents};
use vm_superio::{Serial, Trigger};
use vmm_sys_util::eventfd::EventFd;

/// Wrapper for implementing the trigger functionality for `EventFd`.
///
/// The trigger is used for handling events in the legacy devices.
#[derive(Debug)]
pub struct EventFdTrigger(EventFd);

impl Trigger for EventFdTrigger {
    type E = io::Error;

    fn trigger(&self) -> io::Result<()> {
        self.write(1)
    }
}

impl Deref for EventFdTrigger {
    type Target = EventFd;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl EventFdTrigger {
    /// Clone an `EventFdTrigger`.
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(EventFdTrigger((**self).try_clone()?))
    }

    /// Create an `EventFdTrigger`.
    pub fn new(evt: EventFd) -> Self {
        Self(evt)
    }

    /// Get the associated event fd out of an `EventFdTrigger`.
    pub fn get_event(&self) -> EventFd {
        self.0.try_clone().unwrap()
    }
}

/// Errors thrown while feeding input into the serial device.
#[derive(Debug, thiserror::Error)]
pub enum SerialDeviceError {
    /// The UART RX FIFO cannot hold the input.
    #[error("Serial FIFO full")]
    FullFifo,
    /// Error coming from the wrapped `vm_superio` serial.
    #[error("Serial error: {0:?}")]
    Serial(SerialError<io::Error>),
}

/// Wrapper over the `vm_superio` 16550 UART, writing to stdout.
#[derive(Debug)]
pub struct SerialDevice {
    /// Serial device object.
    serial: Serial<EventFdTrigger, NoEvents, Stdout>,
    /// Input to the serial device (needs to be readable).
    input: Option<File>,
}

impl SerialDevice {
    /// Creates a serial device that raises `interrupt_evt` on guest-visible
    /// events, writes guest output to stdout, and optionally reads host input
    /// from `input`.
    pub fn new(interrupt_evt: EventFd, input: Option<File>) -> Self {
        let serial = Serial::new(EventFdTrigger::new(interrupt_evt), io::stdout());
        SerialDevice { serial, input }
    }

    /// The eventfd the device triggers to raise its interrupt (IRQ 4).
    pub fn interrupt_evt(&self) -> &EventFd {
        self.serial.interrupt_evt()
    }

    /// The optional host-side input source of the device.
    pub fn input(&self) -> Option<&File> {
        self.input.as_ref()
    }

    /// Send raw input bytes to the emulated device's RX FIFO.
    pub fn enqueue_input_bytes(&mut self, bytes: &[u8]) -> Result<(), SerialDeviceError> {
        // Fail fast if the serial is serviced with more data than it can buffer.
        if bytes.len() > self.serial.fifo_capacity() {
            return Err(SerialDeviceError::FullFifo);
        }
        self.serial
            .enqueue_raw_bytes(bytes)
            .map(|_bytes_enqueued| ())
            .map_err(SerialDeviceError::Serial)
    }

    /// Reads whatever fits in the RX FIFO from the input source and enqueues
    /// it. Returns the number of bytes consumed (0 signals EOF).
    pub fn read_input(&mut self) -> io::Result<usize> {
        let avail_cap = self.serial.fifo_capacity();
        if avail_cap == 0 {
            return Err(io::Error::from_raw_os_error(libc::ENOBUFS));
        }

        if let Some(input) = self.input.as_mut() {
            let mut out = vec![0u8; avail_cap];
            let count = input.read(&mut out)?;
            if count > 0 {
                self.serial
                    .enqueue_raw_bytes(&out[..count])
                    .map_err(|_| io::Error::from_raw_os_error(libc::ENOBUFS))?;
            }

            return Ok(count);
        }

        Err(io::Error::from_raw_os_error(libc::ENOTTY))
    }

    /// Handles a guest read from the UART register at `offset`.
    pub fn bus_read(&mut self, offset: u64, data: &mut [u8]) {
        if let (Ok(offset), 1) = (u8::try_from(offset), data.len()) {
            data[0] = self.serial.read(offset);
        } else {
            debug!(
                "serial: invalid read of {} bytes at offset {offset}",
                data.len()
            );
        }
    }

    /// Handles a guest write to the UART register at `offset`.
    pub fn bus_write(&mut self, offset: u64, data: &[u8]) {
        if let (Ok(offset), 1) = (u8::try_from(offset), data.len()) {
            if let Err(err) = self.serial.write(offset, data[0]) {
                error!("Failed the write to serial: {err:?}");
            }
        } else {
            debug!(
                "serial: invalid write of {} bytes at offset {offset}",
                data.len()
            );
        }
    }
}
