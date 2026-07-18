//! Current-process identity (effective uid/gid), read directly from
//! `/proc/self/status` rather than pulling in a syscall-wrapper crate for
//! two integers. Shared by every binary that needs to know whether (and
//! as whom) it is running rootless — `ocirun spec --rootless` today,
//! rootless container creation later.

/// The effective UID and GID of the current process. Falls back to `(0,
/// 0)` if `/proc/self/status` cannot be read or parsed (should not happen
/// on any Linux the workspace supports; erring toward "root" here is the
/// same behavior a failed syscall would produce as a raw `-1`/`errno`, and
/// callers already treat uid 0 as the conservative default).
pub fn effective_uid_gid() -> (u32, u32) {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return (0, 0);
    };
    let uid = field(&status, "Uid:").unwrap_or(0);
    let gid = field(&status, "Gid:").unwrap_or(0);
    (uid, gid)
}

/// Parse the effective (second) column of a `Uid:`/`Gid:` line:
/// `Uid:\t<real>\t<effective>\t<saved>\t<fs>`.
fn field(status: &str, prefix: &str) -> Option<u32> {
    status
        .lines()
        .find_map(|line| line.strip_prefix(prefix))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_proc_self_status() {
        // We can't control our own uid in a unit test, but we can check
        // the dependency-free parse agrees with a second, independent
        // parse of the same file (catches off-by-one column bugs).
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        let expected_uid: u32 = status
            .lines()
            .find(|l| l.starts_with("Uid:"))
            .unwrap()
            .split_whitespace()
            .nth(2) // ["Uid:", real, effective, ...]
            .unwrap()
            .parse()
            .unwrap();
        let (uid, _gid) = effective_uid_gid();
        assert_eq!(uid, expected_uid);
    }

    #[test]
    fn field_parses_synthetic_line() {
        let status = "Name:\ttest\nUid:\t1000\t1001\t1000\t1000\nGid:\t2000\t2001\t2000\t2000\n";
        assert_eq!(field(status, "Uid:"), Some(1001));
        assert_eq!(field(status, "Gid:"), Some(2001));
    }

    #[test]
    fn field_missing_line_is_none() {
        assert_eq!(field("Name:\ttest\n", "Uid:"), None);
    }
}
