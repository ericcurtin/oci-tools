// SPDX-License-Identifier: Apache-2.0

//! Builds the one, fixed device tree this backend's pet VMs boot
//! with: `/memory`, a single always-on boot `/cpus/cpu@0` (no `psci`
//! node at all -- see below), the GICv3 `hvf::gic` created, and the
//! `hvf::pl011` console -- the aarch64 analogue of how
//! `crate::arch::mptable` describes devices to the guest on the
//! x86_64/KVM side, just via a device tree instead of an MP table.
//!
//! No secondary-CPU support yet, deliberately: a `psci`-enable-method
//! CPU requires this backend to answer real PSCI HVC calls
//! (`CPU_ON`, `PSCI_VERSION`, ...), which nothing here implements --
//! omitting the `/psci` node entirely means Linux's own
//! `psci_dt_init` finds no such node and skips PSCI setup outright,
//! so the boot CPU never attempts one. A future phase adding real SMP
//! would need both a `/psci` node and an HVC-call responder in
//! `hvf::vcpu`'s own exit loop.

use crate::hvf::fdt::FdtWriter;
use crate::hvf::gic::GicLayout;
use crate::hvf::layout;

/// Builds the complete device tree blob for a single-vCPU pet VM with
/// `ram_size` bytes of RAM based at [`layout::RAM_BASE`], the given
/// `gic` device (already created via [`crate::hvf::gic::create`]),
/// and kernel command line `bootargs`. `initrd`, if present, is a
/// `(start, end)` guest physical address pair (`/chosen`'s own
/// `linux,initrd-start`/`-end` convention -- `end` is exclusive, one
/// byte past the last initrd byte, per the binding).
pub fn build_device_tree(
    gic: &GicLayout,
    ram_size: u64,
    bootargs: &str,
    initrd: Option<(u64, u64)>,
) -> Vec<u8> {
    let mut fdt = FdtWriter::new();

    // phandle 2: the GIC node below. Referenced by the root's own
    // "interrupt-parent" (every device without its own explicit
    // interrupt-parent inherits it from there -- the same convention
    // QEMU's own generated `virt` device tree uses) and would be
    // referenced the same way by any future device's "interrupts"
    // property that needs a *different* parent than the default.
    const GIC_PHANDLE: u32 = 2;

    fdt.begin_node("");
    fdt.property_u32("#address-cells", 2);
    fdt.property_u32("#size-cells", 2);
    fdt.property_strings("compatible", &["linux,dummy-virt"]);
    fdt.property_string("model", "oci-tools,ocivmm-hvf");
    fdt.property_u32("interrupt-parent", GIC_PHANDLE);

    fdt.begin_node("chosen");
    fdt.property_string("bootargs", bootargs);
    fdt.property_string("stdout-path", &format!("/pl011@{:x}", layout::PL011_BASE));
    if let Some((start, end)) = initrd {
        fdt.property_cells("linux,initrd-start", &be64_cells(start));
        fdt.property_cells("linux,initrd-end", &be64_cells(end));
    }
    fdt.end_node();

    fdt.begin_node(&format!("memory@{:x}", layout::RAM_BASE));
    fdt.property_string("device_type", "memory");
    fdt.property_cells("reg", &be64_pair_cells(layout::RAM_BASE, ram_size));
    fdt.end_node();

    fdt.begin_node("cpus");
    fdt.property_u32("#address-cells", 1);
    fdt.property_u32("#size-cells", 0);

    fdt.begin_node("cpu@0");
    fdt.property_string("device_type", "cpu");
    fdt.property_strings("compatible", &["arm,armv8"]);
    // A single, always-on boot CPU needs no "enable-method" at all
    // (that property only applies to *secondary* CPUs -- see the
    // module docs on why this backend has none yet).
    fdt.property_cells("reg", &[0]); // MPIDR_EL1 affinity 0, matching hvf::gic's own test/boot setup.
    fdt.end_node();

    fdt.end_node(); // cpus

    // The ARM generic architected timer -- *required* for Linux's
    // `arch_timer` driver (keyed off this exact `compatible` string
    // via `CLOCKSOURCE_OF_DECLARE`) to register a clockevent device
    // at all; found the hard way, cross-checking against a real
    // reference implementation (`qemu-system-aarch64 -M virt -accel
    // hvf`, itself Hypervisor.framework-backed, on this same Apple
    // Silicon hardware) after this backend's own boot attempt hung
    // indefinitely (spinning at a single, unchanging PC deep in
    // kernel-virtual-address C code, confirmed by forcibly
    // `hv_vcpus_exit`-cancelling the vCPU mid-hang and re-checking its
    // PC across several rounds) with no `/timer` node at all: without
    // one, the kernel has no clockevent/jiffies source, and enough of
    // early boot depends on jiffies actually advancing that it never
    // progresses. The four standard PPI numbers here (secure/non-
    // secure physical, virtual, hypervisor) match both QEMU's own
    // generated tree and Hypervisor.framework's own `hv_gic_intid_t`
    // values (`HV_GIC_INT_EL1_VIRTUAL_TIMER = 27` = PPI 11 = INTID
    // 16+11, the third entry below).
    fdt.begin_node("timer");
    fdt.property_strings("compatible", &["arm,armv8-timer", "arm,armv7-timer"]);
    fdt.property_empty("always-on");
    fdt.property_cells(
        "interrupts",
        &[
            1, 13, 4, // secure physical timer (PPI 13, level-high)
            1, 14, 4, // non-secure physical timer (PPI 14, level-high)
            1, 11, 4, // virtual timer (PPI 11, level-high)
            1, 10, 4, // hypervisor physical timer (PPI 10, level-high)
        ],
    );
    fdt.end_node();

    fdt.begin_node(&format!(
        "interrupt-controller@{:x}",
        layout::GIC_DISTRIBUTOR_BASE
    ));
    fdt.property_strings("compatible", &["arm,gic-v3"]);
    fdt.property_u32("#interrupt-cells", 3);
    fdt.property_empty("interrupt-controller");
    fdt.property_cells(
        "reg",
        &[
            &be64_pair_cells(gic.distributor_base, gic.distributor_size)[..],
            &be64_pair_cells(gic.redistributor_base, gic.redistributor_size)[..],
        ]
        .concat(),
    );
    fdt.property_u32("phandle", GIC_PHANDLE);
    fdt.end_node();

    // A fixed reference clock: amba-pl011's own driver requires a
    // working `apb_pclk` clock lookup to probe successfully at all
    // (`devm_clk_get`) -- found the hard way by cross-checking
    // against QEMU's own generated `virt` device tree, which provides
    // exactly this node for the same reason.
    // phandle 1: referenced by pl011@...'s own "clocks" property
    // below. This writer has no automatic phandle-allocation pass (a
    // single, fixed tree shape doesn't need one) -- "phandle" is just
    // an ordinary u32 property by this name, the same as real `.dts`
    // source's `<&label>` syntax compiles down to.
    const APB_PCLK_PHANDLE: u32 = 1;

    fdt.begin_node("apb-pclk");
    fdt.property_strings("compatible", &["fixed-clock"]);
    fdt.property_u32("#clock-cells", 0);
    fdt.property_u32("clock-frequency", 24_000_000);
    fdt.property_string("clock-output-names", "clk24mhz");
    fdt.property_u32("phandle", APB_PCLK_PHANDLE);
    fdt.end_node();

    fdt.begin_node(&format!("pl011@{:x}", layout::PL011_BASE));
    fdt.property_strings("compatible", &["arm,pl011", "arm,primecell"]);
    fdt.property_cells(
        "reg",
        &be64_pair_cells(layout::PL011_BASE, layout::PL011_SIZE),
    );
    fdt.property_cells(
        "interrupts",
        &[0, layout::PL011_SPI, 4], // <GIC_SPI 1 IRQ_TYPE_LEVEL_HIGH>: type=SPI(0), num=1, flags=level-high(4).
    );
    fdt.property_strings("clock-names", &["uartclk", "apb_pclk"]);
    // Both clock-names entries point at the same fixed-clock
    // (`apb-pclk`'s own `#clock-cells` is 0, so each reference is a
    // single cell: just its phandle, no further arguments).
    fdt.property_cells("clocks", &[APB_PCLK_PHANDLE, APB_PCLK_PHANDLE]);
    fdt.end_node();

    fdt.end_node(); // root

    fdt.finish(0)
}

fn be64_cells(value: u64) -> [u32; 2] {
    [(value >> 32) as u32, value as u32]
}

fn be64_pair_cells(a: u64, b: u64) -> [u32; 4] {
    let a = be64_cells(a);
    let b = be64_cells(b);
    [a[0], a[1], b[0], b[1]]
}

#[cfg(test)]
mod tests {
    //! Cross-checked against `dtc` (see `hvf::fdt`'s own tests for
    //! why), not just this module's own self-consistency.

    use super::*;
    use std::io::Write;
    use std::process::{Command, Stdio};

    fn decompile(dtb: &[u8]) -> String {
        let mut child = Command::new("dtc")
            .args(["-I", "dtb", "-O", "dts"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("`dtc` not found on PATH");
        child.stdin.take().unwrap().write_all(dtb).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "dtc rejected the generated dtb:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    #[test]
    fn builds_a_dtc_acceptable_tree_with_the_expected_devices() {
        let gic = GicLayout {
            distributor_base: layout::GIC_DISTRIBUTOR_BASE,
            distributor_size: 0x10000,
            redistributor_base: layout::GIC_REDISTRIBUTOR_BASE,
            redistributor_size: 0x20000, // one vCPU's worth (2x64 KiB frames).
        };
        let dtb = build_device_tree(&gic, 256 * 1024 * 1024, "console=ttyAMA0 panic=-1", None);
        let dts = decompile(&dtb);

        assert!(dts.contains("compatible = \"linux,dummy-virt\";"), "{dts}");
        assert!(dts.contains("device_type = \"memory\";"), "{dts}");
        assert!(dts.contains("compatible = \"arm,gic-v3\";"), "{dts}");
        assert!(
            dts.contains("compatible = \"arm,pl011\", \"arm,primecell\";"),
            "{dts}"
        );
        assert!(dts.contains("compatible = \"fixed-clock\";"), "{dts}");
        assert!(dts.contains("console=ttyAMA0 panic=-1"), "{dts}");
        assert!(dts.contains("interrupt-controller"), "{dts}");
    }

    #[test]
    fn includes_initrd_bounds_when_given() {
        let gic = GicLayout {
            distributor_base: layout::GIC_DISTRIBUTOR_BASE,
            distributor_size: 0x10000,
            redistributor_base: layout::GIC_REDISTRIBUTOR_BASE,
            redistributor_size: 0x20000,
        };
        let dtb = build_device_tree(
            &gic,
            256 * 1024 * 1024,
            "console=ttyAMA0",
            Some((0x4800_0000, 0x4900_0000)),
        );
        let dts = decompile(&dtb);
        assert!(dts.contains("linux,initrd-start"), "{dts}");
        assert!(dts.contains("linux,initrd-end"), "{dts}");
    }
}
