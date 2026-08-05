//! Synthesizing a real `/etc/hosts` for a container with no network
//! namespace of its own — originally `ociman`-private (`docs/design/
//! 0147`), moved here (`docs/design/0296`) once `ocicri` needed the
//! identical primitive for the exact same reason: neither project has
//! any container-networking setup of its own at all (no bridge/pasta/
//! CNI), so every container's own synthesized `/etc/hosts` always
//! matches real podman's own `--network=none` case specifically —
//! `own_names` map to `127.0.0.1`, the same address a real
//! `--network=none` podman container's own loopback-only view would
//! resolve them to. A real, verified-zero-behavior-change move: this
//! module's own complete test suite moved unmodified and still passes
//! byte-for-byte identically.

use std::io;
use std::path::Path;

/// One `--add-host` entry, parsed: real podman's own `name[;name2
/// ...]:IP` syntax (checked directly against
/// `~/git/container-libs/common/libnetwork/etchosts`'s own
/// `parseExtraHosts`). The special `host-gateway` IP keyword (real
/// podman resolves it to a real host-reachable gateway address) isn't
/// supported — this project sets up no container networking of its
/// own at all yet, so there is no real address to resolve it to (see
/// `docs/design/0147`'s own "what this doesn't do yet").
pub fn parse_extra_host(entry: &str) -> io::Result<(Vec<String>, String)> {
    let Some((names, ip)) = entry.split_once(':') else {
        return Err(io::Error::other(format!(
            "--add-host {entry:?}: expected HOST:IP (or HOST1;HOST2:IP)"
        )));
    };
    if names.is_empty() {
        return Err(io::Error::other(format!(
            "--add-host {entry:?}: no hostname given"
        )));
    }
    if ip.is_empty() {
        return Err(io::Error::other(format!(
            "--add-host {entry:?}: the IP address is empty"
        )));
    }
    if ip == "host-gateway" {
        return Err(io::Error::other(format!(
            "--add-host {entry:?}: the \"host-gateway\" IP keyword isn't supported yet (this \
             project sets up no container networking of its own yet, so there is no real \
             host-reachable gateway address to resolve it to)"
        )));
    }
    Ok((
        names.split(';').map(str::to_string).collect(),
        ip.to_string(),
    ))
}

/// Write a real `/etc/hosts` file into `root` (a container's own
/// effective, currently-writable root — `rootfs/` for a plain-
/// extraction container, or the private overlay `upper/` directory
/// for one using this project's own rootless-overlay optimization,
/// see `rootfs_setup::upper_dir`), creating `root/etc` first if the
/// base image didn't already ship one (common for a minimal image —
/// even a bare `busybox` rootfs may have no `/etc` directory at all).
///
/// `own_names` are this container's own identity names, mapped to
/// `127.0.0.1` (empty for a build container — see `ociman::build`'s
/// own call site, which has no single, fixed identity the way a real
/// running container's own hostname/`--name` does).
///
/// Entries, in the same order real podman's own `etchosts.New`
/// writes them (`~/git/container-libs/common/libnetwork/etchosts/
/// hosts.go`): `add_host`'s own entries first (so a user-given
/// override for e.g. `localhost` genuinely takes precedence), then
/// the built-in `127.0.0.1`/`::1 localhost` and `own_names` entries —
/// each only added for a name not already claimed by an earlier
/// entry, matching real podman's own `addEntriesIfNotExists` exactly,
/// rather than ever overwriting a user's own explicit `--add-host`
/// entry.
///
/// This project sets up no container networking of its own at all
/// yet (no bridge/pasta/CNI), so every container's own synthesized
/// `/etc/hosts` always matches real podman's own `--network=none`
/// case specifically: `own_names` map to `127.0.0.1`, the same
/// address a real `--network=none` podman container's own loopback-
/// only view would resolve them to.
pub fn write_etc_hosts(root: &Path, own_names: &[&str], add_host: &[String]) -> io::Result<()> {
    let mut claimed_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut lines = String::new();

    for entry in add_host {
        let (names, ip) = parse_extra_host(entry)?;
        lines.push_str(&format!("{ip}\t{}\n", names.join(" ")));
        claimed_names.extend(names);
    }

    // From here on, `claimed_names` is never updated further: every
    // one of the three built-in entries below is checked against the
    // *same*, user-entries-only set, matching real podman's own
    // `addEntriesIfNotExists` exactly -- an earlier built-in entry
    // claiming a name never blocks a later built-in entry that
    // happens to reuse it (e.g. the container's own hostname
    // genuinely being "localhost" still gets its own `127.0.0.1`
    // line, unaffected by the separate `127.0.0.1 localhost` line
    // above it).
    let write_builtin = |lines: &mut String, ip: &str, names: &[&str]| {
        let free: Vec<&str> = names
            .iter()
            .copied()
            .filter(|n| !claimed_names.contains(*n))
            .collect();
        if !free.is_empty() {
            lines.push_str(&format!("{ip}\t{}\n", free.join(" ")));
        }
    };
    write_builtin(&mut lines, "127.0.0.1", &["localhost"]);
    write_builtin(&mut lines, "::1", &["localhost"]);
    write_builtin(&mut lines, "127.0.0.1", own_names);

    let etc_dir = root.join("etc");
    std::fs::create_dir_all(&etc_dir)?;
    let hosts_path = etc_dir.join("hosts");
    std::fs::write(&hosts_path, lines)?;
    Ok(())
}

/// Write a real `/etc/hostname` file into `root` (same effective-root
/// convention as [`write_etc_hosts`]) — a real, checked-directly gap
/// found while researching `ociman build --no-hostname` (`0459`'s own
/// "deliberately still out of scope" note): every real container
/// engine bind-mounts (real `docker`/`podman`) or, here, directly
/// writes (matching this crate's own already-established simpler
/// approach for `/etc/hosts`/`/etc/resolv.conf`) a real `/etc/
/// hostname` file containing the container's own hostname, separate
/// from the UTS namespace's own `sethostname(2)` value (`spec.
/// hostname`) — a program that reads the *file* directly (rather
/// than calling `gethostname(2)`) needs this to see the right value
/// at all; checked directly, `~/git/podman/libpod/container_internal_
/// linux.go`'s own `c.writeStringToRundir("hostname", c.Hostname()+
/// "\n")` writes exactly this, the same value also passed to
/// `sethostname(2)`.
pub fn write_etc_hostname(root: &Path, hostname: &str) -> io::Result<()> {
    let etc_dir = root.join("etc");
    std::fs::create_dir_all(&etc_dir)?;
    std::fs::write(etc_dir.join("hostname"), format!("{hostname}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // `parse_extra_host` checked directly against real podman's own
    // `parseExtraHosts`
    // (`~/git/container-libs/common/libnetwork/etchosts/hosts.go`).
    #[test]
    fn parse_extra_host_splits_a_single_name() {
        assert_eq!(
            parse_extra_host("foo.example:10.0.0.5").unwrap(),
            (vec!["foo.example".to_string()], "10.0.0.5".to_string())
        );
    }

    #[test]
    fn parse_extra_host_splits_semicolon_separated_names() {
        assert_eq!(
            parse_extra_host("foo;bar;baz:10.0.0.5").unwrap(),
            (
                vec!["foo".to_string(), "bar".to_string(), "baz".to_string()],
                "10.0.0.5".to_string()
            )
        );
    }

    #[test]
    fn parse_extra_host_rejects_missing_colon() {
        assert!(parse_extra_host("no-colon-here").is_err());
    }

    #[test]
    fn parse_extra_host_rejects_empty_name_or_ip() {
        assert!(parse_extra_host(":10.0.0.5").is_err());
        assert!(parse_extra_host("foo:").is_err());
    }

    #[test]
    fn parse_extra_host_rejects_the_host_gateway_keyword() {
        let err = parse_extra_host("foo:host-gateway").unwrap_err();
        assert!(err.to_string().contains("host-gateway"));
    }

    #[test]
    fn write_etc_hosts_default_entries_with_no_add_host_at_all() {
        let dir = tempfile::tempdir().unwrap();
        write_etc_hosts(dir.path(), &["myhost"], &[]).unwrap();
        let content = std::fs::read_to_string(dir.path().join("etc/hosts")).unwrap();
        assert_eq!(
            content,
            "127.0.0.1\tlocalhost\n::1\tlocalhost\n127.0.0.1\tmyhost\n"
        );
    }

    #[test]
    fn write_etc_hosts_with_no_own_names_at_all_still_writes_the_localhost_entries() {
        // The shape `ociman::build`'s own call site uses: no single,
        // fixed identity the way a real running container's own
        // hostname/`--name` does.
        let dir = tempfile::tempdir().unwrap();
        write_etc_hosts(dir.path(), &[], &[]).unwrap();
        let content = std::fs::read_to_string(dir.path().join("etc/hosts")).unwrap();
        assert_eq!(content, "127.0.0.1\tlocalhost\n::1\tlocalhost\n");
    }

    #[test]
    fn write_etc_hosts_keeps_hostname_and_container_name_both_when_distinct() {
        let dir = tempfile::tempdir().unwrap();
        write_etc_hosts(dir.path(), &["myhost", "mycontainer"], &[]).unwrap();
        let content = std::fs::read_to_string(dir.path().join("etc/hosts")).unwrap();
        assert_eq!(
            content,
            "127.0.0.1\tlocalhost\n::1\tlocalhost\n127.0.0.1\tmyhost mycontainer\n"
        );
    }

    #[test]
    fn write_etc_hosts_add_host_entries_come_first() {
        let dir = tempfile::tempdir().unwrap();
        write_etc_hosts(dir.path(), &["myhost"], &["foo;bar:10.0.0.5".to_string()]).unwrap();
        let content = std::fs::read_to_string(dir.path().join("etc/hosts")).unwrap();
        assert_eq!(
            content,
            "10.0.0.5\tfoo bar\n127.0.0.1\tlocalhost\n::1\tlocalhost\n127.0.0.1\tmyhost\n"
        );
    }

    #[test]
    fn write_etc_hosts_a_user_add_host_overriding_localhost_suppresses_both_builtin_localhost_lines()
     {
        let dir = tempfile::tempdir().unwrap();
        write_etc_hosts(dir.path(), &["myhost"], &["localhost:9.9.9.9".to_string()]).unwrap();
        let content = std::fs::read_to_string(dir.path().join("etc/hosts")).unwrap();
        // Matches real podman's own `addEntriesIfNotExists` exactly:
        // both the `127.0.0.1 localhost` *and* `::1 localhost`
        // built-ins are checked against the same user-entries-only
        // set, so a user override of "localhost" suppresses both.
        assert_eq!(content, "9.9.9.9\tlocalhost\n127.0.0.1\tmyhost\n");
    }

    #[test]
    fn write_etc_hosts_creates_a_missing_etc_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!dir.path().join("etc").exists());
        write_etc_hosts(dir.path(), &["myhost"], &[]).unwrap();
        assert!(dir.path().join("etc").is_dir());
    }

    #[test]
    fn write_etc_hosts_surfaces_a_real_add_host_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = write_etc_hosts(dir.path(), &["myhost"], &["bad".to_string()]).unwrap_err();
        assert!(err.to_string().contains("--add-host"));
    }

    #[test]
    fn write_etc_hostname_writes_the_given_name_with_a_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        write_etc_hostname(dir.path(), "my-container").unwrap();
        let content = std::fs::read_to_string(dir.path().join("etc/hostname")).unwrap();
        assert_eq!(content, "my-container\n");
    }

    #[test]
    fn write_etc_hostname_creates_a_missing_etc_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!dir.path().join("etc").exists());
        write_etc_hostname(dir.path(), "my-container").unwrap();
        assert!(dir.path().join("etc").is_dir());
    }

    #[test]
    fn write_etc_hostname_with_an_empty_hostname_still_writes_a_real_bare_newline() {
        // Real buildah's own default for a `RUN` step with no
        // meaningfully-set `Config.Hostname` (`ociman build` has no
        // persisted config-level hostname of its own at all yet --
        // see `docs/design/0459`'s own "deliberately still out of
        // scope" note): still a real write, not skipped.
        let dir = tempfile::tempdir().unwrap();
        write_etc_hostname(dir.path(), "").unwrap();
        let content = std::fs::read_to_string(dir.path().join("etc/hostname")).unwrap();
        assert_eq!(content, "\n");
    }
}
