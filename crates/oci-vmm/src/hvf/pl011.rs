// SPDX-License-Identifier: Apache-2.0

//! A minimal ARM PL011 UART, this backend's serial console -- the
//! aarch64 analogue of `legacy::serial::SerialDevice`'s 16550 on the
//! x86_64/KVM side, wired through [`crate::hvf::mmio`]'s Data-Abort
//! trap-and-emulate instead of KVM's in-kernel port I/O.
//!
//! `vm-superio` (already a dependency, on the KVM/x86_64 side) has no
//! PL011 model at all -- only a 16550 (`Serial`), an i8042, and a
//! PL031 RTC -- so this one is hand-written directly, against the
//! real PL011 Technical Reference Manual register layout, cross-
//! checked against QEMU's own `hw/char/pl011.c` (in particular the
//! `pl011_id_arm` AMBA PrimeCell ID bytes at `0xfe0..0xfff`, which
//! Linux's `amba` bus driver reads back and validates during probe --
//! get those wrong and the device tree's `arm,pl011` node silently
//! fails to bind to a driver at all).
//!
//! Deliberately minimal, matching this project's pet-VM scope: no
//! interrupts are ever raised (`UARTRIS`/`UARTMIS` always read as
//! `0`), because the kernel's own PL011 console write path
//! (`pl011_console_write`) polls `UARTFR.TXFF` rather than waiting for
//! an interrupt -- true of every serial console driver Linux ships,
//! since `printk` must also work with interrupts disabled (panics,
//! early boot). A future phase that needs guest input beyond `ocivmm
//! cp`'s existing disk-image-based channel (interactive shells, ...)
//! would need real RX interrupt delivery via `hv_gic_set_spi`; not
//! needed for this project's own pet-VM model so far.

use std::collections::VecDeque;
use std::io::{self, Write};

use crate::hvf::mmio::MmioDevice;

/// `UARTDR`: data register.
const REG_DR: u64 = 0x000;
/// `UARTFR`: flag register.
const REG_FR: u64 = 0x018;
/// `UARTILPR`: IrDA low-power counter (stored, otherwise unused).
const REG_ILPR: u64 = 0x020;
/// `UARTIBRD`: integer baud rate divisor.
const REG_IBRD: u64 = 0x024;
/// `UARTFBRD`: fractional baud rate divisor.
const REG_FBRD: u64 = 0x028;
/// `UARTLCR_H`: line control register.
const REG_LCR_H: u64 = 0x02c;
/// `UARTCR`: control register.
const REG_CR: u64 = 0x030;
/// `UARTIFLS`: interrupt FIFO level select.
const REG_IFLS: u64 = 0x034;
/// `UARTIMSC`: interrupt mask set/clear.
const REG_IMSC: u64 = 0x038;
/// `UARTRIS`: raw interrupt status (read-only).
const REG_RIS: u64 = 0x03c;
/// `UARTMIS`: masked interrupt status (read-only).
const REG_MIS: u64 = 0x040;
/// `UARTICR`: interrupt clear register (write-only).
const REG_ICR: u64 = 0x044;
/// `UARTDMACR`: DMA control register (stored, no DMA device modeled).
const REG_DMACR: u64 = 0x048;
/// First AMBA PrimeCell ID register (`UARTPeriphID0`); seven more
/// follow at 4-byte strides up to and including `UARTPCellID3` at
/// `0xffc`.
const REG_ID_START: u64 = 0xfe0;

/// `UARTFR.TXFE` (transmit FIFO empty) -- always set: writes are
/// "transmitted" (handed to `W`) synchronously, so the FIFO is never
/// non-empty from the guest's point of view.
const FR_TXFE: u32 = 1 << 7;
/// `UARTFR.RXFE` (receive FIFO empty).
const FR_RXFE: u32 = 1 << 4;

/// QEMU's own `pl011_id_arm` -- the eight bytes Linux's `amba` bus
/// driver reads back from `UARTPeriphID0..3`/`UARTPCellID0..3` (one
/// significant byte per 4-byte register) to confirm a device tree's
/// `arm,pl011` node really is a PL011 before binding a driver to it.
const AMBA_ID: [u8; 8] = [0x11, 0x10, 0x14, 0x00, 0x0d, 0xf0, 0x05, 0xb1];

/// A minimal PL011, writing transmitted bytes to `W` (production:
/// `io::Stdout`, matching `SerialDevice`; tests: any other `Write`).
#[derive(Debug)]
pub struct Pl011<W: Write> {
    writer: W,
    /// `UARTCR`, `UARTLCR_H`, `UARTIBRD`, `UARTFBRD`, `UARTIFLS`,
    /// `UARTILPR`, `UARTDMACR`: stored verbatim, no behavior depends
    /// on their value (this model has no baud-rate/framing/DMA
    /// concept at all -- see the module docs on why interrupts are
    /// similarly unimplemented).
    cr: u32,
    lcr_h: u32,
    ibrd: u32,
    fbrd: u32,
    ifls: u32,
    ilpr: u32,
    dmacr: u32,
    /// `UARTIMSC`: stored so a guest read of it (or of the always-
    /// zero `UARTRIS`/`UARTMIS`) sees back what it last wrote, even
    /// though no interrupt this model could mask is ever raised.
    imsc: u32,
    /// Bytes available for the guest to read from `UARTDR`. Never
    /// populated by anything in this module yet (no host-side input
    /// path wired up); `enqueue_input` exists for a future phase to
    /// call, and is exercised directly by this module's own tests in
    /// the meantime.
    rx: VecDeque<u8>,
}

impl<W: Write> Pl011<W> {
    /// Creates a PL011 in its architectural reset state (`UARTCR =
    /// 0x300`: `TXE`/`RXE` enabled, `UARTEN` clear -- matching real
    /// hardware and QEMU's own `pl011_reset`), writing transmitted
    /// bytes to `writer`.
    pub fn new(writer: W) -> Self {
        Pl011 {
            writer,
            cr: 0x300,
            lcr_h: 0,
            ibrd: 0,
            fbrd: 0,
            ifls: 0x12,
            ilpr: 0,
            dmacr: 0,
            imsc: 0,
            rx: VecDeque::new(),
        }
    }

    /// Queues bytes for the guest to read back via `UARTDR`. Not
    /// called by any current boot path (see the module docs); kept
    /// for a future phase and exercised directly by this module's own
    /// tests.
    pub fn enqueue_input(&mut self, bytes: &[u8]) {
        self.rx.extend(bytes);
    }

    fn flags(&self) -> u32 {
        let mut flags = FR_TXFE;
        if self.rx.is_empty() {
            flags |= FR_RXFE;
        }
        flags
    }

    fn read_reg(&mut self, offset: u64) -> u32 {
        match offset {
            REG_DR => u32::from(self.rx.pop_front().unwrap_or(0)),
            REG_FR => self.flags(),
            REG_ILPR => self.ilpr,
            REG_IBRD => self.ibrd,
            REG_FBRD => self.fbrd,
            REG_LCR_H => self.lcr_h,
            REG_CR => self.cr,
            REG_IFLS => self.ifls,
            REG_IMSC => self.imsc,
            // UARTRIS/UARTMIS: always 0 -- see the module docs on why
            // this model never raises an interrupt.
            REG_RIS | REG_MIS => 0,
            REG_DMACR => self.dmacr,
            id if (REG_ID_START..REG_ID_START + 32).contains(&id) => {
                let index = usize::try_from((id - REG_ID_START) / 4).unwrap();
                u32::from(AMBA_ID[index])
            }
            _ => 0,
        }
    }

    fn write_reg(&mut self, offset: u64, value: u32) {
        match offset {
            REG_DR => {
                // Real hardware truncates UARTDR writes to the low 8
                // (data) bits; the upper bits configure parity/break
                // injection this model doesn't emulate.
                let byte = value as u8;
                if self
                    .writer
                    .write_all(&[byte])
                    .and_then(|()| self.writer.flush())
                    .is_err()
                {
                    tracing::error!("pl011: failed writing a transmitted byte to the console");
                }
            }
            REG_ILPR => self.ilpr = value,
            REG_IBRD => self.ibrd = value,
            REG_FBRD => self.fbrd = value,
            REG_LCR_H => self.lcr_h = value,
            REG_CR => self.cr = value,
            REG_IFLS => self.ifls = value,
            REG_IMSC => self.imsc = value,
            REG_ICR => {} // Clears raw interrupt status bits; always 0 already.
            REG_DMACR => self.dmacr = value,
            REG_FR | REG_RIS | REG_MIS => {} // Read-only; writes ignored, matching real hardware.
            _ => {}
        }
    }
}

impl<W: Write> MmioDevice for Pl011<W> {
    fn read(&mut self, offset: u64, data: &mut [u8]) {
        let value = self.read_reg(offset & !0x3);
        let bytes = value.to_le_bytes();
        let len = data.len().min(4);
        data[..len].copy_from_slice(&bytes[..len]);
    }

    fn write(&mut self, offset: u64, data: &[u8]) {
        let mut bytes = [0u8; 4];
        let len = data.len().min(4);
        bytes[..len].copy_from_slice(&data[..len]);
        self.write_reg(offset & !0x3, u32::from_le_bytes(bytes));
    }
}

/// Convenience alias for the production console: a PL011 writing to
/// stdout, exactly like `legacy::serial::SerialDevice`'s own default.
pub type StdoutPl011 = Pl011<io::Stdout>;

#[cfg(test)]
mod tests {
    //! Plain register-level tests of the `MmioDevice` impl itself --
    //! no vCPU/entitlement involved (that round trip, real Data
    //! Abort decode into a register read/write, is already proven
    //! against a trivial fake device by `hvf::mmio`'s own hardware
    //! test; this just needs to prove *this* device's register
    //! semantics are correct).

    use super::*;

    fn read_u32(dev: &mut Pl011<Vec<u8>>, offset: u64) -> u32 {
        let mut buf = [0u8; 4];
        dev.read(offset, &mut buf);
        u32::from_le_bytes(buf)
    }

    #[test]
    fn amba_primecell_id_matches_qemu_pl011_id_arm() {
        let mut dev = Pl011::new(Vec::new());
        for (i, expected) in AMBA_ID.iter().enumerate() {
            let offset = REG_ID_START + 4 * i as u64;
            assert_eq!(
                read_u32(&mut dev, offset) as u8,
                *expected,
                "ID register {i}"
            );
        }
    }

    #[test]
    fn writing_dr_forwards_bytes_to_the_writer() {
        let mut dev = Pl011::new(Vec::new());
        for byte in b"hi\n" {
            dev.write(REG_DR, &[*byte]);
        }
        assert_eq!(dev.writer, b"hi\n");
    }

    #[test]
    fn flag_register_reflects_rx_queue_state() {
        let mut dev = Pl011::new(Vec::new());
        assert_eq!(read_u32(&mut dev, REG_FR), FR_TXFE | FR_RXFE);

        dev.enqueue_input(b"x");
        assert_eq!(
            read_u32(&mut dev, REG_FR) & FR_RXFE,
            0,
            "RXFE should clear once a byte is queued"
        );

        let mut byte = [0u8; 1];
        dev.read(REG_DR, &mut byte);
        assert_eq!(byte[0], b'x');
        assert_eq!(
            read_u32(&mut dev, REG_FR) & FR_RXFE,
            FR_RXFE,
            "RXFE should be set again once the queue is drained"
        );
    }

    #[test]
    fn control_registers_round_trip() {
        let mut dev = Pl011::new(Vec::new());
        assert_eq!(read_u32(&mut dev, REG_CR), 0x300, "reset UARTCR");

        dev.write(REG_CR, &0x301u32.to_le_bytes());
        assert_eq!(read_u32(&mut dev, REG_CR), 0x301);

        dev.write(REG_IBRD, &26u32.to_le_bytes());
        dev.write(REG_FBRD, &3u32.to_le_bytes());
        assert_eq!(read_u32(&mut dev, REG_IBRD), 26);
        assert_eq!(read_u32(&mut dev, REG_FBRD), 3);

        // UARTRIS/UARTMIS are always 0 -- this model never raises an
        // interrupt (see the module docs).
        assert_eq!(read_u32(&mut dev, REG_RIS), 0);
        assert_eq!(read_u32(&mut dev, REG_MIS), 0);
    }

    /// The full, real pipeline: a guest program actually running
    /// under `Hypervisor.framework` writes "Hi\n" a byte at a time to
    /// a PL011's `UARTDR`, at an IPA never `Vm::map`-ped at all (a
    /// real Data Abort exit each time), and this test confirms the
    /// bytes arrive at the device's own writer in order -- run for
    /// real on Apple Silicon hardware, requires the hypervisor
    /// entitlement (see `hvf`'s own module docs and
    /// `ci/codesign-ocivmm.sh`).
    #[test]
    #[allow(unsafe_code)] // mmap + Vm::map: safety documented at each call site below.
    fn a_running_guest_prints_through_a_real_data_abort() {
        use crate::hvf::mmio::emulate;
        use crate::hvf::sys::{HV_REG_CPSR, HV_REG_PC, hv_reg_x};
        use crate::hvf::vcpu::{ExitReason, Vcpu};
        use crate::hvf::vm::Vm;

        const PAGE_SIZE: usize = 16384;
        const CODE_ADDR: u64 = PAGE_SIZE as u64;
        const UART_ADDR: u64 = 2 * PAGE_SIZE as u64; // deliberately never mapped.
        const CPSR_EL1H_MASKED: u64 = 0x3c5;

        // `mov w2, #0x48` ; `str w2, [x1]` ; `mov w2, #0x69` ;
        // `str w2, [x1]` ; `mov w2, #0xa` ; `str w2, [x1]` ; `hvc #0`
        // -- verified via `as -arch arm64`/`objdump -d`, not
        // hand-encoded blind (see docs/design/0249).
        const CODE: [u32; 7] = [
            0x5280_0902,
            0xb900_0022,
            0x5280_0d22,
            0xb900_0022,
            0x5280_0142,
            0xb900_0022,
            0xd400_0002,
        ];

        let host_ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                PAGE_SIZE,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };
        assert_ne!(host_ptr, libc::MAP_FAILED);
        let host_ptr = host_ptr.cast::<u8>();
        for (i, insn) in CODE.iter().enumerate() {
            unsafe {
                host_ptr
                    .add(i * 4)
                    .cast::<u32>()
                    .write_unaligned(insn.to_le());
            }
        }

        let vm = Vm::create().expect("hv_vm_create");
        unsafe {
            vm.map(host_ptr, CODE_ADDR, PAGE_SIZE, true, true, true)
                .expect("hv_vm_map");
        }

        let vcpu = Vcpu::create(&vm).expect("hv_vcpu_create");
        vcpu.set_reg(HV_REG_PC, CODE_ADDR).unwrap();
        vcpu.set_reg(HV_REG_CPSR, CPSR_EL1H_MASKED).unwrap();
        vcpu.set_reg(hv_reg_x(1), UART_ADDR).unwrap();

        let mut uart = Pl011::new(Vec::new());
        loop {
            match vcpu.run().unwrap() {
                ExitReason::Exception(exception) => {
                    let ec = (exception.syndrome >> 26) & 0x3f;
                    if ec == 0x16 {
                        break; // The closing hvc.
                    }
                    emulate(&vcpu, &exception, UART_ADDR, &mut uart).expect("emulate PL011 access");
                }
                other => panic!("unexpected exit: {other:?}"),
            }
        }

        assert_eq!(uart.writer, b"Hi\n");
    }
}
