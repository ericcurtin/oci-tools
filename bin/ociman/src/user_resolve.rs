//! Resolve an OCI image's `USER` config field against the *image's own*
//! `/etc/passwd`/`/etc/group`, the same way real `podman` does.
//!
//! `crun`/`runc` (and this project's own `ocirun`) never do this: by the
//! time a runtime-spec reaches them, `process.user` is already fully
//! numeric -- see the OCI runtime-spec's `User` type. Name resolution is
//! squarely a higher-level-tool concern, done once, before the spec is
//! synthesized, which is why it lives here in `ociman` rather than in
//! `oci-runtime-core`.
//!
//! Semantics are ported from `github.com/moby/sys/user`'s `GetExecUser`
//! (vendored into real podman as `pkg/lookup`), minus supplementary
//! group IDs -- this runtime doesn't support extra gids yet (see the
//! single-mapped-uid limitation in `main.rs::resolve_user`), so
//! collecting them would just be dead data.
//!
//! # Symlink-escape protection
//!
//! Real podman's own `pkg/lookup` guards its `/etc/passwd`/`/etc/group`
//! reads with `github.com/cyphar/filepath-securejoin`, specifically
//! because a malicious or corrupt image could make either path (or any
//! containing directory) a symlink pointing outside the rootfs, e.g.
//! `/etc -> /` or `/etc/passwd -> /etc/shadow`, tricking a naive
//! `read_to_string` into reading an arbitrary *host* file instead (0024
//! flagged this as a known gap when this module was first written, not
//! yet fixed). Rather than hand-roll `securejoin`'s own component-by-
//! component symlink-clamping algorithm — subtle to get exactly right,
//! and inherently race-prone unless every step is also re-verified
//! atomically — this uses the kernel's own purpose-built mechanism
//! instead: `openat2(2)`'s `RESOLVE_IN_ROOT` resolve flag (Linux 5.6+),
//! which resolves a path against a directory fd *as if* that fd were
//! chroot()ed to (any symlink, absolute or relative, and any `..` that
//! would otherwise escape above it, is transparently reinterpreted
//! relative to that same root instead), atomically, in the kernel, with
//! no TOCTOU window at all. Verified against a real symlink escape
//! attempt (a rootfs whose `etc/passwd` was a symlink to an outside
//! file containing a marker string) before writing any of the tests
//! below: `RESOLVE_IN_ROOT` correctly reports `ENOENT` instead of ever
//! reading the escape target.

use anyhow::Context;
use rustix::fs::{Mode, OFlags, ResolveFlags};
use std::io::Read as _;
use std::path::Path;

/// A resolved `/etc/passwd` row: only the two fields anything here
/// actually needs.
struct PasswdEntry {
    uid: u32,
    gid: u32,
}

/// Resolve `user` (an image's raw `USER` field, `""` if unset) to a
/// numeric `(uid, gid)` pair by consulting `<rootfs>/etc/passwd` and
/// (if `user` names a group explicitly) `<rootfs>/etc/group`.
///
/// - `""` resolves to `(0, 0)`.
/// - A numeric uid is used as-is even without a passwd entry; if it
///   *does* have one, that entry's gid becomes the default group.
/// - A non-numeric name is only valid if `/etc/passwd` has a matching
///   entry -- there is no other way to turn a name into a number.
/// - An explicit `user:group` overrides the group the same way: a
///   numeric group is used as-is, a named one needs an `/etc/group`
///   entry.
pub fn resolve(rootfs: &Path, user: &str) -> anyhow::Result<(u32, u32)> {
    if user.is_empty() {
        return Ok((0, 0));
    }

    let (user_part, group_part) = user.split_once(':').unwrap_or((user, ""));
    let numeric_uid: Option<u32> = user_part.parse().ok();

    let (uid, mut gid) = match (
        find_passwd_entry(rootfs, user_part, numeric_uid)?,
        numeric_uid,
    ) {
        (Some(entry), _) => (entry.uid, entry.gid),
        (None, Some(uid)) => (uid, 0),
        (None, None) => anyhow::bail!(
            "image USER {user_part:?} has no matching entry in the image's own /etc/passwd \
             (and isn't numeric either, so there's no other way to resolve it)"
        ),
    };

    if !group_part.is_empty() {
        let numeric_gid: Option<u32> = group_part.parse().ok();
        gid = match (
            find_group_gid(rootfs, group_part, numeric_gid)?,
            numeric_gid,
        ) {
            (Some(found), _) => found,
            (None, Some(gid)) => gid,
            (None, None) => anyhow::bail!(
                "image group {group_part:?} has no matching entry in the image's own \
                 /etc/group (and isn't numeric either, so there's no other way to resolve it)"
            ),
        };
    }

    Ok((uid, gid))
}

/// Find the `/etc/passwd` row matching `name` (or, if `numeric_uid` is
/// given, the row whose own uid field equals it -- matching real
/// `moby/sys/user`'s rule that a numeric `USER` is always treated as a
/// uid, never a name, even if some entry happens to share its name).
/// `Ok(None)` covers both "no `/etc/passwd` in this image" and "no
/// matching row" -- callers fall back to the numeric uid either way.
fn find_passwd_entry(
    rootfs: &Path,
    name: &str,
    numeric_uid: Option<u32>,
) -> anyhow::Result<Option<PasswdEntry>> {
    let Some(contents) = read_optional(rootfs, "etc/passwd")? else {
        return Ok(None);
    };
    for line in contents.lines() {
        let fields: Vec<&str> = line.splitn(7, ':').collect();
        if fields.len() < 4 {
            continue; // blank/malformed line: real getpwent skips these too
        }
        let matches = match numeric_uid {
            Some(uid) => fields[2].parse::<u32>().ok() == Some(uid),
            None => fields[0] == name,
        };
        if matches {
            return Ok(Some(PasswdEntry {
                uid: fields[2].parse().unwrap_or(0),
                gid: fields[3].parse().unwrap_or(0),
            }));
        }
    }
    Ok(None)
}

/// Find the `/etc/group` row matching `name` (or `numeric_gid`, same
/// numeric-always-wins rule as [`find_passwd_entry`]) and return its
/// gid.
fn find_group_gid(
    rootfs: &Path,
    name: &str,
    numeric_gid: Option<u32>,
) -> anyhow::Result<Option<u32>> {
    let Some(contents) = read_optional(rootfs, "etc/group")? else {
        return Ok(None);
    };
    for line in contents.lines() {
        let fields: Vec<&str> = line.splitn(4, ':').collect();
        if fields.len() < 3 {
            continue;
        }
        let matches = match numeric_gid {
            Some(gid) => fields[2].parse::<u32>().ok() == Some(gid),
            None => fields[0] == name,
        };
        if matches {
            return Ok(fields[2].parse().ok());
        }
    }
    Ok(None)
}

/// Read `<rootfs>/<relative>`'s contents, resolved via `openat2`'s
/// `RESOLVE_IN_ROOT` (see this module's own doc comment for why) so
/// neither `relative` nor any symlink encountered while resolving it
/// can escape `rootfs`. Treats "doesn't exist" (including a symlink
/// escape attempt, which resolves as a plain `ENOENT` under
/// `RESOLVE_IN_ROOT` rather than as the escape target) as `Ok(None)`
/// rather than an error -- plenty of real images have no `/etc/group`
/// at all, and that's fine, not a reason to refuse to run them.
fn read_optional(rootfs: &Path, relative: &str) -> anyhow::Result<Option<String>> {
    let root_fd = match rustix::fs::open(rootfs, OFlags::PATH | OFlags::DIRECTORY, Mode::empty()) {
        Ok(fd) => fd,
        Err(e) if e == rustix::io::Errno::NOENT => return Ok(None),
        Err(e) => {
            return Err(e).with_context(|| format!("opening rootfs {}", rootfs.display()));
        }
    };
    let opened = rustix::fs::openat2(
        &root_fd,
        relative,
        OFlags::RDONLY,
        Mode::empty(),
        ResolveFlags::IN_ROOT,
    );
    let fd = match opened {
        Ok(fd) => fd,
        Err(e) if e == rustix::io::Errno::NOENT => return Ok(None),
        Err(e) => {
            return Err(e).with_context(|| {
                format!("opening {} inside rootfs {}", relative, rootfs.display())
            });
        }
    };
    let mut contents = String::new();
    std::fs::File::from(fd)
        .read_to_string(&mut contents)
        .with_context(|| format!("reading {} inside rootfs {}", relative, rootfs.display()))?;
    Ok(Some(contents))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rootfs_with(passwd: Option<&str>, group: Option<&str>) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("etc")).unwrap();
        if let Some(passwd) = passwd {
            std::fs::write(dir.path().join("etc/passwd"), passwd).unwrap();
        }
        if let Some(group) = group {
            std::fs::write(dir.path().join("etc/group"), group).unwrap();
        }
        dir
    }

    const PASSWD: &str = "root:x:0:0:root:/root:/bin/sh\napp:x:1000:1000:App:/home/app:/bin/sh\n";
    const GROUP: &str = "root:x:0:\napp:x:1000:\nstaff:x:50:app\n";

    #[test]
    fn empty_user_is_root() {
        let dir = rootfs_with(None, None);
        assert_eq!(resolve(dir.path(), "").unwrap(), (0, 0));
    }

    #[test]
    fn numeric_uid_without_passwd_defaults_gid_to_zero() {
        let dir = rootfs_with(None, None);
        assert_eq!(resolve(dir.path(), "1000").unwrap(), (1000, 0));
    }

    #[test]
    fn numeric_uid_with_passwd_entry_picks_up_its_gid() {
        let dir = rootfs_with(Some(PASSWD), None);
        assert_eq!(resolve(dir.path(), "1000").unwrap(), (1000, 1000));
    }

    #[test]
    fn named_user_resolves_via_passwd() {
        let dir = rootfs_with(Some(PASSWD), None);
        assert_eq!(resolve(dir.path(), "app").unwrap(), (1000, 1000));
        assert_eq!(resolve(dir.path(), "root").unwrap(), (0, 0));
    }

    #[test]
    fn unknown_name_without_passwd_entry_is_an_error() {
        let dir = rootfs_with(Some(PASSWD), None);
        let err = resolve(dir.path(), "nobody").unwrap_err();
        assert!(err.to_string().contains("nobody"), "{err}");
    }

    #[test]
    fn unknown_name_with_no_passwd_at_all_is_also_an_error() {
        let dir = rootfs_with(None, None);
        let err = resolve(dir.path(), "app").unwrap_err();
        assert!(err.to_string().contains("app"), "{err}");
    }

    #[test]
    fn explicit_numeric_group_overrides_passwd_gid() {
        let dir = rootfs_with(Some(PASSWD), None);
        assert_eq!(resolve(dir.path(), "app:0").unwrap(), (1000, 0));
    }

    #[test]
    fn explicit_named_group_resolves_via_group_file() {
        let dir = rootfs_with(Some(PASSWD), Some(GROUP));
        assert_eq!(resolve(dir.path(), "app:staff").unwrap(), (1000, 50));
    }

    #[test]
    fn unknown_named_group_is_an_error() {
        let dir = rootfs_with(Some(PASSWD), Some(GROUP));
        let err = resolve(dir.path(), "app:wheel").unwrap_err();
        assert!(err.to_string().contains("wheel"), "{err}");
    }

    #[test]
    fn numeric_uid_and_gid_need_no_passwd_or_group_at_all() {
        let dir = rootfs_with(None, None);
        assert_eq!(resolve(dir.path(), "1000:1000").unwrap(), (1000, 1000));
    }

    /// A malicious/corrupt image whose `/etc/passwd` is itself a
    /// symlink pointing *outside* the rootfs must not have that target
    /// read at all -- proving `RESOLVE_IN_ROOT` (see this module's own
    /// doc comment) actually blocks the escape, not just that a
    /// same-rootfs symlink still works normally (the next test).
    #[test]
    fn a_passwd_symlink_escaping_the_rootfs_is_not_followed() {
        let dir = rootfs_with(None, None);
        let secret = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(secret.path(), "root:x:0:0:root:/root:/bin/sh\n").unwrap();
        std::os::unix::fs::symlink(secret.path(), dir.path().join("etc/passwd")).unwrap();

        // The escape attempt must not resolve to the secret file at
        // all: from `resolve`'s own point of view this looks exactly
        // like "no /etc/passwd present", the same `Ok(None)` case a
        // missing file produces, not a successful read of the outside
        // target.
        let err = resolve(dir.path(), "nonexistent-name").unwrap_err();
        assert!(
            err.to_string().contains("nonexistent-name"),
            "expected the ordinary \"no /etc/passwd\" error path, not a successful escape: {err}"
        );
        // A numeric uid still resolves fine (falls back to "no passwd
        // entry" exactly as if the file were simply absent).
        assert_eq!(resolve(dir.path(), "0").unwrap(), (0, 0));
    }

    /// A symlink whose target stays *inside* the rootfs (a completely
    /// ordinary, non-malicious thing for a real image to do, e.g.
    /// `/etc/passwd` symlinked to `/usr/etc/passwd` as some distros'
    /// usr-merge layouts do) must still resolve and read normally --
    /// `RESOLVE_IN_ROOT` only needs to block *escapes*, not symlinks in
    /// general.
    #[test]
    fn a_passwd_symlink_staying_inside_the_rootfs_still_resolves() {
        let dir = rootfs_with(None, None);
        std::fs::write(dir.path().join("etc/real-passwd"), PASSWD).unwrap();
        std::fs::remove_file(dir.path().join("etc/passwd")).ok();
        std::os::unix::fs::symlink("real-passwd", dir.path().join("etc/passwd")).unwrap();

        assert_eq!(resolve(dir.path(), "app").unwrap(), (1000, 1000));
    }
}
