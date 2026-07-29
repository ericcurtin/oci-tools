//! Synthesizing a real `/etc/resolv.conf` for a container with no
//! network namespace of its own (`docs/design/0297`, closing `0296`'s
//! own "still ahead") — a direct port of real cri-o's own
//! `ParseDNSOptions` (`~/git/cri-o/internal/lib/sandbox/infra.go`),
//! deliberately *not* real podman's own richer `libnetwork/resolvconf`
//! package: that package's own extra logic (filtering `127.0.0.1`/
//! `127.0.0.53` loopback nameservers, namespace-aware `KeepHost*`
//! merging) exists specifically for a container with its *own* private
//! network namespace, a case this project has no equivalent of at all
//! — every container here already shares the calling process's own
//! real network namespace unmodified (`Spec::into_rootless` strips the
//! `network` namespace entry outright), so a real host nameserver
//! genuinely *is* reachable from inside the container exactly as it is
//! from the host, with nothing to filter.

use std::io;
use std::path::Path;

const HOST_RESOLV_CONF: &str = "/etc/resolv.conf";

/// Write a real `/etc/resolv.conf` into `root` (a container's own
/// effective, currently-writable root), creating `root/etc` first if
/// needed — matching real cri-o's own `ParseDNSOptions` exactly: with
/// no explicit `servers`/`searches`/`options` at all (the common,
/// unconfigured case), copies the real host's own `/etc/resolv.conf`
/// verbatim (meaningful, not just cosmetic, precisely because this
/// project's own containers share the host's real network namespace —
/// see this module's own doc comment); otherwise synthesizes one from
/// scratch, in real cri-o's own exact field order: `search` line
/// first (if any), then one `nameserver` line per server, then
/// `options` last (if any) — never a blend of "some real host lines,
/// some explicit ones" the way real podman's own separate
/// `KeepHost*`-flagged merge mode can produce, matching cri-o's own
/// simpler, unconditional "either/or" rule instead.
pub fn write_resolv_conf(
    root: &Path,
    servers: &[String],
    searches: &[String],
    options: &[String],
) -> io::Result<()> {
    let etc_dir = root.join("etc");
    std::fs::create_dir_all(&etc_dir)?;
    let dest = etc_dir.join("resolv.conf");

    if servers.is_empty() && searches.is_empty() && options.is_empty() {
        // A missing host `/etc/resolv.conf` (a real, if unusual,
        // possibility -- e.g. a minimal container image being used as
        // this process's own root) is tolerated exactly like real
        // cri-o's own `copyFile` would fail loudly on it: propagated
        // as a real error, not silently skipped, so a caller relying
        // on DNS working still finds out immediately rather than
        // discovering it only once name resolution itself starts
        // failing inside the container.
        std::fs::copy(HOST_RESOLV_CONF, &dest)?;
        return Ok(());
    }

    let mut content = String::new();
    if !searches.is_empty() {
        content.push_str("search ");
        content.push_str(&searches.join(" "));
        content.push('\n');
    }
    for server in servers {
        content.push_str("nameserver ");
        content.push_str(server);
        content.push('\n');
    }
    if !options.is_empty() {
        content.push_str("options ");
        content.push_str(&options.join(" "));
        content.push('\n');
    }
    std::fs::write(&dest, content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthesizes_search_nameservers_and_options_in_the_real_cri_o_order() {
        let dir = tempfile::tempdir().unwrap();
        write_resolv_conf(
            dir.path(),
            &["10.0.0.1".to_string(), "10.0.0.2".to_string()],
            &["example.com".to_string(), "internal".to_string()],
            &["ndots:5".to_string()],
        )
        .unwrap();
        let content = std::fs::read_to_string(dir.path().join("etc/resolv.conf")).unwrap();
        assert_eq!(
            content,
            "search example.com internal\nnameserver 10.0.0.1\nnameserver 10.0.0.2\noptions ndots:5\n"
        );
    }

    #[test]
    fn only_servers_given_omits_search_and_options_lines_entirely() {
        let dir = tempfile::tempdir().unwrap();
        write_resolv_conf(dir.path(), &["1.1.1.1".to_string()], &[], &[]).unwrap();
        let content = std::fs::read_to_string(dir.path().join("etc/resolv.conf")).unwrap();
        assert_eq!(content, "nameserver 1.1.1.1\n");
    }

    #[test]
    fn only_options_given_still_creates_a_real_file_with_just_that_line() {
        let dir = tempfile::tempdir().unwrap();
        write_resolv_conf(dir.path(), &[], &[], &["edns0".to_string()]).unwrap();
        let content = std::fs::read_to_string(dir.path().join("etc/resolv.conf")).unwrap();
        assert_eq!(content, "options edns0\n");
    }

    #[test]
    fn nothing_given_at_all_copies_the_real_hosts_own_resolv_conf_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        write_resolv_conf(dir.path(), &[], &[], &[]).unwrap();
        let content = std::fs::read_to_string(dir.path().join("etc/resolv.conf")).unwrap();
        let host_content = std::fs::read_to_string(HOST_RESOLV_CONF).unwrap();
        assert_eq!(content, host_content);
    }

    #[test]
    fn creates_a_missing_etc_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!dir.path().join("etc").exists());
        write_resolv_conf(dir.path(), &["1.1.1.1".to_string()], &[], &[]).unwrap();
        assert!(dir.path().join("etc").is_dir());
    }
}
