// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
//
// oci-vmm original: Firecracker only ever boots an uncompressed ELF
// vmlinux; it has no bzImage support at all (checked directly — no
// `BzImage`/`bzimage` reference anywhere in `~/git/firecracker`). A
// pet VM's kernel is whatever its own `dnf`/`apt` installed (a
// compressed bzImage, like every real distro ships), but
// `linux-loader`'s own `bzimage` feature only loads the bzImage's
// still-*compressed* payload into guest memory at its preferred
// address — it does not decompress it (checked directly in its own
// `src/loader/bzimage/mod.rs`: "Seek the compressed vmlinux.bin and
// read it to memory"), matching what a real BIOS bootloader does
// (jump to the bzImage's own embedded decompression stub, which this
// VMM's direct-64-bit-entry boot never runs at all). So the caller
// (`ocivmm`'s own `disk.rs`) decompresses the bzImage into a plain ELF
// vmlinux itself before calling [`load_kernel`], which then uses
// `linux-loader`'s ELF loader and takes its `kernel_load` address
// directly as the 64-bit entry point — ELF loading always yields a
// direct, correct entry regardless of whether the kernel also carries
// a PVH note (checked directly in the ELF loader's own `mod.rs`:
// `kernel_load` is set from the plain ELF header unconditionally; PVH
// capability is tracked separately for callers that want it, and this
// one doesn't). The rest of this module — cmdline/initrd/boot_params
// assembly — reuses Firecracker's own `configure_64bit_boot` logic
// (ported and adjusted below).

//! Loading the guest's own kernel + initramfs and building the Linux
//! 64-bit boot protocol's `boot_params` ("zero page").

use std::fs::File;

use linux_loader::configurator::linux::LinuxBootConfigurator;
use linux_loader::configurator::{BootConfigurator, BootParams};
use linux_loader::loader::bootparam::boot_params;
use linux_loader::loader::elf::Elf;
use linux_loader::loader::{Cmdline, KernelLoader, KernelLoaderResult, load_cmdline};
use vm_memory::{Address, Bytes, GuestAddress, GuestMemory, GuestMemoryRegion};

use crate::arch::layout;
use crate::arch::{BootProtocol, EntryPoint};
use crate::mem::GuestMemoryMmap;

/// e820 memory map entry types (from the kernel's own `<asm/e820/types.h>`,
/// not re-exported by `linux-loader`'s raw `boot_params` bindings).
const E820_RAM: u32 = 1;
const E820_RESERVED: u32 = 2;

/// Where the guest's initrd landed, for [`configure_boot_params`].
pub struct InitrdConfig {
    /// Guest physical address.
    pub address: GuestAddress,
    /// Size in bytes.
    pub size: usize,
}

/// Errors loading the kernel/initrd or building `boot_params`.
#[derive(Debug, thiserror::Error)]
pub enum BootError {
    /// Failed to load the kernel bzImage.
    #[error("loading the kernel image: {0}")]
    KernelLoad(#[source] linux_loader::loader::Error),
    /// Failed to read the initrd file.
    #[error("reading the initrd image: {0}")]
    InitrdRead(#[source] std::io::Error),
    /// The initrd does not fit below the 32-bit MMIO gap.
    #[error("initrd ({size} bytes) does not fit in low memory")]
    InitrdTooBig {
        /// The initrd's size in bytes.
        size: usize,
    },
    /// Failed to write the initrd into guest memory.
    #[error("writing the initrd into guest memory")]
    InitrdWrite,
    /// Failed to build or write the kernel command line.
    #[error("writing the kernel command line: {0}")]
    Cmdline(#[source] linux_loader::cmdline::Error),
    /// The e820 map ran out of space.
    #[error("too many e820 entries")]
    E820,
    /// Failed to write `boot_params` ("zero page") into guest memory.
    #[error("writing boot_params into guest memory")]
    ZeroPage,
}

/// Load `kernel` (an already-decompressed, plain ELF vmlinux — see
/// the module docs for why it must be decompressed first) into
/// `guest_mem`, returning its 64-bit entry point.
pub fn load_kernel(kernel: &File, guest_mem: &GuestMemoryMmap) -> Result<EntryPoint, BootError> {
    let mut kernel_file = kernel.try_clone().map_err(BootError::InitrdRead)?;
    let result: KernelLoaderResult = Elf::load(
        guest_mem,
        None,
        &mut kernel_file,
        Some(GuestAddress(layout::HIMEM_START)),
    )
    .map_err(BootError::KernelLoad)?;
    Ok(EntryPoint {
        entry_addr: result.kernel_load,
        protocol: BootProtocol::LinuxBoot,
    })
}

/// Load `initrd` into `guest_mem`, placed just below the 32-bit MMIO
/// gap (or below whatever low-RAM ceiling is smaller) — never at
/// top-of-RAM, and never above 4 GiB: the `ramdisk_image` field
/// `configure_boot_params` writes into `boot_params.hdr` is 32 bits
/// wide (as is, separately, Linux's own PVH `modlist[0].paddr` copy —
/// this VMM never uses PVH, but the constraint is the header field
/// itself, not the boot path), so a top-of-RAM placement silently
/// truncates the address on any guest bigger than 4 GiB and the
/// kernel never finds its initramfs at all.
pub fn load_initrd(
    initrd: &mut File,
    guest_mem: &GuestMemoryMmap,
) -> Result<InitrdConfig, BootError> {
    let size = initrd
        .metadata()
        .map_err(BootError::InitrdRead)?
        .len()
        .try_into()
        .map_err(|_| BootError::InitrdTooBig { size: usize::MAX })?;

    let low_ram_end = guest_mem
        .iter()
        .map(|r| r.start_addr().raw_value() + r.len())
        .filter(|&end| end <= layout::MMIO32_MEM_START)
        .max()
        .unwrap_or(0);
    let addr = low_ram_end
        .checked_sub(size as u64)
        .ok_or(BootError::InitrdTooBig { size })?
        & !0xfff; // page-align down
    anyhow_ensure_initrd_fits(addr, size, layout::HIMEM_START)?;

    let mut buf = vec![0u8; size];
    std::io::Read::read_exact(initrd, &mut buf).map_err(BootError::InitrdRead)?;
    guest_mem
        .write_slice(&buf, GuestAddress(addr))
        .map_err(|_| BootError::InitrdWrite)?;
    Ok(InitrdConfig {
        address: GuestAddress(addr),
        size,
    })
}

fn anyhow_ensure_initrd_fits(addr: u64, size: usize, floor: u64) -> Result<(), BootError> {
    if addr < floor {
        return Err(BootError::InitrdTooBig { size });
    }
    Ok(())
}

/// Write the kernel command line at [`layout::CMDLINE_START`].
pub fn write_cmdline(guest_mem: &GuestMemoryMmap, cmdline: &str) -> Result<(), BootError> {
    let mut c = Cmdline::new(layout::CMDLINE_MAX_SIZE).map_err(BootError::Cmdline)?;
    c.insert_str(cmdline).map_err(BootError::Cmdline)?;
    load_cmdline(guest_mem, GuestAddress(layout::CMDLINE_START), &c).map_err(BootError::KernelLoad)
}

/// Build and write `boot_params` ("zero page"): the e820 map, the
/// initrd pointer, and the command-line pointer — ported from
/// Firecracker's `configure_64bit_boot`, minus its ACPI RSDP pointer
/// (this VMM has no ACPI tables at all — enumeration is legacy PCI
/// conf1 + an MP table).
pub fn configure_boot_params(
    guest_mem: &GuestMemoryMmap,
    initrd: &Option<InitrdConfig>,
) -> Result<(), BootError> {
    const KERNEL_BOOT_FLAG_MAGIC: u16 = 0xaa55;
    const KERNEL_HDR_MAGIC: u32 = 0x5372_6448;
    const KERNEL_LOADER_OTHER: u8 = 0xff;
    const KERNEL_MIN_ALIGNMENT_BYTES: u32 = 0x0100_0000;

    let himem_start = GuestAddress(layout::HIMEM_START);
    let mut params = boot_params::default();

    params.hdr.type_of_loader = KERNEL_LOADER_OTHER;
    params.hdr.boot_flag = KERNEL_BOOT_FLAG_MAGIC;
    params.hdr.header = KERNEL_HDR_MAGIC;
    params.hdr.cmd_line_ptr = layout::CMDLINE_START as u32;
    params.hdr.cmdline_size = layout::CMDLINE_MAX_SIZE as u32;
    params.hdr.kernel_alignment = KERNEL_MIN_ALIGNMENT_BYTES;
    if let Some(initrd) = initrd {
        params.hdr.ramdisk_image = u32::try_from(initrd.address.raw_value())
            .map_err(|_| BootError::InitrdTooBig { size: initrd.size })?;
        params.hdr.ramdisk_size = u32::try_from(initrd.size)
            .map_err(|_| BootError::InitrdTooBig { size: initrd.size })?;
    }

    add_e820_entry(&mut params, 0, layout::SYSTEM_MEM_START, E820_RAM)?;
    add_e820_entry(
        &mut params,
        layout::SYSTEM_MEM_START,
        layout::SYSTEM_MEM_SIZE,
        E820_RESERVED,
    )?;
    for region in guest_mem.iter() {
        let addr = std::cmp::max(himem_start, region.start_addr());
        if addr.raw_value() > region.last_addr().raw_value() {
            continue;
        }
        add_e820_entry(
            &mut params,
            addr.raw_value(),
            region.last_addr().unchecked_offset_from(addr) + 1,
            E820_RAM,
        )?;
    }

    LinuxBootConfigurator::write_bootparams(
        &BootParams::new(&params, GuestAddress(layout::ZERO_PAGE_START)),
        guest_mem,
    )
    .map_err(|_| BootError::ZeroPage)
}

fn add_e820_entry(
    params: &mut boot_params,
    addr: u64,
    size: u64,
    mem_type: u32,
) -> Result<(), BootError> {
    let n = params.e820_entries as usize;
    if n >= params.e820_table.len() {
        return Err(BootError::E820);
    }
    params.e820_table[n].addr = addr;
    params.e820_table[n].size = size;
    params.e820_table[n].type_ = mem_type;
    params.e820_entries += 1;
    Ok(())
}
