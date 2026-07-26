// SPDX-License-Identifier: Apache-2.0

//! Phase 3's capstone test: boot a real, unmodified, stock arm64
//! Linux `Image` under this backend end to end (`hvf::vm`/`vcpu`/
//! `gic`/`pl011`/`boot`/`machine` together) and confirm it reaches its
//! own kernel banner and panics cleanly for lack of a root
//! filesystem -- exactly `docs/design/0249`'s own stated phase 3
//! milestone.
//!
//! Requires `OCIVMM_TEST_KERNEL_IMAGE` to point at a real, plain
//! (not gzip- or EFI-zboot-wrapped -- see `hvf::boot`'s own module
//! docs) arm64 `Image` file on disk, and requires the compiled test
//! binary itself to be codesigned with the hypervisor entitlement
//! (`ci/codesign-ocivmm.sh`) -- neither of which plain `cargo test`
//! provides on its own, matching every other real-hardware `hvf`
//! test in this crate. A clear, explicit skip (not a silent one) if
//! the environment variable is unset, since most development/CI
//! environments won't have a suitable kernel Image lying around by
//! default; not yet wired into any CI job at all (`docs/design/0249`
//! phase 7 is what would do that, once a real provisioning story
//! exists to produce this file automatically).
//!
//! The `//!` doc comment above must stay *before* the `#![cfg(...)]`
//! below -- confirmed directly: `missing_docs` still fires for this
//! whole (empty, on any other target) crate otherwise, since a
//! crate-level `cfg` strips everything textually after it, doc
//! comments included (the working precedent is this crate's own
//! `lib.rs`, before phase 2 replaced its own whole-crate `cfg` with
//! per-module ones).

#![cfg(all(target_os = "macos", target_arch = "aarch64"))]
#![allow(unsafe_code)] // mmap + Vm::map: safety documented at each call site below.

use std::time::{Duration, Instant};

use oci_vmm::hvf::ImageHeader;
use oci_vmm::hvf::gic;
use oci_vmm::hvf::layout;
use oci_vmm::hvf::machine;
use oci_vmm::hvf::mmio::emulate;
use oci_vmm::hvf::pl011::Pl011;
use oci_vmm::hvf::sys::{HV_REG_CPSR, HV_REG_PC, HV_SYS_REG_MPIDR_EL1, hv_reg_x};
use oci_vmm::hvf::vcpu::{ExitReason, Vcpu};
use oci_vmm::hvf::vm::Vm;

/// Comfortably larger than any stock distro kernel `Image` this
/// project targets, with room for the device tree placed well past
/// it.
const RAM_SIZE: u64 = 256 * 1024 * 1024;
/// Where the device tree is placed: 192 MiB into RAM, safely past any
/// realistic kernel `Image` size.
const DTB_OFFSET: u64 = 192 * 1024 * 1024;

/// EC (bits `[31:26]` of `ESR_ELx`): Data Abort from a lower EL --
/// see `hvf::mmio`.
const ESR_EC_DATA_ABORT_LOWER_EL: u64 = 0x24;
/// EC: trapped `WFI`/`WFE` instruction execution -- treated as a
/// no-op continue (a spurious wakeup is always architecturally legal;
/// see this test's own comment at its call site).
const ESR_EC_WFI_WFE: u64 = 0x01;

/// Hard safety caps so a genuinely stuck guest (rather than one that
/// legitimately reaches a panic) fails this test loudly within a
/// bounded time, instead of hanging a `cargo test` run forever.
///
/// The wall-clock cap is enforced by a real watchdog thread calling
/// `hv_vcpus_exit` (not just a check *between* `vcpu.run()` calls):
/// once the guest kernel panics (`panic=-1`, no reboot), it settles
/// into a final idle/halt loop that does not reliably produce any
/// further exit at all -- confirmed directly developing this test,
/// where the very first version of this loop (deadline checked only
/// between calls) hung indefinitely at exactly that point, even
/// though the guest had already reached and printed the panic this
/// test is looking for. A single `vcpu.run()` call can otherwise
/// simply never return.
const MAX_ITERATIONS: u64 = 2_000_000;
const MAX_WALL_CLOCK: Duration = Duration::from_secs(30);

#[test]
fn boots_a_real_kernel_image_to_its_own_console_and_panics_without_a_rootfs() {
    let Ok(kernel_path) = std::env::var("OCIVMM_TEST_KERNEL_IMAGE") else {
        eprintln!(
            "skipping: set OCIVMM_TEST_KERNEL_IMAGE to point at a real distro aarch64 kernel \
             package's own vmlinuz/Image file to run this test -- `hvf::load_image` transparently \
             unwraps whichever wrapping (if any) it uses (EFI zboot, a bare gzip stream, or \
             neither), so any of Ubuntu's, CentOS Stream's, or Alpine's own real kernel packages \
             work directly, no manual pre-extraction needed"
        );
        return;
    };
    let raw_kernel =
        std::fs::read(&kernel_path).unwrap_or_else(|e| panic!("reading {kernel_path}: {e}"));
    let kernel = oci_vmm::hvf::load_image(&raw_kernel)
        .expect("unwrap the kernel package's own vmlinuz/Image");
    let header = ImageHeader::parse(&kernel).expect("parse arm64 Image header");
    let entry_addr = header.entry_address(layout::RAM_BASE);
    let kernel_offset = entry_addr - layout::RAM_BASE;
    assert!(
        kernel_offset + kernel.len() as u64 <= DTB_OFFSET,
        "kernel Image ({} bytes) does not fit before this test's own DTB_OFFSET",
        kernel.len()
    );

    // SAFETY: a fresh, anonymous, private, host-RW (never RWX --
    // macOS denies that outright for a plain mmap, confirmed while
    // developing hvf's very first smoke test) mapping, exactly
    // RAM_SIZE bytes, outliving this whole test (leaked deliberately,
    // same tradeoff as every other hvf hardware test).
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
    assert_ne!(
        host_ptr,
        libc::MAP_FAILED,
        "mmap RAM_SIZE bytes of guest RAM"
    );
    let host_ptr = host_ptr.cast::<u8>();

    // SAFETY: `host_ptr..+RAM_SIZE` is the fresh mapping above;
    // `kernel_offset + kernel.len() <= DTB_OFFSET < RAM_SIZE` was just
    // asserted, so the copy stays within it.
    unsafe {
        host_ptr
            .add(kernel_offset as usize)
            .copy_from_nonoverlapping(kernel.as_ptr(), kernel.len());
    }

    let vm = Vm::create().expect("hv_vm_create (codesigned with the hypervisor entitlement?)");
    // SAFETY: `host_ptr`/`RAM_SIZE` describe the mmap above.
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
        "console=ttyAMA0 panic=-1 earlycon=pl011,0x9000000",
        None,
        0, // No virtio-mmio devices in this test -- see hvf_virtio_blk.rs for phase 4's own.
    );
    assert!(
        DTB_OFFSET + dtb.len() as u64 <= RAM_SIZE,
        "device tree ({} bytes) does not fit within RAM_SIZE past DTB_OFFSET",
        dtb.len()
    );
    // SAFETY: same mapping as above; the just-checked bound keeps the
    // copy within it.
    unsafe {
        host_ptr
            .add(DTB_OFFSET as usize)
            .copy_from_nonoverlapping(dtb.as_ptr(), dtb.len());
    }

    // Must be created after hv_gic_create, per its own documented
    // ordering requirement.
    let vcpu = Vcpu::create(&vm).expect("hv_vcpu_create");
    vcpu.set_reg(HV_REG_PC, entry_addr).unwrap();
    vcpu.set_reg(hv_reg_x(0), dtb_addr).unwrap(); // x0: dtb physical address.
    vcpu.set_reg(hv_reg_x(1), 0).unwrap();
    vcpu.set_reg(hv_reg_x(2), 0).unwrap();
    vcpu.set_reg(hv_reg_x(3), 0).unwrap();
    vcpu.set_reg(HV_REG_CPSR, 0x3c5).unwrap(); // EL1h, all DAIF masks set.
    vcpu.set_sys_reg(HV_SYS_REG_MPIDR_EL1, 0x8000_0000).unwrap(); // Affinity 0, matching the device tree's own cpu@0 "reg".
    // Every real distro kernel's debug-monitor bring-up writes
    // OSDLR_EL1 unconditionally, which traps by default -- this
    // backend emulates none of the ARM debug architecture, so lets
    // the guest access it (and MDSCR_EL1/DBG*_EL1) natively instead.
    vcpu.set_trap_debug(false).unwrap();

    // See MAX_WALL_CLOCK's own doc comment on why this watchdog
    // exists rather than just checking a deadline between
    // `vcpu.run()` calls: `hv_vcpus_exit` is explicitly documented as
    // safe to call from another thread (unlike every other `hv_vcpu_*`
    // call), specifically for cancelling a blocked `run()`.
    static VCPU_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    VCPU_ID.store(vcpu.id(), std::sync::atomic::Ordering::SeqCst);
    std::thread::spawn(|| {
        std::thread::sleep(MAX_WALL_CLOCK);
        let mut id = VCPU_ID.load(std::sync::atomic::Ordering::SeqCst);
        let _ = unsafe { oci_vmm::hvf::sys::hv_vcpus_exit(&mut id, 1) };
    });

    let mut console = Pl011::new(Vec::new());
    let deadline = Instant::now() + MAX_WALL_CLOCK;
    let mut iterations: u64 = 0;
    let mut stopped_reason: Option<String> = None;

    while Instant::now() < deadline && iterations < MAX_ITERATIONS {
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
                    } else {
                        stopped_reason = Some(format!(
                            "unhandled data abort at guest physical address {addr:#x} \
                             (ESR_ELx={:#x})",
                            exception.syndrome
                        ));
                        break;
                    }
                } else if ec == ESR_EC_WFI_WFE {
                    // A spurious WFI/WFE wakeup is always
                    // architecturally legal; Linux never assumes a
                    // specific reason and always re-checks its actual
                    // wait condition. Just resume.
                    continue;
                } else if ec == oci_vmm::hvf::sysreg_trap::ESR_EC_SYS64 {
                    oci_vmm::hvf::sysreg_trap::emulate(&vcpu, &exception)
                        .expect("emulate sys64 trap");
                } else {
                    stopped_reason = Some(format!(
                        "unexpected exception, EC={ec:#x} ESR_ELx={:#x}",
                        exception.syndrome
                    ));
                    break;
                }
            }
            ExitReason::VtimerActivated => {
                // Not yet established whether this backend needs to
                // do anything at all here with hvf::gic active (see
                // Vcpu::set_vtimer_mask's own doc comment) -- unmask
                // and continue is the documented action for a VMM
                // with no GIC device of its own; tried here as the
                // most plausible action either way.
                vcpu.set_vtimer_mask(false).expect("unmask vtimer");
            }
            ExitReason::Canceled => {
                // The watchdog fired: MAX_WALL_CLOCK elapsed with the
                // vCPU still blocked in a single run() call (expected
                // once the guest reaches its own final post-panic
                // idle loop -- see MAX_WALL_CLOCK's own doc comment).
                // Not itself a failure; the assertions below decide
                // pass/fail from whatever console output was actually
                // captured before this happened.
                break;
            }
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

    // `stopped_reason` being set means an exception this test doesn't
    // recognize at all occurred (not the expected watchdog-driven
    // `Canceled`, which never sets it) -- a real, unexpected failure
    // regardless of what console output happened to be captured
    // before it.
    assert!(
        stopped_reason.is_none(),
        "stopped for an unhandled reason before completing: {stopped_reason:?}"
    );
    assert!(
        iterations < MAX_ITERATIONS,
        "hit the {MAX_ITERATIONS} vCPU-exit safety cap without the guest reaching a terminal \
         state -- see captured console output above"
    );
    assert!(
        output.contains("Linux version") || output.contains("Booting Linux"),
        "guest console output never showed a kernel banner -- see captured output above"
    );
    assert!(
        output.contains("Kernel panic") || output.contains("VFS: Unable to mount root fs"),
        "expected a clean panic for lack of a root filesystem (no initrd/root= was given) -- \
         see captured output above"
    );
}
