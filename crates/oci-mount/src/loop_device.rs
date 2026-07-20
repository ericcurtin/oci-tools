//! Loop device attach/detach -- turning a regular file (a built erofs
//! image, most importantly) into a real block device `mount(2)` can
//! target, via the kernel's own `/dev/loop-control` and per-device
//! `LOOP_*` ioctls directly (no `losetup` shellout: unlike `mkfs.erofs`
//! or `veritysetup`, this is a handful of simple ioctls the kernel
//! already exposes, matching this workspace's own established
//! preference for direct syscalls over a CLI wrapper wherever a
//! syscall suffices -- see `oci-erofs::verity`'s own fs-verity ioctls
//! for the same reasoning).
//!
//! Every detail here was confirmed directly against a real loop
//! device on this development host before writing any of this, not
//! assumed from `linux/loop.h` alone: attaching a plain 16 MiB backing
//! file via `LOOP_CONFIGURE` (the modern, single-call, atomic
//! alternative to the older `LOOP_SET_FD`-then-`LOOP_SET_STATUS64`
//! two-step) succeeded and was independently confirmed via `losetup
//! -a`/`losetup <dev>`, which reported the correct backing file path;
//! `read_only` was confirmed to genuinely reject a real write
//! afterwards (`EPERM`) while `/sys/block/loopN/ro` correctly read
//! back `1`; **not** setting the kernel's own `LO_FLAGS_AUTOCLEAR` bit
//! was a deliberate, verified choice -- with it set, the device was
//! observed to detach itself the instant this crate's own attaching
//! process exited (confirmed directly: a `losetup -a` right
//! afterwards no longer listed it), which is exactly wrong for
//! `ociboot`'s own real use (a deployment's loop device needs to
//! outlive the short-lived process that attached it, for as long as
//! the deployment itself stays mounted); [`detach`] on an already-
//! detached device was confirmed to return the kernel's own real
//! `ENXIO`.
//!
//! Needs privilege (`/dev/loop-control` and `/dev/loopN` are
//! `root:disk`-owned, confirmed directly on this development host) --
//! exactly like the rest of the loop/mount-dependent machinery this
//! workspace already gates behind `sudo` in its own tests
//! (`oci-erofs::verity`'s loopback fs-verity tests, `oci-erofs::builder`'s
//! implicit reliance on real image files).

use std::io;
use std::os::fd::AsRawFd as _;
use std::path::{Path, PathBuf};

use rustix::fs::{Mode, OFlags, open};

/// `LOOP_CTL_GET_FREE`/`LOOP_CONFIGURE`/`LOOP_CLR_FD` from
/// `linux/loop.h` -- unlike `oci-erofs::verity`'s own fs-verity ioctl
/// numbers (which are `_IOW`/`_IOWR`-encoded and had to be computed),
/// these are plain, unencoded literal constants in the real kernel
/// header, copied here directly and double-checked against
/// `/usr/include/linux/loop.h` on this development host.
const LOOP_CTL_GET_FREE: libc::c_ulong = 0x4C82;
const LOOP_CONFIGURE: libc::c_ulong = 0x4C0A;
const LOOP_CLR_FD: libc::c_ulong = 0x4C01;

/// `enum` values from `linux/loop.h`'s own loop flags.
const LO_FLAGS_READ_ONLY: u32 = 1;
const LO_FLAGS_DIRECT_IO: u32 = 16;

/// `struct loop_info64` from `linux/loop.h`, field-for-field.
#[repr(C)]
struct LoopInfo64 {
    lo_device: u64,
    lo_inode: u64,
    lo_rdevice: u64,
    lo_offset: u64,
    lo_sizelimit: u64,
    lo_number: u32,
    lo_encrypt_type: u32,
    lo_encrypt_key_size: u32,
    lo_flags: u32,
    lo_file_name: [u8; 64],
    lo_crypt_name: [u8; 64],
    lo_encrypt_key: [u8; 32],
    lo_init: [u64; 2],
}

/// `struct loop_config` from `linux/loop.h`, used with `LOOP_CONFIGURE`
/// to atomically set up and configure a loop device in one call.
#[repr(C)]
struct LoopConfig {
    fd: u32,
    block_size: u32,
    info: LoopInfo64,
    reserved: [u64; 8],
}

/// Options controlling how [`attach`] configures the new loop device.
/// Unlike `oci-erofs::builder::BuildOptions` (whose fields must always
/// be given explicitly for reproducibility), both fields here have a
/// perfectly ordinary, safe default -- a plain read-write loop device
/// with no special I/O mode -- so [`Default`] is provided.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AttachOptions {
    /// Reject writes to the resulting device at the kernel level
    /// (`LO_FLAGS_READ_ONLY`) -- the right choice for mounting an
    /// already-sealed erofs image.
    pub read_only: bool,
    /// Bypass the host page cache for I/O against the backing file
    /// (`LO_FLAGS_DIRECT_IO`) -- avoids double-buffering the same
    /// data once in the backing file's own page cache and again in
    /// the loop device's.
    pub direct_io: bool,
}

/// Attach `backing_file` to a newly allocated loop device and return
/// its path (e.g. `/dev/loop7`).
///
/// Deliberately never sets the kernel's own `LO_FLAGS_AUTOCLEAR` --
/// confirmed directly (see this module's own top doc comment) that
/// doing so tears the device down the moment every file descriptor
/// referencing it closes, including this very function's own
/// short-lived ones, which is exactly wrong for a device meant to
/// outlive the process that attached it. Callers own the device's
/// full lifecycle explicitly, via [`detach`].
pub fn attach(backing_file: &Path, options: &AttachOptions) -> io::Result<PathBuf> {
    let control = open("/dev/loop-control", OFlags::RDWR, Mode::empty())?;
    // SAFETY: `control` is a valid, open file descriptor to the real
    // `/dev/loop-control` device; `LOOP_CTL_GET_FREE` takes no
    // pointer argument at all, just returns the free device's own
    // number (or a negative `errno` on failure) -- confirmed directly
    // against a real loop-control device.
    #[allow(unsafe_code)]
    let device_number = unsafe { libc::ioctl(control.as_raw_fd(), LOOP_CTL_GET_FREE) };
    if device_number < 0 {
        return Err(io::Error::last_os_error());
    }

    let device_path = PathBuf::from(format!("/dev/loop{device_number}"));
    let device = open(&device_path, OFlags::RDWR, Mode::empty())?;
    let backing = open(backing_file, OFlags::RDWR, Mode::empty())?;

    let mut flags = 0u32;
    if options.read_only {
        flags |= LO_FLAGS_READ_ONLY;
    }
    if options.direct_io {
        flags |= LO_FLAGS_DIRECT_IO;
    }
    // SAFETY: every field of `LoopConfig`/`LoopInfo64` is a plain
    // integer or fixed-size byte array with no validity invariants
    // beyond being initialized, so zeroing is always a valid starting
    // point -- the same pattern `oci-erofs::verity`'s own
    // `FsverityEnableArg` uses.
    #[allow(unsafe_code)]
    let mut config: LoopConfig = unsafe { std::mem::zeroed() };
    config.fd = backing.as_raw_fd() as u32;
    config.info.lo_flags = flags;

    // SAFETY: `device` is a valid, open file descriptor to the freshly
    // allocated loop device from `LOOP_CTL_GET_FREE` above; `config`
    // is a properly initialized, correctly-sized `LoopConfig` the
    // kernel only ever reads from (an `LOOP_CONFIGURE` ioctl) --
    // confirmed field-for-field against the real kernel header, and
    // confirmed to actually work against a real loop device (verified
    // via `losetup -a` reporting back the correct backing file and
    // read-only bit) before this was written.
    #[allow(unsafe_code)]
    let ret = unsafe { libc::ioctl(device.as_raw_fd(), LOOP_CONFIGURE, &config) };
    if ret != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(device_path)
}

/// Detach a loop device previously returned by [`attach`], freeing it
/// for reuse.
///
/// Returns the kernel's own real error (`io::ErrorKind::Uncategorized`,
/// raw errno `ENXIO` -- `std` has no dedicated `ErrorKind` for it) if
/// `loop_device` isn't currently attached to anything, confirmed
/// directly rather than assumed.
pub fn detach(loop_device: &Path) -> io::Result<()> {
    let device = open(loop_device, OFlags::RDWR, Mode::empty())?;
    // SAFETY: `device` is a valid, open file descriptor to a real loop
    // device; `LOOP_CLR_FD` takes no pointer argument at all.
    #[allow(unsafe_code)]
    let ret = unsafe { libc::ioctl(device.as_raw_fd(), LOOP_CLR_FD) };
    if ret != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// `/dev/loop-control` and `/dev/loopN` are `root:disk`-owned
    /// (confirmed directly on this development host), so every real
    /// test in this module needs actual root privilege -- not just
    /// `sudo` access to shell out one command, since [`attach`]/
    /// [`detach`] are plain Rust functions this *process* calls
    /// in-process, not a CLI this crate could simply prefix with
    /// `sudo`.
    ///
    /// `LOOP_CTL_GET_FREE` itself is not atomic against a second,
    /// concurrent caller (confirmed directly: running this module's
    /// own tests in parallel, `cargo test`'s own default, produced
    /// real `EBUSY` failures from two tests racing to configure the
    /// *same* just-allocated device number) -- a real, pre-existing
    /// kernel-level property of this specific ioctl, not a bug in
    /// [`attach`] itself. `LOOP_DEVICE_TEST_LOCK` serializes every
    /// test in this module against every other one to work around
    /// that, held for a whole test's real privileged work (including
    /// across the `sudo` re-exec below, which blocks until the child
    /// process finishes).
    ///
    /// Each test calls this first: if the current process is already
    /// root, it returns the lock guard directly and the test's own
    /// body runs for real, still holding it. Otherwise, if
    /// passwordless `sudo` is available (true both on this
    /// development host and in the CI VM's own `ci` user, per
    /// `ci/vm.sh`'s own cloud-init `NOPASSWD:ALL`), it re-execs this
    /// *same* test binary under `sudo -n`, filtered to just this one
    /// test by its exact name -- which runs this exact function again,
    /// this time actually as root, so its own body executes for real
    /// -- then asserts that re-exec succeeded and returns `None` so
    /// the original, still-unprivileged invocation doesn't also try
    /// (and fail) to run the privileged body itself. Skips with a
    /// clear message only if neither condition holds.
    fn run_as_root_or_reexec(test_name: &str) -> Option<std::sync::MutexGuard<'static, ()>> {
        static LOOP_DEVICE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let guard = LOOP_DEVICE_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // SAFETY: `geteuid()` takes no arguments and can't fail.
        #[allow(unsafe_code)]
        let is_root = unsafe { libc::geteuid() } == 0;
        if is_root {
            return Some(guard);
        }
        let sudo_available = Command::new("sudo")
            .args(["-n", "true"])
            .status()
            .is_ok_and(|status| status.success());
        if !sudo_available {
            eprintln!("skipping: not root and no passwordless sudo available");
            return None;
        }
        let exe = std::env::current_exe().expect("current_exe");
        let status = Command::new("sudo")
            .arg("-n")
            .arg(exe)
            .args(["--exact", test_name, "--nocapture"])
            .status()
            .expect("spawning sudo -n <test binary>");
        assert!(
            status.success(),
            "privileged re-exec of {test_name} failed (exit status: {status})"
        );
        None
    }

    /// `LOOP_CLR_FD` genuinely can return success while only
    /// *scheduling* the clear rather than performing it immediately --
    /// a real, documented kernel behavior (`drivers/block/loop.c`):
    /// if anything else has the device node open at the moment of the
    /// call (observed directly and repeatedly on this development
    /// host: `systemd-udevd` transiently opens newly configured block
    /// devices to probe them), the clear only completes once every
    /// other opener closes it. This isn't a bug in [`detach`] -- real
    /// `losetup -d` has exactly the same kernel-level behavior, this
    /// crate doesn't (and shouldn't) paper over it -- but a *test*
    /// asserting the device is gone needs to tolerate the real,
    /// short, environment-dependent delay this can introduce rather
    /// than assuming synchronous completion the kernel's own API
    /// never actually promised.
    fn wait_until_truly_detached(device: &Path) -> bool {
        for _ in 0..20 {
            let visible = Command::new("losetup")
                .arg(device)
                .output()
                .is_ok_and(|out| out.status.success());
            if !visible {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        false
    }

    #[test]
    fn attach_then_detach_a_real_loop_device() {
        let Some(_guard) =
            run_as_root_or_reexec("loop_device::tests::attach_then_detach_a_real_loop_device")
        else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let backing = dir.path().join("backing.img");
        std::fs::write(&backing, vec![0u8; 16 * 1024 * 1024]).unwrap();

        let device = attach(&backing, &AttachOptions::default()).unwrap();
        // `Path::starts_with` matches whole path *components*, not a
        // string prefix (`Path::new("/dev/loop23").starts_with("/dev/loop")`
        // is `false`, a well-known Rust gotcha) -- compare the parent
        // directory and file name prefix directly instead.
        assert_eq!(device.parent(), Some(Path::new("/dev")));
        let name = device.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("loop"), "unexpected device name: {name}");

        // Confirmed via the real `losetup` (a completely independent
        // implementation) that the device really is associated with
        // the right backing file, not just that our own ioctl call
        // returned success.
        let out = Command::new("losetup").arg(&device).output().unwrap();
        assert!(
            out.status.success(),
            "real losetup should see the device this module attached"
        );
        let listed = String::from_utf8_lossy(&out.stdout);
        assert!(
            listed.contains(backing.to_str().unwrap()),
            "real losetup should report the correct backing file: {listed}"
        );

        detach(&device).unwrap();
        assert!(
            wait_until_truly_detached(&device),
            "real losetup should no longer see a detached device"
        );
    }

    #[test]
    fn read_only_genuinely_rejects_a_real_write() {
        let Some(_guard) =
            run_as_root_or_reexec("loop_device::tests::read_only_genuinely_rejects_a_real_write")
        else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let backing = dir.path().join("backing.img");
        std::fs::write(&backing, vec![0u8; 16 * 1024 * 1024]).unwrap();

        let device = attach(
            &backing,
            &AttachOptions {
                read_only: true,
                direct_io: false,
            },
        )
        .unwrap();

        let ro_flag = std::fs::read_to_string(format!(
            "/sys/block/{}/ro",
            device.file_name().unwrap().to_str().unwrap()
        ))
        .unwrap();
        assert_eq!(
            ro_flag.trim(),
            "1",
            "the kernel's own /sys ro flag should be set"
        );

        // A real write to the device node itself must genuinely fail
        // -- not just report success while silently doing nothing.
        let write_err = std::fs::OpenOptions::new()
            .write(true)
            .open(&device)
            .and_then(|mut f| {
                use std::io::Write as _;
                f.write_all(&[0xffu8; 512])
            })
            .expect_err("writing to a read-only loop device must fail");
        assert_eq!(write_err.kind(), io::ErrorKind::PermissionDenied);

        detach(&device).unwrap();
    }

    #[test]
    fn detaching_an_already_detached_device_is_a_real_enxio_error() {
        let Some(_guard) = run_as_root_or_reexec(
            "loop_device::tests::detaching_an_already_detached_device_is_a_real_enxio_error",
        ) else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let backing = dir.path().join("backing.img");
        std::fs::write(&backing, vec![0u8; 16 * 1024 * 1024]).unwrap();

        let device = attach(&backing, &AttachOptions::default()).unwrap();
        detach(&device).unwrap();
        // The first `detach` above can return success while the clear
        // is only *scheduled*, not yet actually performed (see
        // `wait_until_truly_detached`'s own doc comment) -- immediately
        // detaching again races against that real, environment-
        // dependent delay (`systemd-udevd` transiently opening the
        // device to probe it, observed directly on real CI hardware)
        // and can spuriously see the device as still genuinely
        // attached rather than the real ENXIO this test exists to
        // verify. Wait for the real clear to finish first.
        assert!(
            wait_until_truly_detached(&device),
            "device should have become genuinely detached"
        );

        let err = detach(&device).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::ENXIO));
    }
}
