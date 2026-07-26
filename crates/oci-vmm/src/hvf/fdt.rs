// SPDX-License-Identifier: Apache-2.0

//! A minimal, hand-rolled Flattened Device Tree (FDT/`.dtb`) writer.
//!
//! arm64 Linux has no equivalent of x86_64's `bzImage`/boot-params
//! ABI at all (see `crate::boot`'s own module docs on that side): the
//! bootloader instead builds a device tree blob describing the
//! machine (memory, CPUs, interrupt controller, and every platform
//! device -- there is no ACPI/firmware enumeration here, matching
//! this project's existing "no ACPI, no firmware" stance from the
//! x86_64/KVM port) and passes its physical address in `x0` at kernel
//! entry.
//!
//! No `fdt`/`vm-fdt`-style crate dependency: like `arch::mptable` on
//! the x86_64 side (a hand-rolled MP table, not a generic ACPI
//! library) and `hvf::pl011` (a hand-rolled device model, not a
//! generic peripheral crate), this project owns the small amount of
//! code that matters rather than pull in a generic devicetree crate
//! for the one, fixed shape of tree this backend ever needs to emit.
//! Cross-checked against the real format (`dtc`/`fdtdump` -- both
//! already installed on this project's own development hardware) by
//! this module's own tests, not just self-consistency.

use std::collections::HashMap;

/// `FDT_MAGIC`.
const MAGIC: u32 = 0xd00d_feed;
/// The structure/format version this writer emits (`17`, the current
/// version since the specification's own v0.1 -- there is no version
/// 18 or later).
const VERSION: u32 = 17;
/// The oldest version claiming to be *back*-compatible with what this
/// writer emits.
const LAST_COMP_VERSION: u32 = 16;

const FDT_BEGIN_NODE: u32 = 0x0000_0001;
const FDT_END_NODE: u32 = 0x0000_0002;
const FDT_PROP: u32 = 0x0000_0003;
const FDT_END: u32 = 0x0000_0009;

/// Builds a single flattened device tree blob. Nodes/properties must
/// be emitted in the order they should appear (a well-formed,
/// balanced sequence of [`begin_node`](Self::begin_node)/
/// [`end_node`](Self::end_node) calls, properties only directly
/// inside a currently-open node) -- there is no tree data structure
/// here to reorder or validate ahead of time, matching a real
/// bootloader's own single-pass FDT writers.
#[derive(Debug, Default)]
pub struct FdtWriter {
    struct_block: Vec<u8>,
    strings: Vec<u8>,
    string_offsets: HashMap<String, u32>,
    open_nodes: u32,
}

impl FdtWriter {
    /// Starts a new, empty tree.
    pub fn new() -> Self {
        Self::default()
    }

    /// Opens a node named `name` (e.g. `""` for the root node, or
    /// `"memory@40000000"`).
    pub fn begin_node(&mut self, name: &str) {
        self.push_u32(FDT_BEGIN_NODE);
        self.push_cstr(name);
        self.open_nodes += 1;
    }

    /// Closes the innermost still-open node.
    pub fn end_node(&mut self) {
        assert!(self.open_nodes > 0, "end_node() with no open node");
        self.push_u32(FDT_END_NODE);
        self.open_nodes -= 1;
    }

    /// Emits a raw-bytes property (every typed `property_*` helper
    /// below is a thin encoding on top of this one).
    pub fn property(&mut self, name: &str, data: &[u8]) {
        let nameoff = self.intern(name);
        self.push_u32(FDT_PROP);
        self.push_u32(u32::try_from(data.len()).expect("property data fits in u32"));
        self.push_u32(nameoff);
        self.struct_block.extend_from_slice(data);
        self.pad_struct_block();
    }

    /// A single big-endian 32-bit cell property (e.g. `#address-cells
    /// = <2>`).
    pub fn property_u32(&mut self, name: &str, value: u32) {
        self.property(name, &value.to_be_bytes());
    }

    /// A property encoded as a list of big-endian 32-bit cells (e.g.
    /// `interrupts = <GIC_SPI 1 IRQ_TYPE_LEVEL_HIGH>`, or a `reg`
    /// property under `#address-cells`/`#size-cells` `= <2>`, where
    /// each 64-bit address/size is two cells).
    pub fn property_cells(&mut self, name: &str, cells: &[u32]) {
        let mut data = Vec::with_capacity(cells.len() * 4);
        for cell in cells {
            data.extend_from_slice(&cell.to_be_bytes());
        }
        self.property(name, &data);
    }

    /// A single NUL-terminated string property (e.g. `model =
    /// "..."`).
    pub fn property_string(&mut self, name: &str, value: &str) {
        let mut data = Vec::with_capacity(value.len() + 1);
        data.extend_from_slice(value.as_bytes());
        data.push(0);
        self.property(name, &data);
    }

    /// A property encoded as a concatenation of NUL-terminated
    /// strings (e.g. `compatible = "arm,pl011", "arm,primecell"`).
    pub fn property_strings(&mut self, name: &str, values: &[&str]) {
        let mut data = Vec::new();
        for value in values {
            data.extend_from_slice(value.as_bytes());
            data.push(0);
        }
        self.property(name, &data);
    }

    /// A boolean/flag property with no value (e.g.
    /// `interrupt-controller;`).
    pub fn property_empty(&mut self, name: &str) {
        self.property(name, &[]);
    }

    fn intern(&mut self, name: &str) -> u32 {
        if let Some(&offset) = self.string_offsets.get(name) {
            return offset;
        }
        let offset = u32::try_from(self.strings.len()).expect("strings block fits in u32");
        self.strings.extend_from_slice(name.as_bytes());
        self.strings.push(0);
        self.string_offsets.insert(name.to_string(), offset);
        offset
    }

    fn push_u32(&mut self, value: u32) {
        self.struct_block.extend_from_slice(&value.to_be_bytes());
    }

    fn push_cstr(&mut self, s: &str) {
        self.struct_block.extend_from_slice(s.as_bytes());
        self.struct_block.push(0);
        self.pad_struct_block();
    }

    fn pad_struct_block(&mut self) {
        while !self.struct_block.len().is_multiple_of(4) {
            self.struct_block.push(0);
        }
    }

    /// Finishes the tree and serializes it into a complete `.dtb`
    /// image. `boot_cpuid_phys` is the physical (`MPIDR_EL1`
    /// affinity-derived) ID of the boot CPU.
    ///
    /// # Panics
    /// If `begin_node`/`end_node` calls were left unbalanced.
    pub fn finish(mut self, boot_cpuid_phys: u32) -> Vec<u8> {
        assert_eq!(
            self.open_nodes, 0,
            "unbalanced begin_node/end_node calls: {} node(s) still open at finish()",
            self.open_nodes
        );
        self.push_u32(FDT_END);

        // `fdt_header` is ten big-endian u32 fields (40 bytes,
        // already a multiple of 8); the memory reservation block
        // (one 16-byte all-zero terminating entry -- this backend
        // never reserves any additional guest memory region) follows
        // immediately, and the structure block right after that --
        // both already naturally aligned, no extra padding to
        // compute.
        const HEADER_SIZE: u32 = 40;
        const RESERVE_MAP_SIZE: u32 = 16;

        let off_mem_rsvmap = HEADER_SIZE;
        let off_dt_struct = off_mem_rsvmap + RESERVE_MAP_SIZE;
        let size_dt_struct =
            u32::try_from(self.struct_block.len()).expect("struct block fits in u32");
        let off_dt_strings = off_dt_struct + size_dt_struct;
        let size_dt_strings = u32::try_from(self.strings.len()).expect("strings block fits in u32");
        let total_size = off_dt_strings + size_dt_strings;

        let mut out = Vec::with_capacity(total_size as usize);
        out.extend_from_slice(&MAGIC.to_be_bytes());
        out.extend_from_slice(&total_size.to_be_bytes());
        out.extend_from_slice(&off_dt_struct.to_be_bytes());
        out.extend_from_slice(&off_dt_strings.to_be_bytes());
        out.extend_from_slice(&off_mem_rsvmap.to_be_bytes());
        out.extend_from_slice(&VERSION.to_be_bytes());
        out.extend_from_slice(&LAST_COMP_VERSION.to_be_bytes());
        out.extend_from_slice(&boot_cpuid_phys.to_be_bytes());
        out.extend_from_slice(&size_dt_strings.to_be_bytes());
        out.extend_from_slice(&size_dt_struct.to_be_bytes());
        out.extend_from_slice(&0u64.to_be_bytes());
        out.extend_from_slice(&0u64.to_be_bytes());
        out.extend_from_slice(&self.struct_block);
        out.extend_from_slice(&self.strings);

        debug_assert_eq!(out.len(), total_size as usize);
        out
    }
}

#[cfg(test)]
mod tests {
    //! Cross-checked against `dtc`/`fdtdump` (the standard,
    //! upstream devicetree-compiler tools -- already installed on
    //! this project's own development hardware via Homebrew), not
    //! just this module's own self-consistency: a real, external
    //! parser must accept the blob and report back exactly the
    //! property values this test wrote in.
    //!
    //! Requires `dtc` on `PATH`; a clear failure (not a silent skip)
    //! if it's missing, matching this project's existing "no /dev/kvm
    //! is a clear hard error" precedent (docs/design/0248) rather
    //! than a test that quietly never really ran.

    use super::FdtWriter;
    use std::io::Write;
    use std::process::{Command, Stdio};

    /// Pipes `dtb` through `dtc -I dtb -O dts` and returns the
    /// decompiled source text, so assertions can check for real
    /// property values a from-scratch DT parser extracted -- not
    /// just bytes this module itself produced.
    fn decompile(dtb: &[u8]) -> String {
        let mut child = Command::new("dtc")
            .args(["-I", "dtb", "-O", "dts"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("`dtc` not found on PATH -- `brew install dtc` (or apt/dnf equivalent)");

        child
            .stdin
            .take()
            .unwrap()
            .write_all(dtb)
            .expect("write dtb to dtc's stdin");

        let output = child.wait_with_output().expect("wait for dtc");
        assert!(
            output.status.success(),
            "dtc rejected the generated dtb:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("dtc -O dts produces UTF-8 (ASCII) output")
    }

    #[test]
    fn dtc_accepts_a_minimal_tree_and_reports_back_the_same_values() {
        let mut fdt = FdtWriter::new();
        fdt.begin_node("");
        fdt.property_u32("#address-cells", 2);
        fdt.property_u32("#size-cells", 2);
        fdt.property_strings("compatible", &["linux,dummy-virt"]);

        fdt.begin_node("memory@40000000");
        fdt.property_string("device_type", "memory");
        fdt.property_cells("reg", &[0x0, 0x4000_0000, 0x0, 0x4000_0000]);
        fdt.end_node();

        fdt.begin_node("chosen");
        fdt.property_string("bootargs", "console=ttyAMA0 panic=-1");
        fdt.end_node();

        fdt.end_node(); // root

        let dtb = fdt.finish(0);
        assert_eq!(&dtb[0..4], &0xd00d_feed_u32.to_be_bytes(), "FDT_MAGIC");

        let dts = decompile(&dtb);
        assert!(dts.contains("\"linux,dummy-virt\""), "{dts}");
        assert!(dts.contains("memory@40000000"), "{dts}");
        // dtc renders a 2-cell (64-bit) reg pair as two 32-bit hex
        // groups; 0x40000000 base and size both appear.
        assert!(
            dts.contains("reg = <0x00 0x40000000 0x00 0x40000000>;"),
            "{dts}"
        );
        assert!(dts.contains("console=ttyAMA0 panic=-1"), "{dts}");
    }

    #[test]
    fn string_table_deduplicates_repeated_property_names() {
        let mut fdt = FdtWriter::new();
        fdt.begin_node("");
        fdt.begin_node("a");
        fdt.property_string("compatible", "vendor,a");
        fdt.end_node();
        fdt.begin_node("b");
        fdt.property_string("compatible", "vendor,b");
        fdt.end_node();
        fdt.end_node();

        let dtb = fdt.finish(0);
        // Two nodes both use the property name "compatible"; the
        // string table should contain exactly one copy of it.
        let needle = b"compatible\0";
        let count = dtb
            .windows(needle.len())
            .filter(|window| *window == needle)
            .count();
        assert_eq!(count, 1, "\"compatible\" should be interned exactly once");

        let dts = decompile(&dtb);
        assert!(dts.contains("vendor,a"));
        assert!(dts.contains("vendor,b"));
    }

    #[test]
    #[should_panic(expected = "unbalanced")]
    fn finish_panics_on_unbalanced_nodes() {
        let mut fdt = FdtWriter::new();
        fdt.begin_node("");
        fdt.begin_node("child");
        fdt.end_node();
        // Missing the root's own end_node().
        let _ = fdt.finish(0);
    }
}
