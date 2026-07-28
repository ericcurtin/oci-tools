# Design note 0249: `ocivmm` on macOS/aarch64 — a Hypervisor.framework backend

Status: proposed (phases 2-3 landed: raw HVF bindings, VM/vCPU/GIC/
PL011/FDT foundation, and a real, unmodified stock arm64 kernel boots
end to end to its own console banner and panics cleanly for lack of a
root filesystem; phase 4 partially landed -- virtio-mmio transport and
a virtio-blk device exist and are wired up correctly as far as the
device tree and MMIO trap-and-emulate go, but the guest's own driver
can't yet actually bind to the device, a real open issue, not yet
root-caused; phase 7 landed in reduced, compile-only form -- GitHub-
hosted macOS runners don't support Hypervisor.framework at all, on any
macOS version, a real infrastructure limitation, not something this
project can fix (see phase 7's own entry); phases 5-6 are future work;
all tracked below)
Scope: `crates/oci-vmm/` (new `hvf`/`aarch64` backend, alongside the
existing KVM/x86_64 one), `bin/ocivmm/` (macOS provisioning path,
codesigning), `.github/workflows/ci.yml` (a compile-only `hvf-build`
job landed; real VM-boot CI coverage remains future work, blocked on
both phase 4 and the lack of any self-hosted macOS runner).

## Why this exists

`docs/design/0248` shipped `ocivmm`'s KVM/virtio-pci monitor and
explicitly scoped aarch64 out to the existing `native-test` job
("`oci-vmm` is x86_64-only — its PCI/MSI-X/boot code was written and
verified against x86_64 specifically"), and `oci-vmm` doesn't even
compile on macOS at all (`#![cfg(all(target_os = "linux", target_arch
= "x86_64"))]` in `lib.rs`, plus direct dependencies on Linux-only
rust-vmm crates: `kvm-ioctls`, `vmm-sys-util`'s epoll wrappers). This
note designs the second backend that closes both gaps at once: a real
Apple Silicon host running the pet-VM model (CentOS Stream 10, Ubuntu
26.04) the same way the existing x86_64/KVM cells do.

This is *not* a small CI change. It is a second hypervisor backend
(`kvm-ioctls` has no macOS equivalent at all — Hypervisor.framework is
a completely different API surface), a second CPU architecture (arm64
has no CPUID/MSR/GDT/mptable/i8042 — none of `arch/{gdt,mptable,regs,
interrupts}.rs` or `legacy/i8042.rs` apply), and, unlike the x86_64
path, a guest-provisioning story that can't reuse `oci_runtime_core`'s
Linux container launch at all, because there is no Linux container
host on macOS. Each of those is scoped as its own phase below and
landed independently; only the last phase touches CI.

## Architecture decisions

### Hypervisor API: raw Hypervisor.framework bindings, no existing crate

There is no `kvm-ioctls`-equivalent, maintained, widely-used Rust
crate for Hypervisor.framework at the fidelity this project needs
(register-level vCPU control, `hv_gic_*`). `oci-vmm` writes its own
thin `extern "C"` bindings against the system framework directly
(`libc`-style, the same trust level this workspace already gives
`libc`/`rustix` for raw syscalls), scoped to exactly the calls needed:
`hv_vm_create`/`hv_vm_map`, `hv_vcpu_create`/`hv_vcpu_run`/
`hv_vcpu_get_reg`/`hv_vcpu_set_reg`/`hv_vcpu_get_sys_reg`/
`hv_vcpu_set_sys_reg`, and the `hv_gic_*` virtual GIC calls (macOS 12+;
this project already requires a recent macOS given Apple Silicon).

### Entitlement + codesigning

Hypervisor.framework on Apple Silicon requires the calling process to
hold the `com.apple.security.hypervisor` entitlement *and* be
codesigned — an unsigned or plain ad-hoc-signed (`codesign -s -`)
binary without the entitlements plist gets `HV_DENIED` from
`hv_vm_create` even as root. `ocivmm`'s macOS build gains a post-build
step (`ci/codesign-ocivmm.sh`, `cargo:rerun-if-changed`-driven, mirrors
this project's existing preference for scripted, auditable build steps
over opaque proc-macro/build.rs magic) that ad-hoc-signs the freshly
built binary with a minimal entitlements plist
(`packaging/macos/ocivmm.entitlements`). No paid Apple Developer
account is required for local development or CI (ad-hoc signing is
sufficient for the hypervisor entitlement specifically); a real
Developer ID signature is a distribution concern, out of scope here.

### virtio transport: virtio-mmio + device tree, not PCI

The x86_64 backend chose virtio-**pci** specifically because RHEL-family
x86_64 kernels ship no virtio-mmio support at all (0248). That
constraint doesn't hold on arm64: real CentOS Stream 10 and Ubuntu
26.04 aarch64 kernel packages both build `CONFIG_VIRTIO_MMIO`, since
virtio-mmio is the standard transport for arm64 "virt"-machine-style
boots generally (it's what QEMU's `virt` machine and other real,
shipping HVF/aarch64 backends both use). Building an ECAM-based
PCIe config-space window plus arm64 MSI/GICv3-ITS routing purely to
reuse `pci/{bus,configuration,msix}.rs` would be substantially more new
emulation code for no boot-compatibility benefit, so the aarch64
backend skips PCI entirely: static MMIO register windows for each
device, discovered by the guest kernel from `virtio,mmio` device-tree
nodes generated at boot time (address, size, IRQ per device — no
device is hot-added, so a static, sized-at-boot device tree is
sufficient, unlike PCI's own runtime enumeration). The existing
`virtio::device`/`virtio::block`/`virtio::net` device *logic*
(queues, descriptor handling, virtio-blk/-net protocol) is
transport-agnostic already and is reused as-is behind this new
transport, mirroring how `virtio/transport/pci/` sits behind the same
devices today.

### Guest boot: `Image` + flattened device tree, not bzImage/mptable

arm64 Linux's own boot protocol is entirely different from x86_64's:
a plain, already-uncompressed `Image` file (no bzImage self-extracting
stub to unwrap, unlike `boot.rs`'s `extract_vmlinux` — see phase 3
below on one real packaging wrinkle this still ran into), entered with
the device tree blob's physical address in `x0` and every other
register zeroed, no GDT/page-table/MP-table setup at all. The device
tree itself (built with a small hand-rolled FDT writer, `hvf::fdt` —
matching this project's "own the code that matters" precedent rather
than pulling in a generic `fdt`-crate dependency, and cross-checked
against a real external parser, `dtc`, not just self-consistency)
carries `/memory`, `/cpus` (a single, always-on boot CPU only — see
below on why there's no `/psci` node yet), `/chosen` (bootargs + initrd
location), `/timer` (the architected timer — required for Linux's
clockevent source to exist at all, see phase 3), a `/pl011` UART node
plus its own `apb-pclk` fixed-clock dependency, the GICv3
`interrupt-controller` node, and (phase 4) one `/virtio_mmio` node per
device.

No `/psci` node, and no `enable-method` on the one CPU node, for the
same single-boot-CPU reason: a `psci`-enable-method secondary CPU
requires this backend to answer real PSCI HVC calls (`CPU_ON`,
`PSCI_VERSION`, ...), which nothing here implements yet; omitting
`/psci` entirely means `psci_dt_init` finds no such node and skips
PSCI setup outright, so the (single) boot CPU never attempts one. A
future phase adding real SMP would need both a `/psci` node and an
HVC-call responder in `hvf::vcpu`'s own exit loop — deferred
deliberately, not an oversight.

### Interrupts: `hv_gic`, not a hand-rolled GIC

Unlike x86_64 (`arch/interrupts.rs`'s own LAPIC/IOAPIC-adjacent GSI
routing on top of KVM's in-kernel irqchip), Hypervisor.framework
provides the GIC itself as a virtual device (`hv_gic_create` and
friends, macOS 12+) — there is no in-kernel-vs-userspace irqchip choice
to make the way there was for KVM's PIC/IOAPIC; the aarch64 backend
always uses the framework's own GIC, configured for the vCPU count at
VM-creation time.

### Console: a hand-written PL011, not the 16550 and not `vm-superio`

`legacy/serial.rs`'s 16550 UART emulation (`vm-superio`) is
x86_64/PC-legacy-shaped; arm64 "virt"-style boots use an ARM PL011
instead. `vm-superio` (already a dependency, on the KVM/x86_64 side)
turned out to have **no PL011 model at all** — only a 16550, an i8042,
and a PL031 RTC — an assumption this note originally got wrong before
phase 3 actually went looking; `hvf::pl011` is hand-written directly
against the real PL011 register layout instead, cross-checked against
QEMU's own `hw/char/pl011.c` (in particular the AMBA PrimeCell ID
bytes Linux's `amba` bus driver reads back during probe). It has no
MMIO-mapped host memory of its own at all: every access is a real
stage-2 Data Abort this crate decodes and dispatches (`hvf::mmio`),
since AArch64 has no port I/O for the architecture to trap directly
the way x86_64/KVM's device model does.

### Networking: `vmnet.framework`, not passt

passt is Linux-specific (network namespaces, `/proc/net`), so it has
no role on macOS at all. The aarch64 backend's virtio-net device gets a
new backend against `vmnet.framework` (Apple's own host-network-bridge
API, the same one Virtualization.framework-based tools use) instead of
passt's unix-stream socket framing — a real, separate piece of new
code (phase 5), not a portability shim over the existing passt
backend.

### Provisioning: a bootstrap VM, not a bootstrap container

The one part of `docs/design/0248`'s design that has *no* aarch64/macOS
analogue at all: `ocivmm create` provisions a pet VM's kernel + systemd
by running the distro's own package manager **as a container**
(`oci_runtime_core::launch`) — real Linux namespaces/cgroups, which
plainly don't exist on macOS. Once phases 2-5 land (a working boot +
virtio-mmio + vmnet path), provisioning is redesigned to boot a small,
generic stock image *as a VM* under the same new backend instead of a
container, run `dnf`/`apt` inside that guest over its own network
connectivity, then extract the resulting ext4 image out via the same
loop-mount-and-`ocivmm cp` machinery already used for artifact pull on
Linux (`oci_mount::loop_device`, which is itself already Linux-only —
gaining a userspace-ext4 alternative for the macOS host side of `cp` is
a dependency of this phase too, not yet designed in detail here).

## Phases and status

1. **This design doc.** Done.
2. **Raw HVF bindings + minimal VM/vCPU (landed).** A new
   `crates/oci-vmm/src/hvf/sys.rs` (`extern "C"` declarations against
   `Hypervisor.framework`) and `hvf/vm.rs`/`hvf/vcpu.rs` (safe
   wrappers: create a VM, map a small guest memory region, create one
   vCPU, set `PC`/`CPSR`, run it, read back the exit reason), gated
   `cfg(all(target_os = "macos", target_arch = "aarch64"))` and kept
   fully independent of the existing KVM/x86_64 modules (no shared
   trait yet — that unification is deferred to phase 4, once there are
   two real, working backends to unify rather than one working backend
   and a guess at the right abstraction).    Proven with a smoke test that
   loads a handful of raw AArch64 instructions, runs the vCPU, and
   asserts on the resulting `HV_EXIT_REASON`/register state — run for
   real on this project's own Apple Silicon development hardware
   (an M4 Max), not just compiled. `ci/codesign-ocivmm.sh` plus
   `packaging/macos/ocivmm.entitlements` land in this phase too, since
   the smoke test itself needs the entitlement to get anything but
   `HV_DENIED`.

   Three facts confirmed directly on that hardware, all relevant to
   later phases, none obvious from the framework's own header
   comments alone:
   * **The host page size is 16 KiB, not 4 KiB.** `hv_vm_map` returns
     `HV_BAD_ARGUMENT` for a guest address/size that isn't a multiple
     of the real host page size (`sysctl hw.pagesize` on Apple
     Silicon: 16384) — matters directly for phase 4's guest memory
     layout and phase 3's `Image`/dtb placement.
   * **`hvc`'s reported exit PC is the return address, not the
     `hvc` instruction's own address.** Architecturally correct
     (`hvc` is a synchronous call, like `svc`, not a fault) but easy
     to get backwards when writing the first exit handler.
   * **The entitlement failure mode is a clean `HV_DENIED`, not a
     hang or a kill — *given* the binary is signed at all.** Every
     `cargo build`/`cargo test` output on Apple Silicon is already
     automatically ad-hoc-signed by the linker (a platform
     requirement for arm64 execution, unrelated to this project's own
     entitlement); `hv_vm_create` on that ordinary,
     entitlement-less-but-signed binary returns `HV_DENIED` cleanly.
     A binary with its linker-added signature stripped entirely
     (`codesign --remove-signature`, not a state a normal build ever
     produces) instead gets SIGKILLed by the kernel before
     `hv_vm_create` can return anything — confirmed directly, and
     worth recording so a future "why did the smoke test just
     disappear with no output" doesn't get mis-attributed to this
     module's own code.
3. **arm64 boot + GIC + console (landed).** `hvf::boot` (`Image`
   header parsing), `hvf::mmio` (Data Abort trap-and-emulate — the
   mechanism PL011, and phase 4's virtio-mmio, both need), `hvf::pl011`
   (the console device), `hvf::gic` (`hv_gic` bindings + GICv3
   creation), `hvf::fdt` (the device tree writer), `hvf::layout` (this
   backend's memory map, deliberately matching QEMU's own `virt`
   machine addresses so `qemu-system-aarch64 -M virt -accel hvf` —
   itself Hypervisor.framework-accelerated, available on this same
   Apple Silicon hardware — could be used as an independent reference
   implementation while developing this phase), and `hvf::sysreg_trap`
   (trapped `MSR`/`MRS` handling, see below). Milestone genuinely
   reached, not just approximated: `crates/oci-vmm/tests/hvf_boot.rs`
   boots a real, unmodified Alpine `linux-virt` 6.6.142 aarch64 kernel
   (a real stock distro kernel package, not a custom build) through
   this backend end to end, to its own `Linux version ...` banner and
   a clean `Kernel panic - not syncing: VFS: Unable to mount root fs`
   (no initrd/root= given) — the same "no rootfs yet" milestone the
   x86_64 port used along the way, per 0248's own history. The same
   test also passes unmodified against Ubuntu's real `linux-image`
   kernel and CentOS Stream 10's real `kernel-core` — CentOS Stream 10
   and Ubuntu being this project's actual two target distros (0248).
   `hvf::boot::load_image` now does the unwrapping itself directly
   from a real, unmodified package's own `vmlinuz` (no manual
   pre-extraction step needed, for this test or for
   `ci/fetch-aarch64-kernel.sh`): real distro packaging turned out
   inconsistent in more than one way, confirmed directly against the
   actual current packages rather than assumed —
   * CentOS Stream 10's `kernel-core` RPM ships an **EFI
     zboot**-wrapped (`CONFIG_EFI_ZBOOT`) image, `gzip`-compressed.
   * Ubuntu 24.04/noble's `linux-generic` shipped a **bare `gzip`
     stream**, no zboot wrapping at all.
   * Ubuntu 26.04/resolute's own current kernel switched again: its
     `linux-image-unsigned-*` package (not `linux-image-<ver>-generic`,
     what `linux-image-generic` actually depends on — that one's own
     `vmlinuz` on `ports.ubuntu.com` is a non-zboot, seemingly
     non-functional artifact, arm64 having no real Secure Boot signing
     infrastructure the way amd64 does) ships an EFI zboot image
     again, but **`zstd`-compressed**, not `gzip`. `hvf::boot` decodes
     both compressions (`ruzstd`, the same pure-Rust decoder
     `oci-layer` already uses for zstd-compressed OCI layers).

   `ci/fetch-aarch64-kernel.sh` always resolves whichever kernel
   version/package is *currently* the real, latest one for each distro
   (CentOS Stream 10's rolling `kernel-core`; Ubuntu's own
   `linux-image-generic` meta-package's current dependency, substituted
   to its `-unsigned` sibling) rather than a hardcoded version, so it
   keeps working release after release with no manual bumps — and, not
   incidentally, is exactly what already caught the `zstd` switch above
   during this project's own development, rather than that drifting
   silently unnoticed.

   Facts confirmed directly while getting a real kernel to boot, none
   obvious ahead of time:
   * **A `/timer` device tree node (`compatible = "arm,armv8-timer"`)
     is required for Linux's `arch_timer` driver to register a
     clockevent device *at all*.** Without one, the kernel boots
     (memory, GIC, SMP bring-up all succeed) but hangs indefinitely at
     a fixed instruction address once something depends on jiffies
     actually advancing — confirmed by forcibly cancelling the vCPU
     mid-hang (`hv_vcpus_exit`, explicitly documented as safe to call
     from another thread, unlike every other `hv_vcpu_*` call) and
     re-checking its `PC` across several rounds: identical every time,
     not slow progress. The standard PPI numbers (secure/non-secure
     physical, virtual, hypervisor) match both QEMU's own generated
     tree and `hv_gic_intid_t`'s own values.
   * **The device tree needs a root `interrupt-parent` pointing at the
     GIC.** `dtc` itself warns about this (`Missing interrupt-parent`)
     if omitted; without it device interrupt properties can't resolve
     at all.
   * **The PL011 device tree node needs a real `clocks`/`apb-pclk`
     fixed-clock dependency.** `amba-pl011`'s driver probe calls
     `devm_clk_get` and fails outright without one (found by
     cross-checking QEMU's own generated tree, which provides exactly
     this node for the same reason).
   * **Hypervisor.framework traps some debug-adjacent system registers
     Linux writes unconditionally during early boot, that
     `hv_vcpu_set_trap_debug_reg_accesses(false)` does not cover.**
     Specifically `OSLAR_EL1`/`OSDLR_EL1` (the "OS Lock"/"OS Double
     Lock" registers, written by every stock kernel's own debug-monitor
     bring-up) still exit with `EC == 0x18` ("Trapped MSR, MRS, or
     System instruction execution") regardless. `hvf::sysreg_trap`
     handles this generically (decode `op0`/`op1`/`CRn`/`CRm`/`op2`,
     accept writes, answer reads with `0`) rather than special-casing
     just the two registers hit so far, since this project emulates
     none of the ARM debug architecture and has no reason to expect
     it's seen the last such register.
   * **A guest-run watchdog needs a real cancellation mechanism, not
     just a deadline checked between `vcpu.run()` calls.** Once the
     kernel panics (`panic=-1`, no reboot configured), it settles into
     a final idle/halt loop that does not reliably produce any further
     exit at all — a naive "check `Instant::now()` before each
     `run()`" loop hung indefinitely at exactly that point during this
     phase's own development, even though the guest had already
     printed everything this test was looking for. The fix was a real
     watchdog thread calling `hv_vcpus_exit` after a deadline, exactly
     the mechanism the framework documents that call for.
4. **virtio-mmio transport + device-tree nodes (partially landed;
   blocked on an open issue).** `hvf::virtio_mmio` implements the
   virtio-mmio register file (a completely different layout from
   virtio-pci's common configuration structure, though the same
   underlying protocol: feature negotiation, the device status state
   machine, `QueueNotify`/interrupt handling) directly against
   `crate::virtio::queue::Queue` and `crate::virtio::block::{disk,
   request}` -- the transport- and hypervisor-agnostic parts of the
   *existing* virtio-blk implementation the KVM/x86_64 backend's own
   `virtio::block::device::VirtioBlock` already builds on, now made to
   compile on macOS too (only `vm-memory` needed moving to a common
   dependency; `crate::mem::GuestMemoryMmap`'s own type alias needed
   the same treatment). Deliberately *not* shared: `virtio::device::
   VirtioDevice`/`virtio::transport::VirtioInterrupt`, which are
   shaped around `event_manager`'s epoll-driven `MutEventSubscriber`
   and `vmm_sys_util::eventfd::EventFd` -- this backend has no
   equivalent event loop at all (every `hvf` device, PL011 included,
   is dispatched synchronously out of the vCPU exit loop); `hvf::
   virtio_mmio::MmioVirtioDevice` is this module's own, much smaller
   device trait for that model instead. `hvf::virtio_blk::
   VirtioBlkMmio` is the block device itself, and `hv_gic_set_spi`
   (newly bound) delivers its interrupts.

   Real, confirmed progress, not just compiling: booted against a
   real, unmodified Ubuntu 24.04 `linux-generic` aarch64 kernel (the
   actual real signed `.deb` package, gunzipped down to a plain
   `Image` -- simpler than Alpine's own EFI-zboot-wrapped `vmlinuz`,
   see `hvf::boot`'s own module docs), the guest's `virtio_mmio`
   platform driver *does* find and match the generated device tree
   node by name (`a000000.virtio_mmio`) -- confirming the node's
   `compatible`/`reg`/`interrupts` properties, and `hvf::mmio`'s Data
   Abort trap-and-emulate mechanism underneath it, are all correct.

   What isn't working yet, and is a genuinely open, unresolved
   question rather than a placeholder: the guest's own
   `devm_request_mem_region()` call -- a pure Linux resource-tree
   operation, before any actual MMIO access to the device at all --
   fails with `-EBUSY`, so `hvf::virtio_mmio`'s own register file is
   never actually reached by a guest read/write yet. The *identical*
   symptom (`OF: amba_device_add() failed (-16)`, the AMBA-bus
   equivalent of the same underlying `request_resource` call) has
   silently been present since phase 3's own PL011 node -- it just
   never blocked that phase's milestone, since `earlycon=` bypasses
   the AMBA bus entirely. Investigated at length (ruled out: the
   device tree's own content -- an independent parser, `dtc`, accepts
   it; a missing `dma-coherent` property, matching Firecracker's own
   aarch64 FDT convention, added but confirmed by retest not to be the
   cause; `earlycon=`'s presence on the kernel command line itself;
   and, cross-checked directly against another real, shipping
   HVF-based VMM's aarch64 backend --
   `hv_gic_create`-before-`hv_vcpu_create` ordering, the boot CPSR
   value, `hv_vcpu_set_trap_debug_*` usage, which system registers get
   initialized, and the "leave MMIO regions unmapped" memory-mapping
   strategy are all identical between the two backends, so none of
   those are the cause either) but not yet root-caused. One further,
   decisive data point since: **the bug is confirmed 100% generic**,
   not address- or device-specific at all -- a throwaway third node (a
   PL031 RTC at a brand-new address, `0x09010000`, never previously
   used) hits the identical `-16` the very first time it's tried, and
   `ID_AA64MMFR0_EL1`/`SCTLR_EL1`/`TCR_EL1`/`MAIR_EL1` were all checked
   and are ordinary values, not a corrupt vCPU reset state. The leading
   remaining hypothesis: something makes the *entire* non-RAM address
   space look already-claimed to Linux's `iomem_resource` tree, rather
   than anything about any individual device's own registration --
   confirming that needs real kernel-side introspection (a debug
   kernel build, `kgdb`, `ftrace`) not available without a working
   root filesystem, itself blocked by this very bug. See
   `crates/oci-vmm/tests/hvf_virtio_blk.rs`'s own test (currently
   `#[ignore]`d, with the full investigation write-up in its doc
   comment) for the complete, honest accounting. Milestone once
   resolved: boots to the guest's own systemd against a real
   virtio-blk root disk.
5. **`vmnet.framework` networking (not started).** New virtio-net
   backend parallel to the existing passt one. Milestone: DHCP and
   `--publish` parity with the Linux/passt path.
6. **Bootstrap-VM provisioning (not started).** Redesign `ocivmm
   create`'s provisioning step for macOS: boot a minimal stock image
   under this same backend and run the distro package manager inside
   it instead of a container; work out the macOS side of loop-mount-
   equivalent image access for `ocivmm cp`.
7. **CI wiring (landed, much smaller in scope than originally
   planned — a hard platform wall, not a choice).** The original plan
   here was two new `vm-test` matrix cells (`centos-stream10`/
   `ubuntu-26.04` × `aarch64`), added only once phases 2-6 boot a real
   pet VM end to end locally. That's blocked on phase 4 with no clear
   end date, but a real boot-to-console-and-panic CI job (the phase 3
   milestone, already real and hardware-verified) was tried anyway
   and hit a harder wall than phase 4 itself: **GitHub-hosted macOS
   runners don't support Hypervisor.framework at all, on any macOS
   version** — confirmed directly against
   [`actions/runner-images#13505`](https://github.com/actions/runner-images/issues/13505)
   (closed "not planned"), whose own repro script (`sysctl -n
   kern.hv_support`) fails identically on `macos-14`, `macos-15`, and
   even `macos-26` hosted runners. `hv_vm_create()` can never succeed
   there, regardless of anything in this project's own code — not a
   bug to fix, a real infrastructure limitation. (This is also,
   retroactively, *why* the reference implementations researched
   earlier both avoid this: sailor only boots real HVF VMs on a
   self-hosted `mac/arm64` runner, never GitHub-hosted.)

   Landed instead: `hvf-build`, a `macos-15` job that builds
   `oci-vmm` (including its hvf hardware tests, compile-only) and runs
   its non-hardware unit tests on every push/PR — real compiler/lint
   coverage for this backend that didn't exist in CI at all before,
   without pretending it exercises the real, hardware-verified boot
   path. `ci/fetch-aarch64-kernel.sh` and `ci/hvf-boot-test.sh` (real,
   working, tested against actual current CentOS Stream 10/Ubuntu
   26.04 kernel packages) exist for **local** verification on real
   Apple Silicon hardware — the only place this backend's real
   capability can currently be exercised at all. `hv_gic_create`
   itself requires macOS 15.0+ (`API_AVAILABLE(macos(15.0))`), hence
   `macos-15` rather than `macos-14` even for the compile-only job.
   The original two full-pet-VM `vm-test` cells, and any real VM-boot
   CI coverage at all, remain future work — blocked on both phase 4
   and on provisioning a self-hosted macOS runner, should this
   project ever want one.

## Honest deltas and risks accepted

* Phases 5-7 are unimplemented, and phase 4 is blocked on a real, open
  bug (see phase 4's own entry above for the full accounting): the
  virtio-mmio transport and virtio-blk device are implemented and the
  guest driver does find and match the device tree node, but can't yet
  actually bind to it (`devm_request_mem_region` fails with `-EBUSY`
  for a not-yet-root-caused reason -- notably, the *identical* symptom
  has quietly affected the PL011 AMBA node since phase 3, without
  blocking that phase's own milestone). Phases 2-3 remain real,
  working, and hardware-verified: a real stock arm64 kernel boots to
  console and panics cleanly with no root filesystem, exactly as
  designed — but there is still no working disk or network
  (virtio-blk/virtio-net/vmnet), no SMP, and no way to actually
  provision a pet VM's own rootfs on macOS at all yet.
* `hvf::pl011` never raises a guest interrupt (`UARTRIS`/`UARTMIS`
  always read `0`) — sufficient for the kernel's own polling-based
  console writer (`pl011_console_write`, used by every serial console
  driver Linux ships, precisely because `printk` must also work with
  interrupts disabled), but real interactive input beyond this
  project's existing `ocivmm cp` disk-image channel would need actual
  `hv_gic_set_spi`-driven interrupt delivery, not implemented.
* `hvf::sysreg_trap`'s "answer every unrecognized `MRS` with `0`"
  policy is a real risk for any future kernel/config that reads a
  trapped register expecting a *specific*, non-zero value as part of a
  poll loop (a wrong answer there could hang exactly the way the
  missing `/timer` node did) — no such case has been hit yet, but nothing
  currently detects one before it manifests as a hang.
* The provisioning redesign (phase 6) has no existing precedent in
  this codebase to port from (unlike phases 2-5, which have KVM/x86_64
  analogues to study) — it is new design, and may turn
  out to need more machinery than sketched here (e.g. a
  userspace-ext4-image writer if loop-mount-equivalent access on macOS
  proves impractical).
* No in-kernel-vs-userspace irqchip choice exists on this backend the
  way it did for KVM — `hv_gic` is the only option Hypervisor.framework
  offers, so there's no equivalent design decision to make there, just
  a fact to note.
* CI cost: two more `macos-14`-class runner-minutes cells:
  GitHub-hosted macOS runners are already the most expensive/slowest
  class this project's CI uses, and phase 7 should re-confirm that
  tradeoff (matrix `timeout-minutes`, whether both distros need to run
  on every push vs. a lighter schedule) once real run times are known.
</content>
