//! `ociman search --list-tags --limit` (`docs/design/0543`): exercises
//! the actual built `ociman` binary's own truncation logic against a
//! real, local, anonymous (no-auth) plain-HTTP mock registry serving a
//! real `GET /v2/<name>/tags/list` response -- the same minimal mock
//! pattern `ociman_tls_verify.rs` already established, reused here
//! specifically to prove `--limit`/the real default-25 cap actually
//! reaches `cmd_search`'s own truncation, not just `Client::
//! list_tags`'s own already-thoroughly-unit-tested pagination (see
//! `crates/oci-registry/src/client.rs`'s own `list_tags_follows_link_
//! header_pagination_and_filters_bad_entries`).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::Command;
use std::thread;

use oci_tools_tests::bin_path;

fn ociman(storage_root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
        .env_remove("OCI_TOOLS_LOG")
        .args(args)
        .output()
        .expect("failed to spawn ociman")
}

/// Same minimal mock-registry shape `ociman_tls_verify.rs` already
/// established: one fixed route table, no auth challenge at all (a
/// plain `200` is returned directly, so `Client::request_with_auth`
/// never needs a token round trip).
struct MockRegistry {
    addr: std::net::SocketAddr,
}

impl MockRegistry {
    fn start(routes: HashMap<String, (&'static str, Vec<u8>)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            while let Ok((stream, _)) = listener.accept() {
                Self::handle(stream, &routes);
            }
        });
        MockRegistry { addr }
    }

    fn handle(mut stream: TcpStream, routes: &HashMap<String, (&'static str, Vec<u8>)>) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request_line = String::new();
        reader.read_line(&mut request_line).unwrap();
        let path = request_line
            .split_whitespace()
            .nth(1)
            .unwrap_or("")
            .to_string();
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line.trim().is_empty() {
                break;
            }
        }

        match routes.get(&path) {
            Some((content_type, body)) => {
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(header.as_bytes()).unwrap();
                stream.write_all(body).unwrap();
            }
            None => {
                let resp =
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                stream.write_all(resp.as_bytes()).unwrap();
            }
        }
    }
}

/// A single-page `/v2/testrepo/tags/list` response listing `count`
/// real, distinct tags (`tag-0`, `tag-1`, ...) -- no `Link` header, so
/// `Client::list_tags` never needs to paginate at all.
fn start_mock_with_n_tags(count: usize) -> MockRegistry {
    let tags: Vec<String> = (0..count).map(|i| format!("tag-{i}")).collect();
    let body =
        serde_json::to_vec(&serde_json::json!({ "name": "testrepo", "tags": tags })).unwrap();
    let mut routes = HashMap::new();
    routes.insert(
        "/v2/testrepo/tags/list".to_string(),
        ("application/json", body),
    );
    MockRegistry::start(routes)
}

/// No `--limit` at all: capped at the real, hardcoded default of 25
/// (`searchMaxQueries`), matching real `podman search --list-tags`'s
/// own identical default exactly -- even though the repository itself
/// has far more tags than that.
#[test]
fn search_without_limit_caps_at_the_real_default_of_25() {
    let mock = start_mock_with_n_tags(100);
    let storage_dir = tempfile::tempdir().unwrap();
    let search = ociman(
        storage_dir.path(),
        &[
            "search",
            "--list-tags",
            "--tls-verify=false",
            &format!("{}/testrepo", mock.addr),
        ],
    );
    assert!(
        search.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&search.stderr)
    );
    let stdout = String::from_utf8_lossy(&search.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 26, "header + 25 tags: {lines:?}");
    assert_eq!(lines[0], "NAME\tTAG");
}

/// `--limit 3` overrides the real default of 25.
#[test]
fn search_limit_overrides_the_real_default() {
    let mock = start_mock_with_n_tags(100);
    let storage_dir = tempfile::tempdir().unwrap();
    let search = ociman(
        storage_dir.path(),
        &[
            "search",
            "--list-tags",
            "--tls-verify=false",
            "--limit",
            "3",
            &format!("{}/testrepo", mock.addr),
        ],
    );
    assert!(
        search.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&search.stderr)
    );
    let stdout = String::from_utf8_lossy(&search.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 4, "header + 3 tags: {lines:?}");
}

/// `--limit` larger than the real tag count is a real no-op --
/// `Vec::truncate` never pads/repeats.
#[test]
fn search_limit_larger_than_available_tags_returns_them_all() {
    let mock = start_mock_with_n_tags(5);
    let storage_dir = tempfile::tempdir().unwrap();
    let search = ociman(
        storage_dir.path(),
        &[
            "search",
            "--list-tags",
            "--tls-verify=false",
            "--limit",
            "1000",
            &format!("{}/testrepo", mock.addr),
        ],
    );
    assert!(
        search.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&search.stderr)
    );
    let stdout = String::from_utf8_lossy(&search.stdout);
    assert_eq!(stdout.lines().count(), 6, "header + 5 tags: {stdout:?}");
}

/// `--limit 0` explicitly is the same real "unset" sentinel real
/// podman itself uses -- identical to not passing `--limit` at all.
#[test]
fn search_explicit_limit_zero_behaves_like_the_default() {
    let mock = start_mock_with_n_tags(100);
    let storage_dir = tempfile::tempdir().unwrap();
    let search = ociman(
        storage_dir.path(),
        &[
            "search",
            "--list-tags",
            "--tls-verify=false",
            "--limit",
            "0",
            &format!("{}/testrepo", mock.addr),
        ],
    );
    assert!(search.status.success());
    let stdout = String::from_utf8_lossy(&search.stdout);
    assert_eq!(stdout.lines().count(), 26, "header + 25 tags: {stdout:?}");
}

/// A negative `--limit` is a genuine, checked-directly real-podman
/// quirk (see `Command::Search::limit`'s own doc comment): zero
/// results, and *no header at all* in plain-text mode -- real
/// podman's own identical "nothing printed for zero results" behavior
/// -- but a real, valid, empty `[]` for `--json` (this project's own
/// established "`--json` is always valid JSON" convention, a
/// deliberate divergence from real podman's own literally-empty-
/// stdout-even-for-json quirk).
#[test]
fn search_negative_limit_yields_zero_results_no_header_in_plain_mode_but_valid_json_array() {
    let mock = start_mock_with_n_tags(10);
    let storage_dir = tempfile::tempdir().unwrap();

    let plain = ociman(
        storage_dir.path(),
        &[
            "search",
            "--list-tags",
            "--tls-verify=false",
            "--limit",
            "-1",
            &format!("{}/testrepo", mock.addr),
        ],
    );
    assert!(
        plain.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&plain.stderr)
    );
    assert!(
        plain.stdout.is_empty(),
        "zero results should print nothing at all, not even the header: {:?}",
        String::from_utf8_lossy(&plain.stdout)
    );

    let json = ociman(
        storage_dir.path(),
        &[
            "search",
            "--list-tags",
            "--tls-verify=false",
            "--limit",
            "-1",
            "--json",
            &format!("{}/testrepo", mock.addr),
        ],
    );
    assert!(json.status.success());
    let tags: serde_json::Value = serde_json::from_slice(&json.stdout)
        .unwrap_or_else(|e| panic!("--json output was not valid JSON: {e}"));
    assert_eq!(tags, serde_json::json!([]));
}

/// `--json` reports exactly the (truncated) tag list as a plain array
/// of strings.
#[test]
fn search_json_reports_the_truncated_tag_array() {
    let mock = start_mock_with_n_tags(10);
    let storage_dir = tempfile::tempdir().unwrap();
    let search = ociman(
        storage_dir.path(),
        &[
            "search",
            "--list-tags",
            "--tls-verify=false",
            "--limit",
            "2",
            "--json",
            &format!("{}/testrepo", mock.addr),
        ],
    );
    assert!(
        search.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&search.stderr)
    );
    let tags: serde_json::Value = serde_json::from_slice(&search.stdout)
        .unwrap_or_else(|e| panic!("--json output was not valid JSON: {e}"));
    assert_eq!(tags, serde_json::json!(["tag-0", "tag-1"]));
}
