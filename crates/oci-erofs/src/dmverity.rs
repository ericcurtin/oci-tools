//! Sealing/verifying with a *detached* dm-verity hash tree, via the
//! real `veritysetup` CLI -- one of `docs/HACKING.md`'s own sanctioned
//! shellouts, named there specifically as the fallback for state
//! filesystems that don't support [`crate::verity`]'s fs-verity
//! ioctls at all.
//!
//! Both [`format`] and [`verify`] work entirely at the plain-file
//! level: `veritysetup format`/`verify` accept ordinary regular files
//! directly for both the data device and the hash-tree device, so
//! sealing/checking an already-built erofs image needs no loop device
//! or device-mapper activation at all -- confirmed directly, not
//! assumed from the man page (`veritysetup format` on two plain files
//! in a tempdir, as the calling unprivileged user, succeeds outright).
//! Actually *mounting* a dm-verity-protected image at boot
//! (`veritysetup open` against loop devices, presenting a new
//! `/dev/mapper/<name>` block device) is a much larger, genuinely
//! privileged, boot-time-flow concern that belongs to `ociboot-init`
//! later, not this module.

use std::io;
use std::path::Path;
use std::process::Command;

/// Options controlling [`format`]. Like
/// [`crate::builder::BuildOptions`], neither field has a "random"
/// default: `veritysetup format` generates a fresh random UUID and
/// salt on its own if not told otherwise, which would make the
/// resulting hash tree -- and therefore its own root hash -- different
/// every time even for byte-identical input, exactly the
/// non-reproducibility this crate exists to avoid.
#[derive(Debug, Clone)]
pub struct FormatOptions {
    /// UUID stamped into the hash tree's own superblock.
    pub uuid: String,
    /// Hex-encoded salt prepended to each hashed block (64 hex
    /// characters -- 32 bytes -- matches `veritysetup`'s own default
    /// salt size, but any even-length hex string it accepts works;
    /// the real binary validates this itself).
    pub salt: String,
}

/// Build a detached dm-verity hash tree for `data` at
/// `hash_tree_path` (created if it doesn't already exist), returning
/// the resulting root hash as a lowercase hex string.
///
/// Uses `--root-hash-file` (a real temporary file, read back and
/// discarded) rather than parsing the root hash back out of
/// `veritysetup`'s own human-readable summary on stdout -- that
/// summary's exact wording isn't a stable interface to depend on, and
/// a dedicated machine-readable output file already exists for
/// exactly this purpose.
pub fn format(data: &Path, hash_tree_path: &Path, options: &FormatOptions) -> io::Result<String> {
    let root_hash_file = tempfile::NamedTempFile::new()?;

    let out = Command::new("veritysetup")
        .arg("format")
        .arg(format!("--uuid={}", options.uuid))
        .arg(format!("--salt={}", options.salt))
        .arg(format!(
            "--root-hash-file={}",
            root_hash_file.path().display()
        ))
        .arg(data)
        .arg(hash_tree_path)
        .output()
        .map_err(|e| io::Error::new(e.kind(), format!("spawning veritysetup: {e}")))?;
    if !out.status.success() {
        let reason = String::from_utf8_lossy(&out.stderr);
        let reason = reason.lines().next().unwrap_or("").trim();
        return Err(io::Error::other(format!(
            "veritysetup format exited with {}: {reason}",
            out.status
        )));
    }

    let root_hash = std::fs::read_to_string(root_hash_file.path())?;
    Ok(root_hash.trim().to_string())
}

/// The outcome of [`verify`]: either the data genuinely matches
/// `root_hash` against its own hash tree, or it doesn't (corrupted
/// data, a corrupted/mismatched hash tree, or the wrong root hash
/// entirely) -- a real, expected, *checkable* outcome for a caller
/// deciding whether a given deployment is safe to boot, not something
/// that should force every caller to match on error text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyOutcome {
    /// `data` matches `hash_tree_path` matches `root_hash`, exactly.
    Valid,
    /// `veritysetup verify` itself ran successfully but reported a
    /// genuine mismatch (data corruption, a corrupted/rebuilt hash
    /// tree, or a root hash that simply doesn't match either).
    Invalid,
}

/// Verify `data` against `hash_tree_path` and `root_hash` (as
/// returned by [`format`]).
///
/// Returns `Ok(VerifyOutcome::Invalid)` -- not an error -- for a
/// genuine verification failure (confirmed directly against the real
/// binary's own distinct exit codes: `1` for a root hash that simply
/// doesn't match, `2` for corrupted data caught mid-tree), since that
/// is exactly the real-world case this function exists to let a
/// caller detect and act on (e.g. refuse to boot a corrupted
/// deployment) rather than treating alike with a setup problem.
/// Anything else nonzero (e.g. `4`, "device does not exist") is a
/// real environment/usage error, returned as `Err`.
pub fn verify(data: &Path, hash_tree_path: &Path, root_hash: &str) -> io::Result<VerifyOutcome> {
    let out = Command::new("veritysetup")
        .arg("verify")
        .arg(data)
        .arg(hash_tree_path)
        .arg(root_hash)
        .output()
        .map_err(|e| io::Error::new(e.kind(), format!("spawning veritysetup: {e}")))?;
    if out.status.success() {
        return Ok(VerifyOutcome::Valid);
    }
    match out.status.code() {
        Some(1) | Some(2) => Ok(VerifyOutcome::Invalid),
        _ => {
            let reason = String::from_utf8_lossy(&out.stderr);
            let reason = reason.lines().next().unwrap_or("").trim();
            Err(io::Error::other(format!(
                "veritysetup verify exited with {}: {reason}",
                out.status
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every test in this module skips itself with a clear message
    /// rather than failing outright if `veritysetup` genuinely isn't
    /// installed -- matching `oci-erofs::builder`'s own existing
    /// pattern for `mkfs.erofs`. Neither `format` nor `verify` needs
    /// any privilege at all (both were confirmed to work as a plain,
    /// unprivileged user against ordinary files), so unlike
    /// `oci-erofs::verity`'s own loopback-mount-dependent tests, none
    /// of this needs `sudo`.
    fn veritysetup_available() -> bool {
        Command::new("veritysetup")
            .arg("--version")
            .output()
            .is_ok_and(|out| out.status.success())
    }

    fn sample_options() -> FormatOptions {
        FormatOptions {
            uuid: "c1c1c1c1-c1c1-c1c1-c1c1-c1c1c1c1c1c1".to_string(),
            salt: "a".repeat(64),
        }
    }

    fn write_sample_data(dir: &Path) -> std::path::PathBuf {
        let data = dir.join("data.img");
        // A few real, distinct 4 KiB blocks (the default data block
        // size) rather than one repeated byte, so a single-byte
        // corruption test further down is unambiguous.
        let mut bytes = Vec::with_capacity(3 * 4096);
        for block in 0..3u8 {
            bytes.extend(std::iter::repeat_n(block.wrapping_add(1), 4096));
        }
        std::fs::write(&data, &bytes).unwrap();
        data
    }

    #[test]
    fn format_then_verify_a_real_hash_tree() {
        if !veritysetup_available() {
            eprintln!("skipping: veritysetup not installed");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let data = write_sample_data(dir.path());
        let hash_tree = dir.path().join("hash.img");

        let root_hash = format(&data, &hash_tree, &sample_options()).unwrap();
        assert_eq!(
            root_hash.len(),
            64,
            "sha256 root hash should be 64 hex chars: {root_hash}"
        );
        assert!(
            root_hash.chars().all(|c| c.is_ascii_hexdigit()),
            "root hash should be plain hex: {root_hash}"
        );
        assert!(
            std::fs::metadata(&hash_tree).unwrap().len() > 0,
            "format should have written a real hash tree file"
        );

        assert_eq!(
            verify(&data, &hash_tree, &root_hash).unwrap(),
            VerifyOutcome::Valid
        );
    }

    #[test]
    fn identical_inputs_produce_a_bit_identical_hash_tree_and_root_hash() {
        if !veritysetup_available() {
            eprintln!("skipping: veritysetup not installed");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let data = write_sample_data(dir.path());
        let options = sample_options();

        let hash_tree1 = dir.path().join("hash1.img");
        let root_hash1 = format(&data, &hash_tree1, &options).unwrap();

        // A real, non-simulated gap in wall-clock time, ruling out any
        // hidden reliance on "now" for either the hash tree bytes or
        // the root hash.
        std::thread::sleep(std::time::Duration::from_millis(1100));

        let hash_tree2 = dir.path().join("hash2.img");
        let root_hash2 = format(&data, &hash_tree2, &options).unwrap();

        assert_eq!(root_hash1, root_hash2);
        assert_eq!(
            std::fs::read(&hash_tree1).unwrap(),
            std::fs::read(&hash_tree2).unwrap()
        );
    }

    #[test]
    fn corrupted_data_fails_verification_without_being_a_rust_error() {
        if !veritysetup_available() {
            eprintln!("skipping: veritysetup not installed");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let data = write_sample_data(dir.path());
        let hash_tree = dir.path().join("hash.img");
        let root_hash = format(&data, &hash_tree, &sample_options()).unwrap();

        // Flip one byte in the first block, after sealing.
        let mut bytes = std::fs::read(&data).unwrap();
        bytes[10] ^= 0xff;
        std::fs::write(&data, &bytes).unwrap();

        assert_eq!(
            verify(&data, &hash_tree, &root_hash).unwrap(),
            VerifyOutcome::Invalid
        );
    }

    #[test]
    fn the_wrong_root_hash_fails_verification_without_being_a_rust_error() {
        if !veritysetup_available() {
            eprintln!("skipping: veritysetup not installed");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let data = write_sample_data(dir.path());
        let hash_tree = dir.path().join("hash.img");
        format(&data, &hash_tree, &sample_options()).unwrap();

        let wrong_root_hash = "0".repeat(64);
        assert_eq!(
            verify(&data, &hash_tree, &wrong_root_hash).unwrap(),
            VerifyOutcome::Invalid
        );
    }

    #[test]
    fn a_missing_data_file_is_a_real_error_not_an_invalid_outcome() {
        if !veritysetup_available() {
            eprintln!("skipping: veritysetup not installed");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist.img");
        let hash_tree = dir.path().join("hash.img");

        let err = format(&missing, &hash_tree, &sample_options()).unwrap_err();
        assert!(
            err.to_string().contains("veritysetup format exited with"),
            "unexpected message: {err}"
        );
    }
}
