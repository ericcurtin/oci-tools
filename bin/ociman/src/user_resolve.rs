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

use anyhow::Context;
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
    let Some(contents) = read_optional(&rootfs.join("etc/passwd"))? else {
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
    let Some(contents) = read_optional(&rootfs.join("etc/group"))? else {
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

/// Read a file's contents, treating "doesn't exist" as `Ok(None)`
/// rather than an error -- plenty of real images have no `/etc/group`
/// at all, and that's fine, not a reason to refuse to run them.
fn read_optional(path: &Path) -> anyhow::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
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
}
