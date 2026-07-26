// SPDX-License-Identifier: Apache-2.0

//! Magic addresses used to lay out this backend's aarch64 guests --
//! the aarch64 analogue of `crate::arch::layout` (x86_64/KVM).
//!
//! Deliberately matches QEMU's own well-known `virt` machine memory
//! map (`hw/arm/virt.c`) address-for-address, rather than inventing
//! this backend's own arbitrary layout: real distro device trees,
//! bootloaders, and `earlycon=` kernel command-line conventions all
//! already assume these addresses for a generic arm64 "virt"-shaped
//! board, and matching them for real means `qemu-system-aarch64 -M
//! virt -accel hvf` (also Hypervisor.framework-accelerated, also
//! available on this same Apple Silicon development hardware) can be
//! used as an independent reference implementation to boot the exact
//! same kernel Image/initrd/cmdline against while developing this
//! backend -- a cross-check this project's x86_64/KVM port never had
//! available in comparable form.

/// Guest RAM base address (QEMU `virt`'s own `VIRT_MEM` base).
pub const RAM_BASE: u64 = 0x4000_0000;

/// GICv3 distributor region base address (QEMU `virt`'s own
/// `VIRT_GIC_DIST`).
pub const GIC_DISTRIBUTOR_BASE: u64 = 0x0800_0000;

/// GICv3 redistributor region base address (QEMU `virt`'s own
/// `VIRT_GIC_REDIST`), covering every vCPU's own frame contiguously.
pub const GIC_REDISTRIBUTOR_BASE: u64 = 0x080a_0000;

/// PL011 UART0 base address (QEMU `virt`'s own `VIRT_UART0`) --
/// matches the address several distros' own `earlycon=pl011,
/// 0x9000000` kernel command-line conventions already assume, though
/// this backend's device tree describes it explicitly regardless.
pub const PL011_BASE: u64 = 0x0900_0000;
/// PL011 register region size (one 4 KiB page is ample; the real
/// register file is under 256 bytes).
pub const PL011_SIZE: u64 = 0x1000;

/// PL011 UART0's SPI (Shared Peripheral Interrupt) number (QEMU
/// `virt`'s own convention: SPI 1, i.e. GIC INTID 33 once offset by
/// `GIC_SPI_BASE`). Not currently asserted by this backend (see
/// `crate::hvf::pl011`'s own module docs on why no interrupt is ever
/// raised yet), but still required in the device tree for the
/// guest's `amba` bus probe to succeed.
pub const PL011_SPI: u32 = 1;

/// The first SPI (Shared Peripheral Interrupt) number in GIC INTID
/// space (`GIC_SPI_BASE + n` is INTID `32 + n`; INTIDs 0..31 are
/// SGIs/PPIs, private to each vCPU, never used for a platform device
/// like PL011).
pub const GIC_SPI_BASE: u32 = 32;
