// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Portions Copyright 2017 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the THIRD-PARTY file.
// Ported into oci-vmm from Firecracker (src/vmm/src/devices/legacy/i8042.rs), trimmed of metrics/snapshot/ACPI.

//! A minimal i8042 PS/2 controller that emulates just enough to shut down
//! the machine: on the CPU-reset command (0xFE written to port 0x64) it
//! signals a reset [`EventFd`]. There is no keyboard behind it, so keyboard
//! interrupts were trimmed from the port; commands are still acked so guest
//! probing stays happy.

use std::num::Wrapping;

use tracing::{debug, error};
use vmm_sys_util::eventfd::EventFd;

/// Offset of the status port (port 0x64), relative to the 0x60 base.
const OFS_STATUS: u64 = 4;

/// Offset of the data port (port 0x60), relative to the 0x60 base.
const OFS_DATA: u64 = 0;

// i8042 commands.
// These values are written by the guest driver to port 0x64.
const CMD_READ_CTR: u8 = 0x20; // Read control register
const CMD_WRITE_CTR: u8 = 0x60; // Write control register
const CMD_READ_OUTP: u8 = 0xD0; // Read output port
const CMD_WRITE_OUTP: u8 = 0xD1; // Write output port
const CMD_RESET_CPU: u8 = 0xFE; // Reset CPU

// i8042 status register bits.
const SB_OUT_DATA_AVAIL: u8 = 0x0001; // Data available at port 0x60
const SB_I8042_CMD_DATA: u8 = 0x0008; // i8042 expecting command parameter at port 0x60
const SB_KBD_ENABLED: u8 = 0x0010; // 1 = kbd enabled, 0 = kbd locked

// i8042 control register bits.
const CB_KBD_INT: u8 = 0x0001; // kbd interrupt enabled
const CB_POST_OK: u8 = 0x0004; // POST ok (should always be 1)

/// Internal i8042 buffer size, in bytes.
const BUF_SIZE: usize = 16;

/// A i8042 PS/2 controller that emulates just enough to shutdown the machine.
#[derive(Debug)]
pub struct I8042Device {
    /// CPU reset eventfd. We will set this event when the guest issues CMD_RESET_CPU.
    reset_evt: EventFd,

    /// The i8042 status register.
    status: u8,

    /// The i8042 control register.
    control: u8,

    /// The i8042 output port.
    outp: u8,

    /// The last command sent to port 0x64.
    cmd: u8,

    /// The internal i8042 data buffer.
    buf: [u8; BUF_SIZE],
    bhead: Wrapping<usize>,
    btail: Wrapping<usize>,
}

impl I8042Device {
    /// Constructs an i8042 device that will signal the given event when the guest requests it.
    pub fn new(reset_evt: EventFd) -> Self {
        I8042Device {
            reset_evt,
            control: CB_POST_OK | CB_KBD_INT,
            cmd: 0,
            outp: 0,
            status: SB_KBD_ENABLED,
            buf: [0; BUF_SIZE],
            bhead: Wrapping(0),
            btail: Wrapping(0),
        }
    }

    #[inline]
    fn push_byte(&mut self, byte: u8) {
        self.status |= SB_OUT_DATA_AVAIL;
        if self.buf_len() == BUF_SIZE {
            debug!("i8042: internal buffer full, dropping byte {byte:#04x}");
            return;
        }
        self.buf[self.btail.0 % BUF_SIZE] = byte;
        self.btail += Wrapping(1usize);
    }

    #[inline]
    fn pop_byte(&mut self) -> Option<u8> {
        if self.buf_len() == 0 {
            return None;
        }
        let res = self.buf[self.bhead.0 % BUF_SIZE];
        self.bhead += Wrapping(1usize);
        if self.buf_len() == 0 {
            self.status &= !SB_OUT_DATA_AVAIL;
        }
        Some(res)
    }

    #[inline]
    fn flush_buf(&mut self) {
        self.bhead = Wrapping(0usize);
        self.btail = Wrapping(0usize);
        self.status &= !SB_OUT_DATA_AVAIL;
    }

    #[inline]
    fn buf_len(&self) -> usize {
        (self.btail - self.bhead).0
    }

    /// Handles a guest read from port `0x60 + offset`.
    pub fn bus_read(&mut self, offset: u64, data: &mut [u8]) {
        // All our ports are byte-wide. We don't know how to handle any wider data.
        if data.len() != 1 {
            debug!(
                "i8042: invalid read of {} bytes at offset {offset}",
                data.len()
            );
            return;
        }

        match offset {
            OFS_STATUS => data[0] = self.status,
            OFS_DATA => {
                // The guest wants to read a byte from port 0x60. For the 8042, that means the top
                // byte in the internal buffer. If the buffer is empty, the guest will get a 0.
                data[0] = self.pop_byte().unwrap_or(0);
            }
            _ => debug!("i8042: read from unhandled offset {offset}"),
        }
    }

    /// Handles a guest write to port `0x60 + offset`.
    pub fn bus_write(&mut self, offset: u64, data: &[u8]) {
        // All our ports are byte-wide. We don't know how to handle any wider data.
        if data.len() != 1 {
            debug!(
                "i8042: invalid write of {} bytes at offset {offset}",
                data.len()
            );
            return;
        }

        match offset {
            OFS_STATUS if data[0] == CMD_RESET_CPU => {
                // The guest wants to assert the CPU reset line. We handle that by triggering
                // our exit event fd. Meaning the VMM will be exiting as soon as the VMM
                // thread wakes up to handle this event.
                if let Err(err) = self.reset_evt.write(1) {
                    error!("Failed to trigger i8042 reset event: {err:?}");
                }
            }
            OFS_STATUS if data[0] == CMD_READ_CTR => {
                // The guest wants to read the control register.
                // Let's make sure only the control register will be available for reading from
                // the data port, for the next inb(0x60).
                self.flush_buf();
                let control = self.control;
                // Buffer is empty, push() will always succeed.
                self.push_byte(control);
            }
            OFS_STATUS if data[0] == CMD_WRITE_CTR => {
                // The guest wants to write the control register. This is a two-step command:
                // 1. port 0x64 < CMD_WRITE_CTR
                // 2. port 0x60 < <control reg value>
                // Make sure we'll be expecting the control reg value on port 0x60 for the next
                // write.
                self.flush_buf();
                self.status |= SB_I8042_CMD_DATA;
                self.cmd = data[0];
            }
            OFS_STATUS if data[0] == CMD_READ_OUTP => {
                // The guest wants to read the output port (for lack of a better name - this is
                // just another register on the 8042, that happens to also have its bits connected
                // to some output pins of the 8042).
                self.flush_buf();
                let outp = self.outp;
                // Buffer is empty, push() will always succeed.
                self.push_byte(outp);
            }
            OFS_STATUS if data[0] == CMD_WRITE_OUTP => {
                // Similar to writing the control register, this is a two-step command.
                // I.e. write CMD_WRITE_OUTP at port 0x64, then write the actual out port value
                // to port 0x60.
                self.status |= SB_I8042_CMD_DATA;
                self.cmd = data[0];
            }
            OFS_DATA if (self.status & SB_I8042_CMD_DATA) != 0 => {
                // The guest is writing to port 0x60. This byte can either be:
                // 1. the payload byte of a CMD_WRITE_CTR or CMD_WRITE_OUTP command, in which case
                //    the status reg bit SB_I8042_CMD_DATA will be set, or
                // 2. a direct command sent to the keyboard
                // This match arm handles the first option (when the SB_I8042_CMD_DATA bit is set).
                match self.cmd {
                    CMD_WRITE_CTR => self.control = data[0],
                    CMD_WRITE_OUTP => self.outp = data[0],
                    _ => (),
                }
                self.status &= !SB_I8042_CMD_DATA;
            }
            OFS_DATA => {
                // The guest is sending a command straight to the keyboard (so this byte is not
                // addressed to the 8042, but to the keyboard). Since we're emulating a pretty
                // dumb keyboard, we can get away with blindly ack-in anything (byte 0xFA).
                self.flush_buf();
                // Buffer is empty, push() will always succeed.
                self.push_byte(0xFA);
            }
            _ => debug!(
                "i8042: write of {:#04x} to unhandled offset {offset}",
                data[0]
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_i8042_reset() {
        let mut i8042 = I8042Device::new(EventFd::new(libc::EFD_NONBLOCK).unwrap());
        let reset_evt = i8042.reset_evt.try_clone().unwrap();

        // Write 1 to the reset event fd, so that read doesn't block in case the event fd
        // counter doesn't change (for 0 it blocks).
        reset_evt.write(1).unwrap();
        let data = [CMD_RESET_CPU];
        i8042.bus_write(OFS_STATUS, &data);
        assert_eq!(reset_evt.read().unwrap(), 2);
    }

    #[test]
    fn test_i8042_commands() {
        let mut i8042 = I8042Device::new(EventFd::new(libc::EFD_NONBLOCK).unwrap());
        let mut data = [1];

        // Test reading/writing the control register.
        data[0] = CMD_WRITE_CTR;
        i8042.bus_write(OFS_STATUS, &data);
        assert_ne!(i8042.status & SB_I8042_CMD_DATA, 0);
        data[0] = 0x52;
        i8042.bus_write(OFS_DATA, &data);
        data[0] = CMD_READ_CTR;
        i8042.bus_write(OFS_STATUS, &data);
        assert_ne!(i8042.status & SB_OUT_DATA_AVAIL, 0);
        i8042.bus_read(OFS_DATA, &mut data);
        assert_eq!(data[0], 0x52);

        // Test kbd commands get blindly acked.
        data[0] = 0x52;
        i8042.bus_write(OFS_DATA, &data);
        assert_ne!(i8042.status & SB_OUT_DATA_AVAIL, 0);
        i8042.bus_read(OFS_DATA, &mut data);
        assert_eq!(data[0], 0xFA);
    }
}
