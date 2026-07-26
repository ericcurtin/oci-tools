// SPDX-License-Identifier: Apache-2.0

//! Trap-and-emulate MMIO for devices with no stage-2 mapping at all
//! (this backend's PL011 console, and -- phase 4 -- virtio-mmio):
//! unlike x86_64/KVM's port-I/O-based devices (`crate::pci`, trapped
//! by the architecture directly), AArch64 has no port I/O, so an
//! access to an address `hv_vm_map` never mapped instead surfaces as
//! a stage-2 Data Abort, exiting with `ExitReason::Exception` and an
//! `ESR_ELx` this module decodes.
//!
//! Only `ISV == 1` syndromes (a single, simple integer register
//! load/store -- every access a real PL011/virtio-mmio driver
//! actually performs) are supported; anything else (an exclusive/
//! atomic/SIMD access, or a load/store pair) can't be emulated from
//! the syndrome alone and is rejected rather than silently
//! misinterpreted. No real device driver this project targets emits
//! those forms against a plain MMIO peripheral.

use crate::hvf::sys::{self, hv_reg_t};
use crate::hvf::vcpu::{Exception, Vcpu};

/// A decoded AArch64 Data Abort syndrome (`ESR_ELx.ISS`, Data Abort
/// encoding, `ISV == 1`) -- enough to emulate the single integer
/// load/store instruction that caused it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataAbort {
    /// `true` for a store (guest write), `false` for a load (guest
    /// read).
    pub write: bool,
    /// Access size in bytes: 1, 2, 4, or 8 (`SAS`).
    pub size: u8,
    /// `true` if a load result should be sign-extended (`SSE`);
    /// meaningless for a store.
    pub sign_extend: bool,
    /// The transfer register index, `0..=31` (`SRT`); `31` is the
    /// zero register (`XZR`/`WZR`), not a real `hv_reg_t` -- see
    /// [`sys::hv_reg_x`].
    pub reg: u32,
    /// `true` if the transfer register is accessed as 64-bit `Xt`
    /// (`SF`); `false` for 32-bit `Wt` (in which case a load zeroes,
    /// or a store ignores, the upper 32 bits).
    pub reg_is_64bit: bool,
}

/// Every EL1 (AArch64) synchronous exception exit this backend
/// expects has this EC (bits `[31:26]` of `ESR_ELx`): a Data Abort
/// taken from a lower Exception level to EL2, exactly what a guest
/// MMIO access to an unmapped stage-2 address produces. (`0x25`,
/// "Data Abort taken without a change in Exception level", would mean
/// EL2 itself faulted -- a host bug, not a guest MMIO access, and
/// isn't handled here.)
const ESR_EC_DATA_ABORT_LOWER_EL: u64 = 0x24;

impl DataAbort {
    /// Decodes `syndrome` (`ESR_ELx`) as a Data Abort. Returns `None`
    /// if this isn't a Data-Abort-from-a-lower-EL exception at all,
    /// or if `ISV == 0` (not a single, simple register transfer --
    /// see the module docs).
    pub fn decode(syndrome: u64) -> Option<Self> {
        let ec = (syndrome >> 26) & 0x3f;
        if ec != ESR_EC_DATA_ABORT_LOWER_EL {
            return None;
        }

        let iss = syndrome & 0x01ff_ffff;
        let isv = (iss >> 24) & 0x1;
        if isv == 0 {
            return None;
        }

        let sas = (iss >> 22) & 0x3;
        let sse = (iss >> 21) & 0x1;
        let srt = (iss >> 16) & 0x1f;
        let sf = (iss >> 15) & 0x1;
        let wnr = (iss >> 6) & 0x1;

        Some(DataAbort {
            write: wnr == 1,
            size: 1u8 << sas,
            sign_extend: sse == 1,
            reg: u32::try_from(srt).expect("5-bit field fits in u32"),
            reg_is_64bit: sf == 1,
        })
    }
}

/// A memory-mapped device: a flat register file addressed by a byte
/// offset from wherever it's based in guest physical memory. Mirrors
/// `legacy::serial::SerialDevice`'s own `bus_read`/`bus_write` shape
/// (offset, byte slice), just as a trait so [`emulate`] can be
/// generic over which device actually handled a given access.
pub trait MmioDevice {
    /// Handles a guest read of `data.len()` bytes at `offset`.
    fn read(&mut self, offset: u64, data: &mut [u8]);
    /// Handles a guest write of `data` at `offset`.
    fn write(&mut self, offset: u64, data: &[u8]);
}

/// Errors from [`emulate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MmioError {
    /// The exception wasn't a Data Abort this module can decode --
    /// see [`DataAbort::decode`].
    #[error(
        "not a decodable data-abort-from-lower-EL exception (ESR_ELx={0:#x}): unsupported \
         access form (load/store pair, exclusive, or SIMD/FP), or not a data abort at all"
    )]
    Undecodable(u64),
    /// [`Vcpu::get_reg`]/[`Vcpu::set_reg`] itself failed.
    #[error(transparent)]
    Hv(#[from] crate::hvf::HvError),
}

/// Emulates the single load/store instruction that produced
/// `exception` against `device`, then advances the vCPU's `PC` past
/// it (unlike `hvc`, a Data Abort's reported `PC` is the faulting
/// instruction's own address, not a return address -- confirmed
/// directly on real hardware, see `docs/design/0249`).
///
/// `device_base` is the guest physical address `device`'s offset 0
/// corresponds to; the caller is responsible for having already
/// matched `exception.physical_address` against whichever device's
/// address range it falls within (there is no bus/multi-device
/// dispatch here yet -- see `docs/design/0249` phase 4, once there's
/// a second device model to dispatch across).
pub fn emulate(
    vcpu: &Vcpu,
    exception: &Exception,
    device_base: u64,
    device: &mut dyn MmioDevice,
) -> Result<(), MmioError> {
    let abort =
        DataAbort::decode(exception.syndrome).ok_or(MmioError::Undecodable(exception.syndrome))?;
    let offset = exception.physical_address - device_base;
    let size = usize::from(abort.size);

    if abort.write {
        let value = if abort.reg == 31 {
            0 // XZR: a store of the zero register.
        } else {
            vcpu.get_reg(reg_id(abort.reg))?
        };
        let bytes = value.to_le_bytes();
        device.write(offset, &bytes[..size]);
    } else {
        let mut bytes = [0u8; 8];
        device.read(offset, &mut bytes[..size]);
        let raw = u64::from_le_bytes(bytes);
        let value = sign_or_zero_extend(raw, abort.size, abort.sign_extend, abort.reg_is_64bit);
        if abort.reg != 31 {
            // Writes to XZR are simply discarded, per the architecture.
            vcpu.set_reg(reg_id(abort.reg), value)?;
        }
    }

    let pc = vcpu.get_reg(sys::HV_REG_PC)?;
    vcpu.set_reg(sys::HV_REG_PC, pc + 4)?;
    Ok(())
}

/// Maps a decoded `SRT` field (`0..=31`) to the `hv_reg_t` PL011/
/// virtio-mmio callers actually read/write -- `31` (`XZR`) is handled
/// by [`emulate`] itself and never reaches here.
fn reg_id(srt: u32) -> hv_reg_t {
    sys::hv_reg_x(srt)
}

/// Zero- or sign-extends a `size`-byte little-endian value (already
/// widened into the low bytes of `raw`) to the full 64 bits, per
/// AArch64 load semantics; truncated back to the low 32 bits by the
/// caller's own `set_reg` only matters for a 32-bit (`Wt`) transfer,
/// which zero-extends the upper 32 bits regardless of `sign_extend`
/// (`LDRSW` -- 32-bit destination, sign-extending load -- is the one
/// exception, but always targets an `Xt`, i.e. `reg_is_64bit`, so this
/// still holds).
fn sign_or_zero_extend(raw: u64, size: u8, sign_extend: bool, reg_is_64bit: bool) -> u64 {
    if !sign_extend {
        return raw;
    }
    let bits = size * 8;
    let shift = 64 - u32::from(bits);
    let sign_extended = ((raw << shift) as i64 >> shift) as u64;
    if reg_is_64bit {
        sign_extended
    } else {
        sign_extended & 0xffff_ffff
    }
}

#[cfg(test)]
#[allow(unsafe_code)] // mmap + Vm::map: safety documented at each call site below.
mod tests {
    //! Exercises the whole decode-and-emulate pipeline for real: a
    //! guest writes a word to an intentionally *unmapped* IPA (no
    //! `Vm::map` call for it at all), reads it back, then signals
    //! completion via `hvc` -- proving both the write and read
    //! Data Abort paths against a trivial in-memory "device", and
    //! confirming (unlike `hvc`) that a Data Abort's own reported PC
    //! is the faulting instruction's address, requiring this
    //! module's own `+4` advance.

    use super::{MmioDevice, emulate};
    use crate::hvf::sys::{HV_REG_CPSR, HV_REG_PC, hv_reg_x};
    use crate::hvf::vcpu::{ExitReason, Vcpu};
    use crate::hvf::vm::Vm;

    /// `str w2, [x1]` ; `ldr w3, [x1]` ; `hvc #0` -- verified against
    /// a real assembler/disassembler (`as -arch arm64` /
    /// `objdump -d`), not hand-encoded blind.
    const CODE: [u32; 3] = [0xb900_0022, 0xb940_0023, 0xd400_0002];

    const CPSR_EL1H_MASKED: u64 = 0x3c5;
    const ESR_EC_HVC64: u64 = 0x16;

    /// A single 4-byte register, standing in for a real device model
    /// (PL011, later virtio-mmio) -- just enough to prove `emulate`
    /// round-trips a write followed by a read correctly.
    #[derive(Default)]
    struct FakeReg(u32);

    impl MmioDevice for FakeReg {
        fn read(&mut self, offset: u64, data: &mut [u8]) {
            assert_eq!(offset, 0, "single-register fake device");
            data.copy_from_slice(&self.0.to_le_bytes()[..data.len()]);
        }

        fn write(&mut self, offset: u64, data: &[u8]) {
            assert_eq!(offset, 0, "single-register fake device");
            let mut bytes = [0u8; 4];
            bytes[..data.len()].copy_from_slice(data);
            self.0 = u32::from_le_bytes(bytes);
        }
    }

    #[test]
    #[ignore = "needs real Hypervisor.framework hardware support (hv_vm_create) plus this test \
                binary codesigned with the com.apple.security.hypervisor entitlement -- run \
                locally on real Apple Silicon (ci/codesign-ocivmm.sh, then `cargo test ... -- \
                --ignored --test-threads=1`); GitHub-hosted macOS runners have no hv_support at \
                all on any macOS version, so this can never pass there regardless of signing -- \
                see docs/design/0249 phase 7"]
    fn write_then_read_round_trips_through_a_data_abort() {
        const PAGE_SIZE: usize = 16384; // see hvf::tests's own note on Apple Silicon's page size.
        const CODE_ADDR: u64 = PAGE_SIZE as u64;
        const MMIO_ADDR: u64 = 2 * PAGE_SIZE as u64; // deliberately never mapped.

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
        vcpu.set_reg(hv_reg_x(1), MMIO_ADDR).unwrap(); // x1: MMIO address.
        vcpu.set_reg(hv_reg_x(2), 0xdead_beef).unwrap(); // x2: value to store.

        let mut device = FakeReg::default();

        // `str w2, [x1]`: expect a write Data Abort.
        match vcpu.run().unwrap() {
            ExitReason::Exception(exception) => {
                assert_eq!(exception.physical_address, MMIO_ADDR);
                emulate(&vcpu, &exception, MMIO_ADDR, &mut device).expect("emulate write");
            }
            other => panic!("expected a data abort, got {other:?}"),
        }
        assert_eq!(
            device.0, 0xdead_beef,
            "the device should have observed the store"
        );
        assert_eq!(
            vcpu.get_reg(HV_REG_PC).unwrap(),
            CODE_ADDR + 4,
            "emulate() must itself advance PC past the data-aborting instruction"
        );

        // `ldr w3, [x1]`: expect a read Data Abort, and x3 populated
        // from the device afterwards.
        match vcpu.run().unwrap() {
            ExitReason::Exception(exception) => {
                emulate(&vcpu, &exception, MMIO_ADDR, &mut device).expect("emulate read");
            }
            other => panic!("expected a data abort, got {other:?}"),
        }
        assert_eq!(vcpu.get_reg(hv_reg_x(3)).unwrap(), 0xdead_beef);

        // `hvc #0`: confirms execution actually reached the third
        // instruction (not stuck re-executing the second forever).
        match vcpu.run().unwrap() {
            ExitReason::Exception(exception) => {
                let ec = (exception.syndrome >> 26) & 0x3f;
                assert_eq!(ec, ESR_EC_HVC64);
            }
            other => panic!("expected the closing hvc, got {other:?}"),
        }
    }
}
