# Design note 0248: `ocivmm` — pet microVMs from OCI images, dogfooded as the CI VM harness

Status: implemented
Scope: `bin/ocivmm/` (new binary), `crates/oci-vmm/` (new crate: this
workspace's own KVM/virtio-pci monitor), `ci/setup-host.sh`/
`ci/run-in-vm.sh`/`ci/vm-ci.sh`/`ci/vm-prepare.sh` (rewritten/adapted),
`ci/vm.sh` (deleted), `.github/workflows/ci.yml` (`vm-test` job),
packaging lists.

## What `ocivmm` is

The VM-shaped sibling of `ocibox`: where `ocibox` makes pet
*containers*, `ocivmm run ubuntu:26.04` makes a pet *virtual machine*
— it resolves/pulls the OCI image (the exact same
`oci_registry::resolve_or_pull` + `oci_layer::apply` extraction
`ocibox create` uses), runs the distro's own package manager on it *as
a container* (`oci_runtime_core::launch`, the same machinery `ocibox
enter` uses) to install its own kernel, dracut, and systemd, then
builds an **ext4 disk image** from the result (`mkfs.ext4 -d`, one of
this project's own already-allowed shell-outs) — everything installed
or written inside the guest persists in that image across runs, the
pet model.

Every `run` after that boots the guest's *own* kernel straight into
its *own* systemd as PID 1: no command lands on an autologin root
console (serial-getty, autoconfigured by the provisioning step); a
command runs as a generated oneshot systemd unit whose exit status is
written back into the image and read back once the guest has powered
off.

## No libkrun, no dlopen, nothing dynamically loaded: `oci-vmm`, this workspace's own VMM

The VMM is `crates/oci-vmm` — this project's *own* KVM + virtio-pci
monitor, statically linked into `ocivmm` like any other Rust
dependency. It is not a wrapper around an external hypervisor library
at all: it owns `KVM_CREATE_VM`/vCPU setup/register configuration, PCI
configuration-space emulation (legacy conf1 ports, no ACPI), MSI-X,
virtio-blk, virtio-net, and a 16550 serial console directly, ported
from **Firecracker** (Apache-2.0; the same rust-vmm lineage this
workspace already trusts via `seccompiler`) and trimmed of everything
a pet VM never needs — no snapshots, no metrics, no ACPI, no rate
limiters, no jailer, no MMIO transport.

Why not just reuse an existing microVM monitor (libkrun, crosvm,
cloud-hypervisor)? Every one of them either dynamically loads a
kernel/companion library at run time (libkrun + libkrunfw) or is
itself a large, separately-versioned dependency this workspace would
have no control over — this project's own stated goal is outperforming
`llama.cpp`-style tooling by *minimizing* what's linked and *owning*
the code that matters, the same reasoning behind `ocirun` (not
shelling out to `runc`) and `ociman` (not linking `libpod`). `oci-vmm`
carries that same principle into the one remaining place this
workspace still needed someone else's binary.

### virtio-pci, not virtio-MMIO — the one substantive design choice

Firecracker's own transport is virtio-**MMIO**; `oci-vmm` uses
virtio-**PCI** instead, because that is the one transport every stock
distro kernel actually has built in. RHEL-family kernels ship *no*
virtio-MMIO support at all (checked directly against the real CentOS
Stream 10 kernel-core package's own `.config`), which is exactly why a
generic MMIO-only microVM monitor cannot boot a real CentOS/RHEL
kernel without a custom kernel build — the same wall libkrun-based
prototyping of this milestone hit before this design was chosen.
Enumeration is legacy conf1 port I/O (`0xcf8`/`0xcfc`) plus an MP
table; no ACPI, no firmware.

### Booting a distro's own compressed kernel

`linux-loader`'s own `bzimage` loader does not decompress a bzImage —
checked directly in its source (`"Seek the compressed vmlinux.bin and
read it to memory"`) — it loads the still-compressed payload at the
address a real BIOS bootloader would, then expects the kernel's own
embedded decompression stub to run first, which a direct-64-bit-entry
boot (jumping straight past that stub) never executes at all. So
`ocivmm` unwraps the guest's own bzImage into a plain ELF vmlinux
itself (`bin/ocivmm/src/disk.rs::extract_vmlinux`, decompressing from
the earliest gzip/zstd magic — the same technique the kernel's own
`extract-vmlinux` script uses) and hands that to `linux-loader`'s ELF
loader, using its `kernel_load` entry point directly (ELF loading sets
this from the plain ELF header regardless of whether a PVH note is
also present, which `oci-vmm` doesn't use).

### Multi-vCPU boot: `KVM_RUN` returning `EAGAIN` is normal, not an error

Each vCPU runs its own `KVM_RUN` loop on its own thread; the *boot*
vCPU starts runnable immediately, but the others don't run anything
until the boot CPU sends them an INIT-SIPI-SIPI sequence over the
(emulated) LAPIC during the guest kernel's own SMP bring-up. Verified
directly on real KVM hardware: calling `KVM_RUN` on one of those
not-yet-started vCPUs returns `EAGAIN`, and if that's treated as a
fatal error (as a naive `match` on `kvm_ioctls`'s result might), the
whole VMM process exits the instant the guest kernel prints `smp:
Bringing up secondary CPUs ...` — after everything *else* about early
boot (memory detection, MP table parsing, IRQ setup, RCU init) had
already worked correctly, which is what made this one easy to miss
until real hardware exercised it. Firecracker's own vcpu loop
(`vstate/vcpu.rs::handle_kvm_exit`) treats `EAGAIN` exactly like
`EINTR` — a normal, retryable condition, not a failure — and so does
`oci-vmm`'s.

## The pet VM is a disk image, not a shared directory — `ocivmm cp`

`oci-vmm` has no filesystem-sharing device at all (no virtio-fs): a
pet VM's root filesystem is a plain **ext4 disk image**
(`rootfs.img`), and volumes/live directory sharing (`--volume` in
earlier designs studied from krunvm/libkrun) has no equivalent here.
In its place: **`ocivmm cp`** copies a file or directory into or out
of a *stopped* pet VM by loop-mounting its image
(`oci_mount::loop_device`, already used by `oci-erofs`), docker-`cp`-
style. `run` itself uses the same loop-mount machinery internally: to
copy the guest's own kernel + initramfs out to a small host-side cache
before boot (the host VMM loads them directly; `linux-loader` needs
plain files, not a mounted filesystem) and, for command runs, to read
the exit-status file back once the guest has powered off.

## Networking

`oci-vmm`'s virtio-net device is backed by an already-connected
**passt** unix-stream socket (framed with passt's own documented
4-byte-big-endian-length-prefix wire protocol — studied directly from
libkrun's own `unixstream.rs` backend, itself just passt's own
`--socket` protocol, not libkrun-specific); systemd-networkd does DHCP
against it, and `--publish` becomes passt's own `-t host:guest`
forwards. `ocivmm` spawns passt `--foreground` as its own child
(self-daemonization was observed to unlink the socket file on some
builds) and polls for the socket file before connecting.

passt's own socket path lives directly under `/tmp` (sticky-bit
1777), not inside the pet VM's own `vm_dir` (root-owned, since the
whole harness runs under sudo) — found the hard way, via strace, on
real CI hardware: passt always creates its own, unmapped user
namespace (`unshare(CLONE_NEWUSER)`) before it will `bind()` its
socket, and per `user_namespaces(7)`, a process inside a *new,
unmapped* user namespace has its filesystem permission checks against
anything outside that namespace degrade to the overflow UID —
regardless of the process's real UID beforehand, which is exactly why
`--runas 0:0` and an AppArmor `userns,` exception both turned out to
be dead ends (see git history for the full trail). passt itself
doesn't check its own `bind()` return value at all, so the actual
symptom was never an obvious error: it prints "socket bound"
unconditionally and dies moments later when `listen()` on the
never-actually-bound fd predictably fails too. `/tmp`'s own
permissions already satisfy every UID, sidestepping the question
entirely.

## Provisioning: the distro's own kernel + systemd, installed by the distro

`create` extracts the image to a scratch directory (named `rootfs`,
matching the `oci_runtime_core::Bundle` convention its own
provisioning container needs), runs the distro's package manager in it
as a container (host network kept — package managers need the
registry mirrors; no seccomp filter — this is `ocivmm`'s own trusted
script, not untrusted guest code):

* CentOS: `dnf -y install kernel dracut kmod systemd systemd-resolved
  dbus-broker ...`
* Ubuntu: `apt-get install -y systemd systemd-sysv systemd-resolved
  dbus kmod dracut` then `linux-image-virtual linux-image-extra-
  virtual` (dracut first, so it satisfies the `initramfs-tools |
  linux-initramfs-tool` alternative and owns the kernel's initramfs
  hooks)

then a dracut initramfs able to mount the virtio-blk root device
directly (`root=/dev/vda` — no virtiofs, `oci-vmm` has no such device),
a systemd-networkd DHCP config, systemd-resolved enabled and handed
`/etc/resolv.conf` (the provisioning *container* itself used the
host's own resolv.conf, copied in verbatim before provisioning ran),
and a root-autologin override for `serial-getty@ttyS0` (systemd's
getty-generator spawns it automatically for `console=ttyS0`, the
VMM's only console). Images with no `dnf`/`apt-get` (alpine,
distroless) are a clear, upfront `create` error.

## Dogfooding: the CI VM harness is now `ocivmm`

The `vm-test` matrix (CentOS Stream 10, Ubuntu 26.04, x86_64) no
longer downloads cloud images or boots qemu at all; `ci/vm.sh` (the
qemu/cloud-localds/ssh driver) is deleted outright. The new flow:

1. `ci/setup-host.sh` — /dev/kvm perms, passt, e2fsprogs (`mkfs.ext4`
   + loop-mounting a pet VM's own image). No qemu/OVMF/cloud-image
   tooling, no shared libraries to stage, no kernel toolchain. The VMM
   is KVM-only: no TCG fallback exists, a missing /dev/kvm is a clear
   hard error.
2. `ci/run-in-vm.sh` — `sudo ocivmm create --name oci-ci-<base> -i
   <image>` (idempotent — reuses an already-created pet VM), pushes a
   fresh checkout via `ocivmm cp` (a filtered copy, `.git`/`target`
   excluded, staged into `/root/oci-tools` inside the image; overlays
   onto whatever the pet VM already has there, so `target/` from a
   previous run survives untouched), `sudo ocivmm run oci-ci-<base>
   bash /root/oci-tools/ci/vm-ci.sh`, then `ocivmm cp` pulls
   `artifacts/` (and, CentOS-only, `artifacts-rpm/`) back out. Root
   because a real chown (package managers installing files, `ocivmm
   cp` writing into the loop-mounted image) needs a real root process.
   Guest images: `quay.io/centos/centos:stream10` (docker.io's
   library/centos stops at 8) and `docker.io/library/ubuntu:26.04`.
3. `ci/vm-ci.sh` — the oneshot unit's command: distro packages once
   per pet VM (stamped with `vm-prepare.sh`'s own hash), full
   workspace build/test, artifacts staged at `~/oci-tools/artifacts`
   for the host to pull out via `ocivmm cp` (no shared mount at all —
   the disk image is the only channel), RPM verify-install on the
   CentOS cell exactly as before.

What replaced what, concretely: ~700MB cloud image download → 30-60MB
OCI pull (and zero on a warm cache); UEFI boot + cloud-init minutes →
a ~1-2s direct-kernel boot into systemd; ssh + tar push/pull + port
forward → `ocivmm cp` (loop-mount, VM stopped) for source/artifacts;
qcow2 cache disk + in-guest mkfs/mount → the pet VM's own ext4 image
*is* the cache (distro kernel, packages, rustup, cargo home, target
dir all persist), packed as a root-created `vm-state.tar` because the
actions/cache step runs as the runner user and the image file itself
is root-owned.

Fidelity notes: the guests run the real distro kernels (so the
dm-verity/fs-verity/erofs/loop/overlayfs coverage the cloud images
provided is intact — no custom kernel config anywhere) and real
systemd + D-Bus (so the systemd cgroup driver's environment matches
too; its `systemd --user`-gated tests still skip, same as before).
The one added guest package vs. the cloud images is `apparmor` on
Ubuntu: the cloud image preinstalled `apparmor_parser`, the OCI base
image doesn't, and without it `vm-prepare.sh`'s existing
userns-profile workaround for
`kernel.apparmor_restrict_unprivileged_userns` would silently skip.

## Honest deltas and risks accepted

* **No live directory sharing.** `--volume`/virtiofs-style live
  sharing (studied from krunvm/libkrun during earlier design
  iterations of this milestone) has no equivalent: `oci-vmm` has no
  filesystem-sharing virtio device. `ocivmm cp` (explicit,
  VM-stopped, docker-`cp`-shaped) is the replacement — a real
  capability cut, not hidden, and CI's own source-push/artifact-pull
  is built entirely on it.
* The guest workload runs as root (previously: cloud-init `ci` user +
  passwordless sudo). The suite's privileged tests run directly
  instead of via their sudo re-exec paths — same coverage, one fewer
  indirection.
* `oci-vmm`'s own crate does not compile on macOS (it pulls in
  Linux-only rust-vmm crates — `kvm-ioctls`, `vmm-sys-util`'s epoll
  wrappers — even though its own code is `cfg`-gated away); `ocivmm`
  the *binary* still `cargo check`s cleanly there by target-gating its
  own dependency on `oci-vmm` to `cfg(target_os = "linux")`, matching
  this workspace's existing precedent (`oci-runtime-core`/
  `seccompiler` already don't build on macOS either).
* aarch64 stays on the `native-test` job (GitHub aarch64 runners have
  no /dev/kvm, and `oci-vmm` is x86_64-only — its PCI/MSI-X/boot code
  was written and verified against x86_64 specifically).
