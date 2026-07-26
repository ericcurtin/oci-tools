// SPDX-License-Identifier: Apache-2.0

//! Phase 4's first capstone test: boot a real, unmodified, stock
//! arm64 Linux `Image` (same requirement as `hvf_boot.rs`) with a
//! virtio-mmio virtio-blk device attached, and confirm the guest's
//! own `virtio_blk` driver detects it and reports its real size --
//! `hvf::virtio_mmio`/`hvf::virtio_blk` exercised through the full
//! stack (device tree, MMIO trap-and-emulate, the shared
//! `virtio::queue`/`virtio::block::{disk,request}` protocol logic),
//! not just unit-tested in isolation.
//!
//! Requires `OCIVMM_TEST_KERNEL_IMAGE` (see `hvf_boot.rs`) and
//! `OCIVMM_TEST_DISK_IMAGE` (any regular file at least a few sectors
//! long -- its exact contents don't matter for this test, only that
//! the guest can see and report its size). A clear, explicit skip if
//! either is unset, matching `hvf_boot.rs`.
//!
//! Currently `#[ignore]`d: it does not pass yet. See the doc comment
//! on the test function itself for the full, honest writeup of
//! what's confirmed working and what open question remains -- this
//! is a known, real, currently-unresolved issue, not a placeholder or
//! an oversight (`docs/design/0249` phase 4's own status section
//! tracks it too).

//! The `//!` doc comment above must stay before `#![cfg(...)]` --
//! see `hvf_boot.rs`'s own note on why.

#![cfg(all(target_os = "macos", target_arch = "aarch64"))]
#![allow(unsafe_code)] // mmap + Vm::map: safety documented at each call site below.

use std::time::Duration;

use oci_vmm::hvf::ImageHeader;
use oci_vmm::hvf::gic;
use oci_vmm::hvf::layout;
use oci_vmm::hvf::machine;
use oci_vmm::hvf::mmio::emulate;
use oci_vmm::hvf::pl011::Pl011;
use oci_vmm::hvf::sys::{HV_REG_CPSR, HV_REG_PC, HV_SYS_REG_MPIDR_EL1, hv_reg_x};
use oci_vmm::hvf::vcpu::{ExitReason, Vcpu};
use oci_vmm::hvf::virtio_blk::VirtioBlkMmio;
use oci_vmm::hvf::virtio_mmio::VirtioMmioTransport;
use oci_vmm::hvf::vm::Vm;
use oci_vmm::mem::GuestMemoryMmap;

const RAM_SIZE: u64 = 256 * 1024 * 1024;
const DTB_OFFSET: u64 = 192 * 1024 * 1024;

const ESR_EC_DATA_ABORT_LOWER_EL: u64 = 0x24;
const ESR_EC_WFI_WFE: u64 = 0x01;

const MAX_ITERATIONS: u64 = 4_000_000;
const MAX_WALL_CLOCK: Duration = Duration::from_secs(30);

/// Known, unresolved issue as of `docs/design/0249` phase 4: this
/// test currently fails. What's confirmed, real, and working:
///
/// * The device tree's `virtio_mmio@...` node is well-formed (`dtc`
///   accepts it -- see `hvf::machine`'s own tests) and the guest's
///   `virtio_mmio` platform driver *does* find and match it (a log
///   line names the exact node: `a000000.virtio_mmio`), meaning the
///   `compatible`/`reg`/`interrupts` properties are all correct and
///   `hvf::mmio`'s Data-Abort-based trap-and-emulate is intact (the
///   same mechanism this backend's PL011 console already proved
///   solidly, in `hvf_boot.rs`).
/// * `hvf::virtio_mmio`'s register file itself is never actually
///   reached: the guest's `virtio_mmio_probe` fails at
///   `devm_request_mem_region()` -- a pure Linux resource-tree
///   operation with **no MMIO access to the device at all** -- with
///   `-EBUSY` ("can't request region for resource [mem
///   0x0a000000-0x0a0001ff]"), before ever reading so much as
///   `MagicValue`.
/// * The *identical* symptom (`OF: amba_device_add() failed (-16)`,
///   the AMBA-bus equivalent of the same `request_resource` call) has
///   actually been present since phase 3's own PL011 node -- it just
///   never blocked that phase's milestone, since `earlycon=` bypasses
///   the AMBA bus entirely and keeps working regardless of whether
///   the "real" driver ever attaches. Phase 4's milestone has no such
///   fallback: virtio has no "early" analogue, so this now blocks
///   real progress.
/// * Ruled out directly, not just assumed: the device tree's own
///   content (an external, independent parser, `dtc`, accepts it, and
///   a plain-Rust re-derivation of the exact same bytes was booted
///   under `qemu-system-aarch64 -M virt -accel hvf -dtb ...` -- itself
///   also Hypervisor.framework-backed, on this same hardware --
///   without reproducing this failure, though that comparison turned
///   out to be confounded: QEMU's own auto-generated devices don't
///   necessarily land at the addresses an externally supplied `-dtb`
///   claims, so a "no error" result there is inconclusive, not a
///   clean rule-out). Also ruled out: a missing `dma-coherent`
///   property on the virtio_mmio node (matching Firecracker's own
///   aarch64 FDT convention, since added -- see `hvf::machine` --
///   but confirmed by direct retest *not* to be the cause) and
///   `earlycon=` itself (removing it from the command line does not
///   change the underlying failure, only this test's own ability to
///   observe anything at all before it, since without earlycon there
///   is no console output whatsoever until a real driver attaches).
/// * Not yet ruled out: exactly what, if anything, is already present
///   in the kernel's `iomem_resource` tree at these addresses before
///   `virtio_mmio`/`amba` ever try to claim them -- the kernel's own
///   default `request_resource` failure path doesn't name the
///   conflicting owner, and diagnosing further would need either a
///   kernel with `CONFIG_DEBUG_KERNEL`-style resource-tree
///   introspection, a serial-attached debugger, or ftrace -- none
///   available without a working root filesystem, itself blocked by
///   this very bug.
/// * Cross-checked directly against libkrun's own HVF backend
///   (`/Users/ecurtin/git/libkrun` locally) for anything this backend
///   might be missing: nothing found. libkrun's `hv_vm_map` usage is
///   identical (guest RAM only, `READ|WRITE|EXEC`; every MMIO device
///   region -- including its own virtio-mmio devices -- is left
///   unmapped and handled via the same trap-and-emulate HVF itself
///   provides, not mapped with any special "device" memory attribute
///   at all); `hv_gic_create` always precedes every `hv_vcpu_create`,
///   same as here; the boot CPSR is bit-for-bit the same
///   (`EL1h`, all four DAIF bits masked, `0x3c5`); and libkrun never
///   calls `hv_vcpu_set_trap_debug_exceptions`/
///   `hv_vcpu_set_trap_debug_reg_accesses` at all (relies on
///   whatever HVF's own default is) nor touches `SCTLR_EL1`/
///   `TCR_EL1`/`MAIR_EL1`/`VBAR_EL1`/`CNTKCTL_EL1`/`ID_AA64MMFR0_EL1`/
///   IPA size -- the same minimal register set this backend sets
///   (`MPIDR_EL1`, `PC`, `X0`, `CPSR`) and nothing more. A repo-wide
///   search of libkrun for `iomem`/`request_region`/`EBUSY`/`-16`/
///   resource-conflict handling turns up nothing at all -- this bug
///   class doesn't appear in their code.
/// * **The bug is confirmed 100% generic, not address- or
///   device-specific**: a throwaway third device-tree node (a PL031
///   RTC, `arm,pl031`/`arm,primecell`, at `0x09010000` -- a brand-new
///   address never previously used by this backend at all) hits the
///   *exact* same `OF: amba_device_add() failed (-16)` the very first
///   time it's ever tried. Every non-RAM `iomem` resource request this
///   backend's guest has ever attempted -- PL011, virtio-mmio, and
///   this throwaway PL031 -- fails identically, regardless of
///   address, AMBA-vs-plain-platform-device, or driver. Also checked
///   and ruled out: `ID_AA64MMFR0_EL1`'s `PARange` field (`0x2`,
///   40-bit/1 TiB) and `SCTLR_EL1`/`TCR_EL1`/`MAIR_EL1` (all `0` at
///   reset, MMU off as expected) are all ordinary, sane values, not
///   some corrupt/unusual vCPU reset state.
/// * Given the bug's proven genericity, the leading remaining
///   hypothesis (not yet confirmed) is that something makes the
///   *entire* non-RAM address space look already-claimed to Linux's
///   `iomem_resource` tree -- e.g. an oversized/miscomputed "System
///   RAM", "reserved", or similar resource -- rather than anything
///   about any *individual* device's own registration. Confirming
///   that would need actual kernel-side introspection (a debug
///   kernel build with `iomem_resource` tree dumping, `kgdb`, or
///   `ftrace`) that isn't available without a working root filesystem
///   -- itself blocked by this very bug.
#[test]
#[ignore = "known issue: virtio-mmio/amba devm_request_mem_region fails with -EBUSY before any \
            MMIO access reaches this backend at all -- see this test's own doc comment and \
            docs/design/0249 phase 4"]
fn guest_virtio_blk_driver_detects_the_disk_and_reports_its_size() {
    let (Ok(kernel_path), Ok(disk_path)) = (
        std::env::var("OCIVMM_TEST_KERNEL_IMAGE"),
        std::env::var("OCIVMM_TEST_DISK_IMAGE"),
    ) else {
        eprintln!(
            "skipping: set OCIVMM_TEST_KERNEL_IMAGE (see hvf_boot.rs) and \
             OCIVMM_TEST_DISK_IMAGE (any regular file, at least a few sectors long) to run \
             this test"
        );
        return;
    };
    let disk_len = std::fs::metadata(&disk_path)
        .unwrap_or_else(|e| panic!("stat {disk_path}: {e}"))
        .len();
    let expected_sectors = disk_len / 512;

    let kernel =
        std::fs::read(&kernel_path).unwrap_or_else(|e| panic!("reading {kernel_path}: {e}"));
    let header = ImageHeader::parse(&kernel).expect("parse arm64 Image header");
    let entry_addr = header.entry_address(layout::RAM_BASE);
    let kernel_offset = entry_addr - layout::RAM_BASE;
    assert!(kernel_offset + kernel.len() as u64 <= DTB_OFFSET);

    // SAFETY: a fresh, anonymous, private, host-RW mapping, exactly
    // RAM_SIZE bytes -- see hvf_boot.rs's own identical comment.
    let host_ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            RAM_SIZE as usize,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        )
    };
    assert_ne!(host_ptr, libc::MAP_FAILED);
    let host_ptr = host_ptr.cast::<u8>();
    unsafe {
        host_ptr
            .add(kernel_offset as usize)
            .copy_from_nonoverlapping(kernel.as_ptr(), kernel.len());
    }

    let vm = Vm::create().expect("hv_vm_create (codesigned with the hypervisor entitlement?)");
    unsafe {
        vm.map(
            host_ptr,
            layout::RAM_BASE,
            RAM_SIZE as usize,
            true,
            true,
            true,
        )
        .expect("hv_vm_map (guest RAM)");
    }

    let gic_layout = gic::create(
        &vm,
        layout::GIC_DISTRIBUTOR_BASE,
        layout::GIC_REDISTRIBUTOR_BASE,
    )
    .expect("hv_gic_create");

    let dtb_addr = layout::RAM_BASE + DTB_OFFSET;
    let dtb = machine::build_device_tree(
        &gic_layout,
        RAM_SIZE,
        "console=ttyAMA0 panic=-1 earlycon=pl011,0x9000000 root=/dev/vda ro",
        None,
        1, // One virtio-mmio slot: the block device below.
    );
    assert!(DTB_OFFSET + dtb.len() as u64 <= RAM_SIZE);
    unsafe {
        host_ptr
            .add(DTB_OFFSET as usize)
            .copy_from_nonoverlapping(dtb.as_ptr(), dtb.len());
    }

    let vcpu = Vcpu::create(&vm).expect("hv_vcpu_create");
    vcpu.set_reg(HV_REG_PC, entry_addr).unwrap();
    vcpu.set_reg(hv_reg_x(0), dtb_addr).unwrap();
    vcpu.set_reg(hv_reg_x(1), 0).unwrap();
    vcpu.set_reg(hv_reg_x(2), 0).unwrap();
    vcpu.set_reg(hv_reg_x(3), 0).unwrap();
    vcpu.set_reg(HV_REG_CPSR, 0x3c5).unwrap();
    vcpu.set_sys_reg(HV_SYS_REG_MPIDR_EL1, 0x8000_0000).unwrap();
    vcpu.set_trap_debug(false).unwrap();

    static VCPU_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    VCPU_ID.store(vcpu.id(), std::sync::atomic::Ordering::SeqCst);
    std::thread::spawn(|| {
        std::thread::sleep(MAX_WALL_CLOCK);
        let mut id = VCPU_ID.load(std::sync::atomic::Ordering::SeqCst);
        let _ = unsafe { oci_vmm::hvf::sys::hv_vcpus_exit(&mut id, 1) };
    });

    // SAFETY: `mem` describes exactly the same host mapping/guest
    // base/size already registered with `vm.map` above; `GuestMemoryMmap`
    // is a thin, cheaply-clonable handle onto it (no separate mapping
    // of its own), matching how the rest of this test already treats
    // `host_ptr`/`RAM_SIZE` as the single source of truth for guest RAM.
    let mem = GuestMemoryMmap::from_ranges(&[(
        vm_memory::GuestAddress(layout::RAM_BASE),
        RAM_SIZE as usize,
    )])
    .expect("build a GuestMemoryMmap view for the virtio queue/request code");

    let blk_device = VirtioBlkMmio::new(disk_path.clone().into(), true, "ocivmm-test-disk")
        .unwrap_or_else(|e| panic!("opening {disk_path}: {e}"));
    let virtio_spi = layout::GIC_SPI_BASE + layout::VIRTIO_MMIO_SPI_BASE;
    let mut virtio_blk = VirtioMmioTransport::new(blk_device, mem, virtio_spi);

    let mut console = Pl011::new(Vec::new());
    let mut iterations: u64 = 0;
    let mut stopped_reason: Option<String> = None;

    while iterations < MAX_ITERATIONS {
        iterations += 1;
        match vcpu.run().expect("hv_vcpu_run") {
            ExitReason::Exception(exception) => {
                let ec = (exception.syndrome >> 26) & 0x3f;
                if ec == ESR_EC_DATA_ABORT_LOWER_EL {
                    let addr = exception.physical_address;
                    if (layout::PL011_BASE..layout::PL011_BASE + layout::PL011_SIZE).contains(&addr)
                    {
                        emulate(&vcpu, &exception, layout::PL011_BASE, &mut console)
                            .expect("emulate PL011 access");
                    } else if (layout::VIRTIO_MMIO_BASE
                        ..layout::VIRTIO_MMIO_BASE + layout::VIRTIO_MMIO_SIZE)
                        .contains(&addr)
                    {
                        emulate(&vcpu, &exception, layout::VIRTIO_MMIO_BASE, &mut virtio_blk)
                            .expect("emulate virtio-mmio access");
                    } else {
                        stopped_reason = Some(format!(
                            "unhandled data abort at {addr:#x} (ESR_ELx={:#x})",
                            exception.syndrome
                        ));
                        break;
                    }
                } else if ec == ESR_EC_WFI_WFE {
                    continue;
                } else if ec == oci_vmm::hvf::sysreg_trap::ESR_EC_SYS64 {
                    oci_vmm::hvf::sysreg_trap::emulate(&vcpu, &exception)
                        .expect("emulate sys64 trap");
                } else {
                    stopped_reason = Some(format!(
                        "unexpected EC={ec:#x} ESR_ELx={:#x}",
                        exception.syndrome
                    ));
                    break;
                }
            }
            ExitReason::VtimerActivated => {
                vcpu.set_vtimer_mask(false).expect("unmask vtimer");
            }
            ExitReason::Canceled => break,
            other => {
                stopped_reason = Some(format!("unexpected exit reason: {other:?}"));
                break;
            }
        }
    }

    let console_bytes = console.into_inner();
    let output = String::from_utf8_lossy(&console_bytes);
    eprintln!("---- guest console output ({iterations} vCPU exits) ----\n{output}\n----");
    if let Some(reason) = &stopped_reason {
        eprintln!("stopped early: {reason}");
    }

    assert!(stopped_reason.is_none(), "{stopped_reason:?}");
    assert!(iterations < MAX_ITERATIONS, "hit the vCPU-exit safety cap");
    assert!(
        output.contains("Linux version") || output.contains("Booting Linux"),
        "no kernel banner in captured output"
    );
    assert!(
        output.contains("virtio_blk") && output.contains("vda"),
        "guest never detected the virtio-blk disk as vda -- see captured output above"
    );
    assert!(
        output.contains(&format!("{expected_sectors} 512-byte")),
        "guest-reported sector count doesn't match the real disk image size ({expected_sectors} \
         sectors expected) -- see captured output above"
    );
}
