# Design note 0248: `ocivmm` — pet microVMs from OCI images, dogfooded as the CI VM harness

Status: implemented
Scope: `bin/ocivmm/` (new binary), workspace `Cargo.toml`/`deny.toml`
(pinned libkrun git crates), `ci/setup-host.sh`/`ci/run-in-vm.sh`/
`ci/vm-ci.sh`/`ci/vm-prepare.sh` (rewritten/adapted), `ci/vm.sh`
(deleted), `.github/workflows/ci.yml` (`vm-test` job), packaging lists.

## What `ocivmm` is

The VM-shaped sibling of `ocibox`: where `ocibox` makes pet
*containers*, `ocivmm run ubuntu:26.04` makes a pet *virtual machine*
— it resolves/pulls the OCI image (the exact same
`oci_registry::resolve_or_pull` + per-pet `oci_layer::apply`
extraction `ocibox create` uses, never the shared read-only rootfs
cache), extracts a dedicated writable rootfs under a derived name
(`ubuntu-26.04`), and boots it as a microVM. The rootfs is a plain
host directory served to the guest over virtiofs, so everything
installed inside persists across runs — the pet model — and `rm` is
just deleting the directory.

Three deliberate steps beyond the krunvm design this started from
(studied directly in `~/git/libkrun`):

1. **Nothing is dynamically loaded.** The VMM is libkrun's own
   `krun-vmm`/`krun-devices`/`krun-polly` crates, statically linked as
   ordinary cargo git dependencies pinned to an exact revision — no
   `libkrun.so`, no libkrunfw, no dlopen, no run-time linkage.
2. The pet VM runs the **distro's own kernel**, and
3. the **distro's own systemd as PID 1** — a pet CentOS Stream 10 VM
   *is* CentOS Stream 10: its kernel, its init, its units, and `dnf
   upgrade` inside it upgrades all of them (every boot re-detects the
   newest installed kernel; no kernel state is cached in `vm.json`).

Commands: `run` (create-on-first-use + boot; no command → autologin
root console on hvc0, with a command → a generated oneshot unit that
powers off and hands its exit status back; `-v HOST:GUEST` virtiofs
volumes via an ocivmm-managed `/etc/fstab` block, `-p HOST:GUEST`
port forwards, `-e`, `--cpus`, `--mem`), `create`, `list`/`ls`,
`rm [--all]` — the `ocibox` family shape, deliberately.

## The VMM: libkrun's crates, statically linked

`bin/ocivmm/src/microvm.rs` is the (much thinner) equivalent of
libkrun's C-API layer: `krun_start_enter`'s configuration assembly and
`build_microvm` + event-loop tail, ported line for line from
`src/libkrun/src/lib.rs` at the pinned revision, minus everything a
pet VM never uses — TEE, GPU, vsock/TSI, the embedded init, and the
whole libkrunfw bundled-kernel path (see below for why it isn't
needed even for bootstrap). The crates are consumed like any other
Rust dependency (`deny.toml` allows the one git source and the pin
bounds it), so the whole workspace — including the CI guests that
build it inside an `ocivmm` VM — needs nothing installed beyond a C
toolchain. Everything is Linux-gated target dependencies; the crate
still `cargo check`s on other hosts, where `run` reports a clear
"Linux only" error.

## Provisioning: the distro's own kernel + systemd, installed by the distro, as a container

A fresh OCI rootfs has no kernel and no init system. Rather than boot
a borrowed kernel to install one (the earlier design's libkrunfw
bootstrap), `create` runs the provisioning script **as a container on
the rootfs** — this project's own `oci_runtime_core::launch`/`Bundle`/
`validate` lifecycle, exactly `ocibox enter`'s launch path, with the
network namespace kept on the host (the package manager needs the
mirrors). Inside, with the distro's own package manager:

* CentOS: `dnf -y install kernel dracut kmod systemd dbus-broker ...`
* Ubuntu: `apt-get install -y systemd systemd-sysv dbus kmod dracut`
  then `linux-image-virtual linux-image-extra-virtual` (dracut first,
  so it satisfies `initramfs-tools | linux-initramfs-tool` and owns
  the kernel's initramfs hooks)

then a dracut initramfs able to mount the virtiofs root
(`dracut --no-hostonly --add virtiofs`; the `root=virtiofs:/dev/root`
cmdline syntax checked directly in dracut's own
`modules.d/*virtiofs/parse-virtiofs.sh`), a systemd-networkd DHCP
config, networkd + wait-online enablement by plain symlink, and a
root-autologin override for `serial-getty@hvc0` (systemd's
getty-generator spawns it automatically for `console=hvc0`). Images
with no dnf/apt (alpine, distroless) are a clear, upfront `create`
error: without a distro able to install its own kernel and systemd
there is nothing to boot.

Every `run` boots the guest's own `/boot/vmlinuz-<newest>` through
the external-kernel loader: on x86_64 it scans a distro bzImage for
its compression magic (gzip/zstd/bzip2) and ELF-loads the embedded
vmlinux — checked directly in `~/git/libkrun`'s
`src/vmm/src/builder.rs::load_external_kernel`; `ocivmm` sniffs the
same magics to pick the format. No `init=` on the cmdline, so
kernel/dracut default to `/sbin/init` — the distro's systemd.

Command runs are a generated `ocivmm-run.service`: oneshot (no start
timeout — cargo builds are long), console on `/dev/hvc0`
(`serial-getty@hvc0` masked for the run), ordered after
`network-online.target`, `SuccessAction`/`FailureAction=poweroff`,
and an `ExecStopPost` that writes `$EXIT_STATUS` to a file on the
shared rootfs. Since the boot turns its process into the VMM (the
event loop `_exit`s when the guest powers off), `run` boots through a
re-exec'd `ocivmm __boot` child (the same self-re-exec technique
`ocicri`'s `__launch` uses) and exits with the guest command's own
status read back from that file.

## Networking

Distro kernels have no TSI (that's a libkrunfw kernel patch, and
there is no libkrunfw here at all), so every VM gets a **passt-backed
virtio-net** device (`krun-devices`' unixstream backend; passt
started `--one-off` per boot, its daemonization doubling as the
socket-readiness barrier) and systemd-networkd does DHCP against
passt; `--publish` becomes passt's own `-t host:guest` TCP forwards.
`ocivmm` also writes `/etc/resolv.conf` (host nameservers minus
loopback — the host's systemd-resolved `127.0.0.53` stub would loop
back into the guest — else public resolvers), `/etc/hosts`, and
`/etc/hostname`, only when absent/unusable so a pet's own
customizations survive.

## Dogfooding: the CI VM harness is now `ocivmm`

The `vm-test` matrix (CentOS Stream 10, Ubuntu 26.04, x86_64) no
longer downloads cloud images or boots qemu at all; `ci/vm.sh` (the
qemu/cloud-localds/ssh driver) is deleted outright. The new flow:

1. `ci/setup-host.sh` — /dev/kvm perms and passt, nothing else (no
   qemu/OVMF/cloud-image-utils, no shared libraries to stage, no
   kernel toolchain). The VMM is KVM-only: no TCG fallback exists
   anymore, a missing /dev/kvm is a clear hard error.
2. `ci/run-in-vm.sh` — `sudo ocivmm run --name oci-ci-<base> -v
   "$repo:/src" <image> bash /src/ci/vm-ci.sh`. Root because the
   in-process virtiofs server impersonates guest uids via per-thread
   `setresuid` (checked in libkrun's `passthrough.rs` at the pinned
   revision), which needs CAP_SETUID — without it `dnf`/`apt`/`rpm`
   inside the guest (and the provisioning container) cannot chown
   what they install. Guest images: `quay.io/centos/centos:stream10`
   (docker.io's library/centos stops at 8) and
   `docker.io/library/ubuntu:26.04`.
3. `ci/vm-ci.sh` — the oneshot unit's command: distro packages once
   per pet VM (stamped with `vm-prepare.sh`'s own hash), source
   synced from /src with the same exclusions the old ssh-push used,
   rustup + pinned toolchain, full workspace build/test, artifacts
   written straight to `/src/artifacts` through virtiofs (no pull
   step), RPM verify-install on the CentOS cell exactly as before.

What replaced what, concretely: ~700MB cloud image download → 30-60MB
OCI pull (and zero on a warm cache); UEFI boot + cloud-init minutes →
a ~1-2s direct-kernel boot into systemd; ssh + tar push/pull + port
forward → one virtiofs mount; qcow2 cache disk + in-guest mkfs/mount
→ the pet VM rootfs itself *is* the cache (distro kernel, packages,
rustup, cargo home, target dir all persist), packed as a root-created
`vm-state.tar` because the actions/cache step runs as the runner user
and couldn't read a multi-uid rootfs tree directly.

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

* The guest workload runs as root (previously: cloud-init `ci` user +
  passwordless sudo). The suite's privileged tests run directly
  instead of via their sudo re-exec paths — same coverage, one fewer
  indirection.
* Rootless (non-root) `ocivmm` runs are attempted but degraded: the
  provisioning container falls back to a single-uid userns
  (`into_rootless`), where package managers may fail to chown what
  they install — warned about at run time, documented in the module
  docs; multi-uid userns self-re-exec (the buildah-unshare trick) is
  a possible later increment if a rootless need appears.
* The libkrun crates bring a bounded set of duplicate transitive
  dependency versions and one unmaintained indirect dependency
  (bincode, via imago's qcow2 support `ocivmm` never exercises) —
  each carries a targeted, reasoned entry in `deny.toml`, all bounded
  by the revision pin.
* aarch64 stays on the `native-test` job (GitHub aarch64 runners have
  no /dev/kvm — unchanged from the qemu harness's reasoning).
