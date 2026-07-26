//! oci-tools' own microVM monitor — the engine under `ocivmm`.
//!
//! A deliberately minimal KVM VMM whose one job is booting *stock*
//! distro kernels (the exact `vmlinuz` + initramfs a pet VM's own
//! `dnf`/`apt` installed) with nothing loaded at run time and nothing
//! custom inside the guest:
//!
//! * **virtio over PCI** (modern, MSI-X), because that is the one
//!   transport every stock distro kernel has built in — RHEL-family
//!   kernels ship no virtio-mmio at all (checked against the real
//!   CentOS Stream 10 kernel packages), which is exactly why generic
//!   microVM monitors' MMIO-only device model cannot boot them.
//!   Enumeration is legacy conf1 port I/O (0xcf8/0xcfc) plus an MP
//!   table: no ACPI, no firmware.
//! * **virtio-blk** for the pet VM's ext4 disk image, **virtio-net**
//!   backed by an already-connected passt socket, and a 16550 serial
//!   console on stdio.
//! * x86_64 Linux 64-bit direct boot: the distro bzImage is loaded
//!   as-is (`linux-loader`), the initrd is placed *below 4 GiB* (the
//!   boot protocol's `ramdisk_image` header field is 32 bits — a
//!   top-of-RAM placement silently breaks >4 GiB guests), and the
//!   guest exits through the classic `reboot=k` i8042 pulse.
//!
//! The core is ported from Firecracker (Apache-2.0, the same lineage
//! this workspace already trusts via `seccompiler`), trimmed of
//! everything a pet VM never needs: no snapshots, no metrics, no
//! ACPI, no rate limiters, no jailer, no MMIO transport. Built on the
//! rust-vmm ecosystem crates from crates.io — every byte statically
//! linked, nothing resolved at run time.
//!
//! A second, independent backend (`hvf`) targets macOS/aarch64 via
//! Hypervisor.framework -- see `docs/design/0249-ocivmm-macos-
//! aarch64.md` for the phased plan. It shares no code with the
//! modules above (different hypervisor API, different architecture
//! entirely) and is gated to its own target instead of this crate's
//! historical whole-crate `cfg`.

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub mod arch;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub mod boot;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub mod builder;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub mod hvf;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub mod legacy;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub mod mem;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub mod pci;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub mod virtio;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub mod vstate;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub use builder::{VmmConfig, run};
