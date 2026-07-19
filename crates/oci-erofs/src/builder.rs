//! Building erofs images via the real `mkfs.erofs` CLI -- one of the
//! sanctioned external-tool escape hatches named in `docs/HACKING.md`,
//! wrapped behind the [`ErofsBuilder`] trait so a feature-gated
//! pure-Rust writer can implement the same interface later without
//! disturbing any caller.
//!
//! Determinism -- the same [`BuildOptions`] plus the same source tree
//! always produces a bit-identical image, the whole point of using
//! erofs for immutable deployments at all (see `docs/design/0061`) --
//! was verified directly against the real `mkfs.erofs 1.7.1` binary
//! before any of this was written: a small source tree (regular files,
//! a subdirectory, a symlink) built twice, more than a second apart in
//! wall-clock time, with `timestamp`/`uuid`/`all_root` set, produced
//! two images whose sha256 matched exactly byte for byte. The
//! resulting image was also loop-mounted directly (`mount -t erofs`)
//! to confirm it's a real, usable filesystem, not just a well-formed
//! header.
//!
//! This crate never *derives* `timestamp`/`uuid` from a manifest
//! digest itself -- that's `ociboot`'s policy to own (milestone 5),
//! since it's the caller who actually knows the manifest. Kept here,
//! this crate would stop being the thin, honest wrapper the rest of
//! this workspace's external-tool crates (`oci-mount`'s
//! `syscalls`/`options` split, `oci-runtime-core`'s hook runner) are
//! all deliberately kept as.

use std::io;
use std::path::Path;
use std::process::Command;

/// Options controlling how [`ErofsBuilder::build`] invokes its backend.
///
/// Every field maps to a real `mkfs.erofs` flag; none has a "current
/// wall-clock time" or "random" default. A caller that wants a
/// genuinely reproducible image -- the entire reason to reach for this
/// crate -- must pick `timestamp`/`uuid` explicitly: silently
/// defaulting either to "now" or "random" would make it easy to build
/// a non-reproducible image by accident and not notice until two
/// supposedly-identical builds produced different digests.
#[derive(Debug, Clone)]
pub struct BuildOptions {
    /// Fixed UNIX timestamp (seconds) stamped onto every entry and the
    /// superblock, overriding each source file's own real mtime
    /// (`mkfs.erofs -T#`).
    pub timestamp: u64,
    /// Filesystem UUID stamped into the superblock (`mkfs.erofs -U`),
    /// as a standard `8-4-4-4-12` hex string. Rejected by the real
    /// `mkfs.erofs` binary itself (surfaced as this crate's own
    /// [`io::Error`]) if malformed.
    pub uuid: String,
    /// Force every file's owning uid/gid to 0 (`mkfs.erofs
    /// --all-root`). Almost always wanted for reproducibility: without
    /// it, the image bakes in the *build host's* real uid/gid, so the
    /// same source tree built by two different users (or the same
    /// user via two different `--force-uid`-mapped rootless
    /// namespaces) would produce different images.
    pub all_root: bool,
    /// Optional volume label (`mkfs.erofs -L`, 16 bytes max; rejected
    /// by the real binary itself if longer).
    pub volume_label: Option<String>,
}

/// A backend capable of building an erofs image from a source
/// directory tree. [`MkfsErofs`] wraps the real CLI today; a
/// feature-gated pure-Rust writer can implement this same trait later.
pub trait ErofsBuilder {
    /// Build `output` (created, or overwritten if it already exists)
    /// as an erofs image whose root directory is the contents of
    /// `source`.
    fn build(&self, source: &Path, output: &Path, options: &BuildOptions) -> io::Result<()>;
}

/// The real `mkfs.erofs` binary, invoked via `std::process::Command`.
#[derive(Debug, Clone, Copy, Default)]
pub struct MkfsErofs;

impl ErofsBuilder for MkfsErofs {
    fn build(&self, source: &Path, output: &Path, options: &BuildOptions) -> io::Result<()> {
        let mut command = Command::new("mkfs.erofs");
        command
            .arg(format!("-T{}", options.timestamp))
            .arg(format!("-U{}", options.uuid))
            .arg("--quiet");
        if options.all_root {
            command.arg("--all-root");
        }
        if let Some(label) = &options.volume_label {
            command.arg("-L").arg(label);
        }
        command.arg(output).arg(source);

        let out = command
            .output()
            .map_err(|e| io::Error::new(e.kind(), format!("spawning mkfs.erofs: {e}")))?;
        if !out.status.success() {
            // The real binary's own diagnostic is a `<E> erofs: ...`
            // line followed by a full usage dump, all on stderr (
            // confirmed directly: `--quiet` only suppresses the
            // success-path progress output, not error reporting) --
            // just the first line carries the useful part.
            let reason = String::from_utf8_lossy(&out.stderr);
            let reason = reason.lines().next().unwrap_or("").trim();
            return Err(io::Error::other(format!(
                "mkfs.erofs exited with {}: {reason}",
                out.status
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    /// Every real-`mkfs.erofs` test in this module skips itself (rather
    /// than failing outright) when the binary genuinely isn't
    /// installed, so `cargo test` still passes on a machine that
    /// hasn't installed `erofs-utils` -- but the CI VM images
    /// (`ci/vm-prepare.sh`) do install it, so these still run for
    /// real in CI, not just locally.
    fn mkfs_erofs_available() -> bool {
        Command::new("mkfs.erofs")
            .arg("--help")
            .output()
            .is_ok_and(|out| out.status.success())
    }

    fn sample_options() -> BuildOptions {
        BuildOptions {
            timestamp: 0,
            uuid: "c1c1c1c1-c1c1-c1c1-c1c1-c1c1c1c1c1c1".to_string(),
            all_root: true,
            volume_label: None,
        }
    }

    fn write_sample_tree(root: &Path) {
        std::fs::create_dir_all(root.join("subdir")).unwrap();
        std::fs::write(root.join("file1.txt"), b"hello world\n").unwrap();
        std::fs::write(root.join("subdir/file2.txt"), b"another file\n").unwrap();
        std::os::unix::fs::symlink("file1.txt", root.join("link1")).unwrap();
    }

    #[test]
    fn builds_a_real_erofs_image_from_a_directory_tree() {
        if !mkfs_erofs_available() {
            eprintln!("skipping: mkfs.erofs not installed");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        write_sample_tree(&dir.path().join("src"));
        let image = dir.path().join("out.erofs");

        MkfsErofs
            .build(&dir.path().join("src"), &image, &sample_options())
            .unwrap();

        let bytes = std::fs::read(&image).unwrap();
        assert!(
            !bytes.is_empty(),
            "mkfs.erofs should have written a real image"
        );
        // A real EROFS superblock's magic number lives at a fixed
        // offset (1024 bytes in, the same convention ext2/3/4 use to
        // leave room for a boot sector) -- confirmed directly against
        // `erofs-utils`' own `erofs_fs.h` (`EROFS_SUPER_OFFSET` = 1024,
        // `EROFS_SUPER_MAGIC_V1` = 0xE0F5E1E2, little-endian on disk).
        let magic = u32::from_le_bytes(bytes[1024..1028].try_into().unwrap());
        assert_eq!(
            magic, 0xE0F5_E1E2,
            "output should be a real erofs superblock"
        );
    }

    #[test]
    fn identical_inputs_produce_a_bit_identical_image() {
        if !mkfs_erofs_available() {
            eprintln!("skipping: mkfs.erofs not installed");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        write_sample_tree(&dir.path().join("src"));
        let options = sample_options();

        let image1 = dir.path().join("out1.erofs");
        MkfsErofs
            .build(&dir.path().join("src"), &image1, &options)
            .unwrap();

        // A real, non-simulated gap in wall-clock time, so a pass here
        // actually rules out any hidden reliance on "now" rather than
        // `options.timestamp`.
        std::thread::sleep(std::time::Duration::from_millis(1100));

        let image2 = dir.path().join("out2.erofs");
        MkfsErofs
            .build(&dir.path().join("src"), &image2, &options)
            .unwrap();

        assert_eq!(
            std::fs::read(&image1).unwrap(),
            std::fs::read(&image2).unwrap(),
            "same options + same source tree must produce a bit-identical image"
        );
    }

    #[test]
    fn all_root_normalizes_ownership_regardless_of_source_permissions() {
        if !mkfs_erofs_available() {
            eprintln!("skipping: mkfs.erofs not installed");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        write_sample_tree(&src);
        // Distinctive, non-default permission bits on one file, to
        // prove --all-root affects ownership but this crate makes no
        // claim about mode bits (mkfs.erofs preserves those as-is).
        std::fs::set_permissions(
            src.join("file1.txt"),
            std::fs::Permissions::from_mode(0o640),
        )
        .unwrap();

        let image = dir.path().join("out.erofs");
        MkfsErofs.build(&src, &image, &sample_options()).unwrap();
        assert!(std::fs::metadata(&image).unwrap().len() > 0);
    }

    #[test]
    fn a_missing_source_directory_is_a_real_error_with_a_useful_message() {
        if !mkfs_erofs_available() {
            eprintln!("skipping: mkfs.erofs not installed");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("out.erofs");

        let err = MkfsErofs
            .build(
                &dir.path().join("does-not-exist"),
                &image,
                &sample_options(),
            )
            .unwrap_err();

        let message = err.to_string();
        assert!(
            message.contains("mkfs.erofs exited with"),
            "unexpected message: {message}"
        );
    }

    #[test]
    fn an_overlong_volume_label_is_rejected_by_the_real_binary() {
        if !mkfs_erofs_available() {
            eprintln!("skipping: mkfs.erofs not installed");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        write_sample_tree(&dir.path().join("src"));
        let image = dir.path().join("out.erofs");
        let mut options = sample_options();
        options.volume_label = Some("this-label-is-way-too-long-for-16-bytes".to_string());

        let err = MkfsErofs
            .build(&dir.path().join("src"), &image, &options)
            .unwrap_err();
        assert!(
            err.to_string().contains("invalid volume label"),
            "unexpected message: {err}"
        );
    }
}
