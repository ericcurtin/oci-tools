//! The pet VM's disk image: building it from an extracted+provisioned
//! rootfs directory (`mkfs.ext4 -d`, one of this project's own
//! explicitly-allowed shell-outs — see `docs/HACKING.md`'s repository
//! rules), loop-mounting it for [`crate::cmd_cp`] and boot-time kernel
//! extraction, and unwrapping the guest's own compressed bzImage into
//! a plain ELF vmlinux `oci_vmm::boot` can load directly (see that
//! module's own doc comment for why the unwrapping has to happen at
//! all).

#[cfg(target_os = "linux")]
mod imp {
    use std::path::{Path, PathBuf};

    use anyhow::Context as _;

    /// Build an ext4 image at `image_path` from the contents of `src_dir`.
    /// `size_mib` must be large enough to hold `src_dir` plus headroom for
    /// later guest writes (`dnf`/`apt` upgrades, build artifacts, ...);
    /// the file itself is sparse, so an unused allowance costs nothing on
    /// disk.
    pub fn build_ext4_image(
        src_dir: &Path,
        image_path: &Path,
        size_mib: u64,
    ) -> anyhow::Result<()> {
        let truncate = std::process::Command::new("truncate")
            .arg("-s")
            .arg(format!("{size_mib}M"))
            .arg(image_path)
            .status()
            .context("running truncate")?;
        anyhow::ensure!(truncate.success(), "truncate failed ({truncate})");

        let mkfs = std::process::Command::new("mkfs.ext4")
            .args(["-F", "-q", "-d"])
            .arg(src_dir)
            .arg(image_path)
            .status()
            .context("running mkfs.ext4 (install e2fsprogs)")?;
        anyhow::ensure!(mkfs.success(), "mkfs.ext4 failed ({mkfs})");
        Ok(())
    }

    /// Loop-mount `image_path` (ext4), run `f` with the mountpoint, then
    /// always unmount and detach the loop device — even if `f` errors.
    /// `f`'s return value must be self-contained (own everything it
    /// returns; the mountpoint is gone by the time this function returns).
    pub fn with_loop_mount<R>(
        image_path: &Path,
        read_only: bool,
        f: impl FnOnce(&Path) -> anyhow::Result<R>,
    ) -> anyhow::Result<R> {
        let loop_device = oci_mount::loop_device::attach(
            image_path,
            &oci_mount::loop_device::AttachOptions {
                read_only,
                direct_io: false,
            },
        )
        .with_context(|| format!("attaching a loop device to {}", image_path.display()))?;

        let result = (|| -> anyhow::Result<R> {
            let mountpoint = tempfile::tempdir().context("creating a mountpoint")?;
            let options =
                oci_mount::options::parse_mount_options(&[if read_only { "ro" } else { "rw" }]);
            let device_str = loop_device
                .to_str()
                .context("loop device path is not valid UTF-8")?;
            oci_mount::syscalls::mount(Some(device_str), mountpoint.path(), Some("ext4"), &options)
                .with_context(|| {
                    format!("mounting {device_str} at {}", mountpoint.path().display())
                })?;
            let result = f(mountpoint.path());
            let _ = rustix::mount::unmount(mountpoint.path(), rustix::mount::UnmountFlags::empty());
            result
        })();

        let _ = oci_mount::loop_device::detach(&loop_device);
        result
    }

    /// A guest kernel found inside the disk image, copied out to a
    /// host-side cache (`<vm_dir>/boot-cache/`) so `oci_vmm::boot` can
    /// read it directly once the loop mount above is gone.
    pub struct GuestKernel {
        /// A plain, decompressed ELF vmlinux (see the module docs).
        pub vmlinuz: PathBuf,
        /// The guest's own initramfs, if any.
        pub initramfs: Option<PathBuf>,
    }

    /// Loop-mount `image` read-only, find the newest kernel the guest's
    /// own package manager installed (the highest-versioned
    /// `/lib/modules/<kver>` with a matching vmlinuz), decompress it into
    /// `<vm_dir>/boot-cache/vmlinux`, copy its initramfs alongside, and
    /// return paths into that cache. Re-detected on every boot so a `dnf
    /// upgrade` inside the pet VM takes effect — nothing is cached across
    /// runs beyond the extracted files themselves (overwritten each time).
    pub fn find_guest_kernel(vm_dir: &Path, image: &Path) -> anyhow::Result<Option<GuestKernel>> {
        let cache = vm_dir.join("boot-cache");
        std::fs::create_dir_all(&cache).with_context(|| format!("creating {}", cache.display()))?;

        with_loop_mount(image, true, |mountpoint| {
            let modules_dir = mountpoint.join("lib/modules");
            let entries = match std::fs::read_dir(&modules_dir) {
                Ok(entries) => entries,
                Err(_) => return Ok(None),
            };
            let mut kvers: Vec<String> = entries
                .filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().into_string().ok())
                .collect();
            kvers.sort_by(|a, b| compare_versions(a, b));

            while let Some(kver) = kvers.pop() {
                // Debian/Ubuntu install the image at /boot/vmlinuz-<kver>;
                // RHEL-family kernels own it at /lib/modules/<kver>/vmlinuz
                // (kernel-install only copies it to /boot on real
                // systems, which a container-provisioned rootfs is not).
                let Some(vmlinuz) = [
                    format!("boot/vmlinuz-{kver}"),
                    format!("lib/modules/{kver}/vmlinuz"),
                ]
                .iter()
                .map(|p| mountpoint.join(p))
                .find(|p| p.is_file()) else {
                    continue;
                };

                let bytes = std::fs::read(&vmlinuz)
                    .with_context(|| format!("reading {}", vmlinuz.display()))?;
                let vmlinux = match extract_vmlinux(&bytes) {
                    Ok(v) => v,
                    Err(err) => {
                        tracing::debug!(kernel = %vmlinuz.display(), %err, "could not unwrap kernel image");
                        continue;
                    }
                };
                let cached_vmlinuz = cache.join("vmlinux");
                std::fs::write(&cached_vmlinuz, vmlinux)
                    .with_context(|| format!("writing {}", cached_vmlinuz.display()))?;

                let initramfs = [
                    format!("boot/ocivmm-initrd-{kver}.img"),
                    format!("boot/initramfs-{kver}.img"),
                    format!("boot/initrd.img-{kver}"),
                ]
                .iter()
                .map(|p| mountpoint.join(p))
                .find(|p| p.is_file())
                .map(|src| {
                    let dst = cache.join("initrd.img");
                    std::fs::copy(&src, &dst).with_context(|| {
                        format!("copying {} to {}", src.display(), dst.display())
                    })?;
                    Ok::<_, anyhow::Error>(dst)
                })
                .transpose()?;

                return Ok(Some(GuestKernel {
                    vmlinuz: cached_vmlinuz,
                    initramfs,
                }));
            }
            Ok(None)
        })
    }

    /// Order two kernel-version strings by their numeric segments
    /// (`6.12.10-300` > `6.12.9-400`), falling back to lexicographic for
    /// equal numeric prefixes — a tiny `sort -V` equivalent.
    fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
        let segments = |s: &str| -> Vec<u64> {
            s.split(|c: char| !c.is_ascii_digit())
                .filter(|seg| !seg.is_empty())
                .filter_map(|seg| seg.parse().ok())
                .collect()
        };
        segments(a).cmp(&segments(b)).then_with(|| a.cmp(b))
    }

    /// Unwrap a bzImage's inner ELF vmlinux: decompress from the earliest
    /// gzip/zstd magic (both single-stream; trailing bzImage bytes after
    /// the stream are ignored by construction) — the same technique the
    /// kernel's own `extract-vmlinux` script uses, done host-side so
    /// `oci_vmm::boot`'s ELF loader can be used directly (see its own
    /// module docs for why a plain `BzImage::load` is not enough).
    fn extract_vmlinux(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
        use std::io::Read as _;
        let find = |magic: &[u8]| bytes.windows(magic.len()).position(|w| w == magic);
        let gz = find(&[0x1f, 0x8b, 0x08]);
        let zst = find(&[0x28, 0xb5, 0x2f, 0xfd]);
        let mut elf = Vec::new();
        match (gz, zst) {
            (Some(g), z) if z.is_none_or(|z| g < z) => {
                flate2::read::GzDecoder::new(&bytes[g..])
                    .read_to_end(&mut elf)
                    .context("decompressing gzip vmlinux")?;
            }
            (_, Some(z)) => {
                ruzstd::decoding::StreamingDecoder::new(&bytes[z..])
                    .context("initializing zstd decoder")?
                    .read_to_end(&mut elf)
                    .context("decompressing zstd vmlinux")?;
            }
            _ => anyhow::bail!("no gzip/zstd stream found in the kernel image"),
        }
        anyhow::ensure!(
            elf.starts_with(&[0x7f, b'E', b'L', b'F']),
            "decompressed kernel payload is not an ELF vmlinux"
        );
        Ok(elf)
    }
    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn compare_versions_orders_numerically() {
            use std::cmp::Ordering;
            assert_eq!(
                compare_versions("6.12.10-300.el10.x86_64", "6.12.9-400.el10.x86_64"),
                Ordering::Greater
            );
            assert_eq!(
                compare_versions("6.8.0-31-generic", "6.8.0-31-generic"),
                Ordering::Equal
            );
            assert_eq!(compare_versions("5.14.0", "6.1.0"), Ordering::Less);
        }

        #[test]
        fn extract_vmlinux_unwraps_gzip_and_zstd_bzimages() {
            use std::io::Write as _;
            let fake_elf = {
                let mut v = vec![0x7f, b'E', b'L', b'F'];
                v.extend_from_slice(&[0u8; 64]);
                v
            };
            let mut gz_image = vec![0u8; 512];
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(&fake_elf).unwrap();
            gz_image.extend_from_slice(&encoder.finish().unwrap());
            assert_eq!(extract_vmlinux(&gz_image).unwrap(), fake_elf);

            let mut zst_image = vec![0u8; 512];
            zst_image.extend_from_slice(&ruzstd::encoding::compress_to_vec(
                fake_elf.as_slice(),
                ruzstd::encoding::CompressionLevel::Fastest,
            ));
            assert_eq!(extract_vmlinux(&zst_image).unwrap(), fake_elf);

            assert!(extract_vmlinux(&[0u8; 128]).is_err());
        }
    }
}

#[cfg(target_os = "linux")]
pub use imp::{build_ext4_image, find_guest_kernel, with_loop_mount};

/// Loop devices, ext4, and container provisioning are Linux-only;
/// everywhere else these report a clear "Linux only" error instead
/// of failing to compile.
#[cfg(not(target_os = "linux"))]
mod stub {
    use std::path::{Path, PathBuf};

    /// See the Linux implementation in this module's `imp` submodule.
    pub struct GuestKernel {
        /// Unused on non-Linux hosts.
        pub vmlinuz: PathBuf,
        /// Unused on non-Linux hosts.
        pub initramfs: Option<PathBuf>,
    }

    /// See the Linux implementation in this module's `imp` submodule.
    pub fn build_ext4_image(
        _src_dir: &Path,
        _image_path: &Path,
        _size_mib: u64,
    ) -> anyhow::Result<()> {
        anyhow::bail!("ocivmm can only build disk images on Linux (mkfs.ext4 + loop devices)")
    }

    /// See the Linux implementation in this module's `imp` submodule.
    pub fn with_loop_mount<R>(
        _image_path: &Path,
        _read_only: bool,
        _f: impl FnOnce(&Path) -> anyhow::Result<R>,
    ) -> anyhow::Result<R> {
        anyhow::bail!("ocivmm can only loop-mount disk images on Linux")
    }

    /// See the Linux implementation in this module's `imp` submodule.
    pub fn find_guest_kernel(_vm_dir: &Path, _image: &Path) -> anyhow::Result<Option<GuestKernel>> {
        anyhow::bail!("ocivmm can only run VMs on Linux (KVM)")
    }
}

#[cfg(not(target_os = "linux"))]
pub use stub::{build_ext4_image, find_guest_kernel, with_loop_mount};
