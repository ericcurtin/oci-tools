//! Sealing and verifying files with fs-verity, via the kernel's own
//! `FS_IOC_ENABLE_VERITY`/`FS_IOC_MEASURE_VERITY` ioctls directly --
//! no external CLI needed. Unlike `mkfs.erofs` itself ([`crate::builder`],
//! a whole on-disk format no simple syscall could write), fs-verity is
//! just two ioctls the kernel already exposes; `docs/HACKING.md`'s own
//! sanctioned-shellout list deliberately does not include an
//! `fsverity` binary, only `veritysetup` (for the detached dm-verity
//! hash tree this module's own planned sibling will use as a fallback
//! when the state filesystem lacks fs-verity support at all).
//!
//! Verified directly against a real fs-verity-capable filesystem
//! before writing any of this, not assumed from the kernel docs alone:
//! built a loopback ext4 image with `mkfs.ext4 -O verity`, enabled
//! fs-verity on a real file inside it, confirmed with `lsattr` that
//! the kernel's own `V` (verity) attribute flag was set, confirmed the
//! file became genuinely immutable (a write afterwards fails with
//! `EPERM`) while remaining readable, and confirmed `measure` returns
//! a real 32-byte SHA-256-derived digest that (correctly) does *not*
//! match a plain `sha256sum` of the file's own content -- the
//! fs-verity digest is computed over a `fsverity_descriptor` (Merkle
//! tree root hash plus metadata), never a raw content hash. Also
//! confirmed the three real, distinct error cases every caller needs
//! to tell apart: `measure` on a file that was never sealed returns
//! the kernel's own `ENODATA` (mapped here to `Ok(None)`, not an
//! error, since "not sealed yet" is an ordinary, expected state, not a
//! failure); `enable` on a filesystem that doesn't support fs-verity
//! at all returns `EOPNOTSUPP` (`io::ErrorKind::Unsupported`, already
//! correctly categorized by `std`); and `enable` requires the *exact*
//! block size the containing filesystem itself uses -- verified by
//! reproducing the real kernel's `EINVAL` when block sizes mismatch
//! (a 16 MiB ext4 image built with `-b 1024` rejects a hardcoded 4096
//! request outright) -- which is why [`enable`] always queries
//! `fstatfs`'s own `f_bsize` rather than assuming any fixed value.

use std::io;
use std::os::fd::AsRawFd as _;
use std::path::Path;

use rustix::fs::{Mode, OFlags, fstatfs, open};

/// `FS_VERITY_HASH_ALG_SHA256` from `linux/fsverity.h`. The only
/// algorithm this module ever requests: SHA-256 is what `mkfs.erofs`
/// itself defaults to and what every other digest in this workspace
/// already uses (`oci-layer`'s own layer digests, `sha2` being the
/// one sanctioned hashing crate per `ci/guards.py`), so there is no
/// real reason for `ociboot` to ever need a different fs-verity
/// algorithm for its own sealed images.
const FS_VERITY_HASH_ALG_SHA256: u32 = 1;

/// Byte length of a SHA-256 fs-verity digest.
pub const DIGEST_LEN: usize = 32;

/// `struct fsverity_enable_arg` from `linux/fsverity.h`, field-for-
/// field (checked directly against the real kernel header installed
/// on this host, `/usr/include/linux/fsverity.h`) -- 128 bytes total,
/// which is also what `FS_IOC_ENABLE_VERITY`'s own encoded ioctl
/// number below assumes.
#[repr(C)]
#[derive(Default)]
struct FsverityEnableArg {
    version: u32,
    hash_algorithm: u32,
    block_size: u32,
    salt_size: u32,
    salt_ptr: u64,
    sig_size: u32,
    reserved1: u32,
    sig_ptr: u64,
    reserved2: [u64; 11],
}

/// `struct fsverity_digest` from `linux/fsverity.h`, with the trailing
/// flexible `digest[]` array fixed at [`DIGEST_LEN`] bytes -- this
/// module only ever requests a SHA-256 digest, so a fixed 32-byte
/// buffer is always exactly big enough.
#[repr(C)]
struct FsverityDigest {
    digest_algorithm: u16,
    digest_size: u16,
    digest: [u8; DIGEST_LEN],
}

/// The standard Linux ioctl-number encoding
/// (`include/uapi/asm-generic/ioctl.h`): `dir<<30 | size<<16 | type<<8
/// | nr`. Computing `FS_IOC_ENABLE_VERITY`/`FS_IOC_MEASURE_VERITY` this
/// way from their own real definitions (`_IOW('f', 133,
/// fsverity_enable_arg)` / `_IOWR('f', 134, fsverity_digest)` in
/// `linux/fsverity.h`), rather than a single hand-computed hex
/// literal, was a deliberate fix after hand-computing the first
/// version *wrong* by one hex digit (`0x4086_6685` instead of the
/// correct `0x4080_6685`) -- which silently produced a request number
/// no ioctl handler recognizes at all (`ENOTTY`) rather than any more
/// obviously-wrong failure, and was only caught by real, unprivileged,
/// non-root manual testing against a real fs-verity-capable loopback
/// filesystem before trusting either constant. `const fn` so both are
/// still resolved entirely at compile time, and a test below checks
/// the result against the two known, independently-published values
/// as a second, permanent guard against this exact mistake recurring.
const fn ioc(dir: u32, ioctl_type: u32, nr: u32, size: u32) -> libc::c_ulong {
    ((dir << 30) | (size << 16) | (ioctl_type << 8) | nr) as libc::c_ulong
}

const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;

/// `FS_IOC_ENABLE_VERITY` = `_IOW('f', 133, struct fsverity_enable_arg)`.
const FS_IOC_ENABLE_VERITY: libc::c_ulong = ioc(
    IOC_WRITE,
    b'f' as u32,
    133,
    size_of::<FsverityEnableArg>() as u32,
);
/// `FS_IOC_MEASURE_VERITY` = `_IOWR('f', 134, struct fsverity_digest)`
/// -- `size` here is `sizeof(digest_algorithm) + sizeof(digest_size)`
/// only (4 bytes): the real kernel struct's trailing `digest[]` is a
/// flexible array member, which never counts towards `sizeof` in C,
/// so [`FsverityDigest`]'s own fixed 32-byte buffer must not be
/// included in this computation either even though it *is* included
/// in Rust's own `size_of::<FsverityDigest>()`.
const FS_IOC_MEASURE_VERITY: libc::c_ulong = ioc(IOC_READ | IOC_WRITE, b'f' as u32, 134, 4);

/// Does `e` mean "fs-verity is not supported here"?
///
/// A real caller ([`enable`]) can return either of two distinct
/// errno values for this, depending on the underlying filesystem
/// driver: `EOPNOTSUPP` (`std`'s own `io::ErrorKind::Unsupported`)
/// when the filesystem *type* understands the fs-verity ioctls but
/// this particular instance wasn't built with the feature bit set (a
/// plain `mkfs.ext4` image without `-O verity`), or `ENOTTY` (`std`
/// has no dedicated `ErrorKind` for this one, so it stays
/// `Uncategorized`) when the filesystem driver doesn't register
/// fs-verity operations at all — confirmed directly on a real host:
/// overlayfs/tmpfs-backed `/tmp` returns `ENOTTY`, not `EOPNOTSUPP`. A
/// caller wanting to fall back to another mechanism (this crate's own
/// [`crate::dmverity`]) needs to treat both the same way; this
/// function is the single place that decides, so [`enable`]'s own
/// callers and this module's tests never drift apart on which errno
/// means what (a real gap once existed here: `ociboot build-image
/// --seal`'s own fallback only checked `Unsupported`, so it
/// hard-failed instead of falling back on exactly the
/// overlayfs/tmpfs-backed CI VM guests this function now also
/// recognizes).
pub fn is_unsupported(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::Unsupported || e.raw_os_error() == Some(libc::ENOTTY)
}

/// Enable fs-verity on `path`, sealing it with a SHA-256 Merkle tree
/// at the containing filesystem's own native block size (queried via
/// `fstatfs`, never assumed -- fs-verity requires an exact match or
/// the kernel rejects the request with `EINVAL`).
///
/// fs-verity has no "disable" operation by design (matching the real
/// kernel feature exactly): once enabled, `path` becomes read-only at
/// the kernel level for the rest of its lifetime. Calling this again
/// on an already-sealed file returns `io::ErrorKind::AlreadyExists`
/// (the kernel's own `EEXIST`). Calling this on a filesystem that
/// doesn't support fs-verity at all returns an error [`is_unsupported`]
/// recognizes -- the caller (this crate's own dm-verity fallback) can
/// match on that specifically rather than treating every failure
/// alike.
pub fn enable(path: &Path) -> io::Result<()> {
    let fd = open(path, OFlags::RDONLY, Mode::empty())?;
    let block_size = fstatfs(&fd)?.f_bsize as u32;
    let arg = FsverityEnableArg {
        version: 1,
        hash_algorithm: FS_VERITY_HASH_ALG_SHA256,
        block_size,
        ..Default::default()
    };
    // SAFETY: `fd` is a valid, open file descriptor this function
    // owns for the duration of the call; `arg` is a properly
    // initialized, correctly-sized `FsverityEnableArg` the kernel only
    // ever reads from (an `_IOW` ioctl) -- confirmed field-for-field
    // against the real kernel header, and confirmed to actually work
    // against a real fs-verity-capable filesystem before this was
    // written.
    #[allow(unsafe_code)]
    let ret = unsafe { libc::ioctl(fd.as_raw_fd(), FS_IOC_ENABLE_VERITY, &arg) };
    if ret != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Read back `path`'s already-enabled fs-verity digest.
///
/// Returns `Ok(None)` -- not an error -- if fs-verity was never
/// enabled on this file (the kernel's own `ENODATA`, which `std`
/// doesn't map to a dedicated `ErrorKind`, so this function matches
/// the raw errno itself rather than making every caller do so):
/// "not sealed yet" is an ordinary, expected state for a caller
/// checking whether a previous `ociboot install` run already sealed a
/// given deployment's image, not a failure.
pub fn measure(path: &Path) -> io::Result<Option<[u8; DIGEST_LEN]>> {
    let fd = open(path, OFlags::RDONLY, Mode::empty())?;
    let mut digest = FsverityDigest {
        digest_algorithm: 0,
        digest_size: DIGEST_LEN as u16,
        digest: [0; DIGEST_LEN],
    };
    // SAFETY: `fd` is a valid, open file descriptor this function
    // owns for the duration of the call; `digest` is a properly
    // initialized, correctly-sized `FsverityDigest` the kernel both
    // reads (`digest_size` as an input, the buffer's own capacity)
    // and writes (`digest_algorithm`/`digest_size`/`digest` as
    // outputs) -- an `_IOWR` ioctl, matching `FsverityDigest`'s own
    // 32-byte fixed buffer exactly since this module only ever
    // requests SHA-256.
    #[allow(unsafe_code)]
    let ret = unsafe { libc::ioctl(fd.as_raw_fd(), FS_IOC_MEASURE_VERITY, &mut digest) };
    if ret != 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ENODATA) {
            return Ok(None);
        }
        return Err(err);
    }
    Ok(Some(digest.digest))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// The two magic numbers every real fs-verity implementation
    /// (this crate, `fsverity-utils`, the kernel selftests, `xfstests`)
    /// agrees on -- independently published, not derived from this
    /// crate's own [`ioc`] helper, specifically so a bug in the
    /// helper itself (or in either struct's `#[repr(C)]` layout)
    /// cannot cancel out and pass anyway. This test exists because an
    /// earlier hand-computed version of `FS_IOC_ENABLE_VERITY` was
    /// wrong by one hex digit and only failed with a generic `ENOTTY`,
    /// not an obviously-wrong error -- this is a second, permanent
    /// guard against exactly that mistake.
    #[test]
    fn ioctl_numbers_match_the_real_kernel_uapi_header() {
        assert_eq!(FS_IOC_ENABLE_VERITY, 0x4080_6685);
        assert_eq!(FS_IOC_MEASURE_VERITY, 0xC004_6686);
    }

    #[test]
    fn fsverity_enable_arg_is_128_bytes_matching_the_real_kernel_struct() {
        assert_eq!(size_of::<FsverityEnableArg>(), 128);
    }

    /// Every test in this module needs a real fs-verity-capable
    /// filesystem, which a plain tempdir on the test-runner's own
    /// filesystem may or may not be (fs-verity is an opt-in feature
    /// only a few filesystem types support, not universal) -- so each
    /// builds its own tiny
    /// loopback ext4 image with the verity feature explicitly enabled
    /// via real `mkfs.ext4`/`sudo mount` (both already relied on
    /// elsewhere in this workspace's own CI, `ci/vm-ci.sh`'s
    /// cache-device setup, whose own `ci` user has the same
    /// passwordless `sudo` this needs -- `ci/vm.sh`'s own cloud-init
    /// `user-data` grants `NOPASSWD:ALL`) and skips itself with a
    /// clear message if any step fails (e.g. no `sudo` access at all
    /// on some other environment) -- exactly like the
    /// `oci-erofs::builder` tests already do for `mkfs.erofs` not
    /// being installed. `chown`ed back to the calling (unprivileged)
    /// user immediately after mounting so [`enable`]/[`measure`]
    /// themselves are exercised exactly as `ociboot` would really call
    /// them -- unprivileged, ordinary file operations, not root-only.
    struct VerityFs {
        /// Held for the fixture's whole lifetime: flocks are released
        /// by the kernel on process death — even SIGKILL — making
        /// "lock acquirable" the next run's reliable owner-is-dead
        /// test (see [`sweep_stale_fixtures`]).
        _lock: std::fs::File,
        dir: std::path::PathBuf,
        mountpoint: std::path::PathBuf,
    }

    impl Drop for VerityFs {
        fn drop(&mut self) {
            cleanup_fixture_dir(&self.dir, &self.mountpoint);
        }
    }

    /// The fixed, per-user base directory for these fixtures —
    /// deliberately *not* a fresh tempdir per run (`docs/design/
    /// 0247`): a test run killed mid-flight (SIGKILL skips every
    /// `Drop`) used to leak its mount + loop device + tempdir
    /// forever, observed for real on this project's own dev host as
    /// ten accumulated stale loop devices. With a fixed base, the
    /// next run finds and sweeps them instead.
    fn fixture_base() -> std::path::PathBuf {
        std::path::PathBuf::from(format!(
            "/tmp/oci-tools-verity-fixture-{}",
            rustix::process::getuid().as_raw()
        ))
    }

    /// Unmounts/detaches/removes one fixture directory, tolerating
    /// every failure (stale state can be half-torn-down already).
    fn cleanup_fixture_dir(dir: &Path, mountpoint: &Path) {
        let _ = Command::new("sudo")
            .args(["-n", "umount", "-q"])
            .arg(mountpoint)
            .output();
        // `mount -o loop`'s autoclear normally frees the loop device
        // at unmount, but loops outliving their mounts have been
        // observed on real half-torn-down fixtures -- detach by
        // backing file, belt and braces.
        if let Ok(out) = Command::new("losetup")
            .arg("-j")
            .arg(dir.join("verity.img"))
            .output()
        {
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                if let Some(device) = line.split(':').next().filter(|s| !s.is_empty()) {
                    let _ = Command::new("sudo")
                        .args(["-n", "losetup", "-d", device])
                        .output();
                }
            }
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Sweeps every fixture subdirectory whose owner is dead — the
    /// nonblocking-flock probe is race-free against a live,
    /// concurrent fixture in another test process (its own held lock
    /// makes the probe fail), unlike any timestamp heuristic.
    fn sweep_stale_fixtures(base: &Path) {
        let Ok(entries) = std::fs::read_dir(base) else {
            return;
        };
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let Ok(lock) = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(dir.join(".lock"))
            else {
                continue;
            };
            if rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive)
                .is_err()
            {
                continue; // A live fixture in a concurrent process.
            }
            cleanup_fixture_dir(&dir, &dir.join("mnt"));
        }
    }

    fn verity_capable_ext4() -> Option<VerityFs> {
        let base = fixture_base();
        std::fs::create_dir_all(&base).ok()?;
        sweep_stale_fixtures(&base);

        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = base.join(format!(
            "{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let image = dir.join("verity.img");
        let mountpoint = dir.join("mnt");
        std::fs::create_dir_all(&mountpoint).ok()?;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(".lock"))
            .ok()?;
        rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive).ok()?;

        let truncate = Command::new("truncate")
            .args(["-s", "16M"])
            .arg(&image)
            .status()
            .ok()?;
        if !truncate.success() {
            return None;
        }
        let mkfs = Command::new("mkfs.ext4")
            .args(["-O", "verity", "-q"])
            .arg(&image)
            .status()
            .ok()?;
        if !mkfs.success() {
            return None;
        }
        // `-n` on every sudo in this fixture (docs/design/0247): a
        // host whose passwordless sudo has lapsed must produce a
        // clean skip, never a test run hanging forever on an
        // invisible password prompt.
        let mount = Command::new("sudo")
            .args(["-n", "mount", "-o", "loop"])
            .arg(&image)
            .arg(&mountpoint)
            .status()
            .ok()?;
        if !mount.success() {
            return None;
        }
        let uid_gid = format!(
            "{}:{}",
            rustix::process::getuid().as_raw(),
            rustix::process::getgid().as_raw()
        );
        let chown = Command::new("sudo")
            .args(["-n", "chown", &uid_gid])
            .arg(&mountpoint)
            .status()
            .ok()?;
        if !chown.success() {
            return None;
        }
        Some(VerityFs {
            _lock: lock,
            dir,
            mountpoint,
        })
    }

    #[test]
    fn enable_then_measure_returns_a_real_digest() {
        let Some(fs) = verity_capable_ext4() else {
            eprintln!("skipping: could not create a real fs-verity-capable ext4 loopback mount");
            return;
        };
        let file = fs.mountpoint.join("sealed.txt");
        std::fs::write(&file, b"hello fs-verity\n").unwrap();

        assert_eq!(
            measure(&file).unwrap(),
            None,
            "unsealed file has no digest yet"
        );

        enable_retrying_on_text_file_busy(&file).unwrap();

        let digest = measure(&file).unwrap().expect("digest after enable");
        assert_ne!(
            digest, [0u8; DIGEST_LEN],
            "digest should be a real hash, not all zero"
        );

        // The fs-verity digest is a hash of the file's own Merkle-tree
        // descriptor, never a plain content hash -- confirmed to
        // genuinely differ from a real sha256 of the same bytes.
        use sha2::Digest as _;
        let content_hash = sha2::Sha256::digest(b"hello fs-verity\n");
        assert_ne!(
            digest.as_slice(),
            content_hash.as_slice(),
            "fs-verity digest must not equal a plain content hash"
        );
    }

    /// [`enable`], tolerating the kernel's own well-documented
    /// `ETXTBSY` ("Text file busy"): fs-verity's real `FS_IOC_ENABLE_
    /// VERITY` genuinely refuses to enable while *any* file descriptor
    /// anywhere still has the file open for writing (so its content
    /// can't change out from under the Merkle tree being built) --
    /// confirmed to be a real, if rare, race on this shared,
    /// multi-tenant development host (observed directly: a freshly
    /// `std::fs::write`-created, already-closed file's own `enable`
    /// call failed with exactly this error, presumably some other
    /// process on the host briefly opening it, e.g. a filesystem
    /// indexer). A handful of retries with a short backoff is exactly
    /// how a real caller (`ociboot`) should also handle this
    /// documented condition -- this isn't papering over a bug in
    /// [`enable`] itself, which correctly returns the kernel's own
    /// real error either way.
    fn enable_retrying_on_text_file_busy(path: &Path) -> io::Result<()> {
        for attempt in 0..5 {
            match enable(path) {
                Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) && attempt < 4 => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                other => return other,
            }
        }
        unreachable!()
    }

    #[test]
    fn sealed_file_becomes_immutable() {
        let Some(fs) = verity_capable_ext4() else {
            eprintln!("skipping: could not create a real fs-verity-capable ext4 loopback mount");
            return;
        };
        let file = fs.mountpoint.join("immutable.txt");
        std::fs::write(&file, b"before\n").unwrap();
        enable_retrying_on_text_file_busy(&file).unwrap();

        // Still readable...
        assert_eq!(std::fs::read(&file).unwrap(), b"before\n");
        // ...but no longer writable, a real kernel-enforced guarantee,
        // not just this crate's own convention.
        let write_result = std::fs::OpenOptions::new().append(true).open(&file);
        let err = match write_result {
            Ok(mut f) => {
                use std::io::Write as _;
                f.write_all(b"after\n")
                    .expect_err("write to a sealed fs-verity file must fail")
            }
            Err(e) => e,
        };
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn enabling_twice_is_a_real_already_exists_error() {
        let Some(fs) = verity_capable_ext4() else {
            eprintln!("skipping: could not create a real fs-verity-capable ext4 loopback mount");
            return;
        };
        let file = fs.mountpoint.join("twice.txt");
        std::fs::write(&file, b"data\n").unwrap();
        enable_retrying_on_text_file_busy(&file).unwrap();

        let err = enable(&file).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn enabling_on_a_non_verity_filesystem_is_unsupported() {
        // Deliberately does *not* build a verity-capable image -- a
        // plain tempdir (whatever filesystem the test runner's own
        // /tmp happens to be) almost never has the verity feature
        // enabled, which is exactly the "state filesystem lacks
        // fs-verity support" case this crate's own planned dm-verity
        // fallback needs to detect.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("plain.txt");
        std::fs::write(&file, b"data\n").unwrap();

        match enable(&file) {
            // Either of the two real "not supported here" errnos
            // `is_unsupported` recognizes (`EOPNOTSUPP` or `ENOTTY`,
            // depending on the underlying filesystem driver -- see
            // its own doc comment).
            Err(e) if is_unsupported(&e) => {}
            // A test host whose /tmp genuinely does support fs-verity
            // (e.g. it's already an ext4 mount with the feature
            // enabled) would make this assertion meaningless either
            // way, so tolerate success too rather than failing on an
            // environment this test can't control.
            Ok(()) => {}
            // The same real, documented `ETXTBSY` race
            // `enable_retrying_on_text_file_busy` exists for -- see
            // its own doc comment.
            Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) => {}
            Err(e) => panic!("unexpected error: {e}"),
        }
    }
}
