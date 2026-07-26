# Design note 0249: `ocivmm` on macOS/aarch64 — a Hypervisor.framework backend

Status: proposed (phase 2 landed: raw HVF bindings + a standalone
VM/vCPU smoke test; phases 3-7 are future work, tracked below)
Scope: `crates/oci-vmm/` (new `hvf`/`aarch64` backend, alongside the
existing KVM/x86_64 one), `bin/ocivmm/` (macOS provisioning path,
codesigning), `.github/workflows/ci.yml` (two new matrix cells, added
only once the backend actually boots a pet VM end to end).

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
boots generally (it's what QEMU's `virt` machine and, notably,
libkrun's own HVF/aarch64 backend both use). Building an ECAM-based
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
stub to unwrap, unlike `boot.rs`'s `extract_vmlinux`), entered with the
device tree blob's physical address in `x0` and every other register
zeroed, no GDT/page-table/MP-table setup at all. The device tree itself
(built at boot time with a small hand-rolled FDT writer, matching this
project's "own the code that matters" precedent rather than pulling in
a generic `fdt`-crate dependency) carries `/memory`, `/cpus`, `/chosen`
(bootargs + initrd location), a `/pl011` UART node, a `/psci` node, and
one `/virtio_mmio` node per device.

### Interrupts: `hv_gic`, not a hand-rolled GIC

Unlike x86_64 (`arch/interrupts.rs`'s own LAPIC/IOAPIC-adjacent GSI
routing on top of KVM's in-kernel irqchip), Hypervisor.framework
provides the GIC itself as a virtual device (`hv_gic_create` and
friends, macOS 12+) — there is no in-kernel-vs-userspace irqchip choice
to make the way there was for KVM's PIC/IOAPIC; the aarch64 backend
always uses the framework's own GIC, configured for the vCPU count at
VM-creation time.

### Console: PL011, not the 16550

`legacy/serial.rs`'s 16550 UART emulation (`vm-superio`) is
x86_64/PC-legacy-shaped; arm64 "virt"-style boots use an ARM PL011,
which `vm-superio` also implements (`vm_superio::Pl011`) — the same
crate, a different one of its device models, wired to a second MMIO
window and device-tree node instead of port 0x3f8.

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
3. **arm64 boot + GIC + console (not started).** `Image`+dtb loader,
   `hv_gic` setup, PL011. Milestone: a stock CentOS/Ubuntu aarch64
   kernel reaches its own early console output and panics cleanly for
   lack of a root filesystem (the same "no rootfs yet" milestone the
   x86_64 port used along the way, per 0248's own history).
4. **virtio-mmio transport + device-tree nodes (not started).** Port
   `virtio::block`/`virtio::net` onto a new `virtio/transport/mmio/`
   sitting alongside `virtio/transport/pci/`; this is also the natural
   point to extract a small `VmBackend`/`VcpuBackend`-shaped trait the
   KVM and HVF modules both implement, now that there are two real
   implementations to abstract over. Milestone: boots to the guest's
   own systemd against a real virtio-blk root disk.
5. **`vmnet.framework` networking (not started).** New virtio-net
   backend parallel to the existing passt one. Milestone: DHCP and
   `--publish` parity with the Linux/passt path.
6. **Bootstrap-VM provisioning (not started).** Redesign `ocivmm
   create`'s provisioning step for macOS: boot a minimal stock image
   under this same backend and run the distro package manager inside
   it instead of a container; work out the macOS side of loop-mount-
   equivalent image access for `ocivmm cp`.
7. **CI wiring (not started, and deliberately last).** Two new
   `vm-test` matrix cells (`centos-stream10`/`ubuntu-26.04` ×
   `aarch64`, `runs-on: macos-14`), added only once phases 2-6 boot a
   real pet VM end to end locally — landing CI cells against a backend
   that can't yet finish a boot would just be permanently-red or
   permanently-skipped jobs, which this project's existing CI has no
   precedent for.

## Honest deltas and risks accepted

* Phases 3-7 are unimplemented as of this note. Phase 2 is a real,
  working foundation (VM/vCPU creation, one instruction executed, exit
  observed, entitlement plumbing proven) but not yet a bootable guest.
* The provisioning redesign (phase 6) has no existing precedent in
  this codebase to port from (unlike phases 2-5, which have KVM/x86_64
  and/or libkrun analogues to study) — it is new design, and may turn
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
