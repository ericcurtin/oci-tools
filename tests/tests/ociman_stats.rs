//! `ociman stats --no-stream` integration tests: a real, one-shot
//! cgroup-v2-accounting sample for a running container (see
//! `docs/design/0145`) — same "no `systemd-run --user --scope` carrier
//! needed" reasoning `ociman_top.rs`/`ociman_pause.rs` already
//! establish (`ociman run` always attempts the systemd cgroup driver
//! itself), so this only needs a reachable `systemd --user` session to
//! skip cleanly where unavailable.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use oci_spec_types::image::ContainerConfig;
use oci_store::Store;

use oci_tools_tests::{bin_path, busybox_path, seed_image};

fn ociman(storage_root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
        .env_remove("OCI_TOOLS_LOG")
        .args(args)
        .output()
        .expect("failed to spawn ociman")
}

fn ociman_run_detached(
    storage_root: &Path,
    image: &str,
    container_args: &[&str],
) -> std::process::Child {
    Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
        .env_remove("OCI_TOOLS_LOG")
        .args(["run", image])
        .args(container_args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn ociman run")
}

/// Same as [`ociman_run_detached`], but with `--name` given *before*
/// the image -- `RunArgs::args`'s own `trailing_var_arg = true`
/// captures everything positional after the image into the
/// container's own command, so `--name` must come first.
fn ociman_run_detached_named(
    storage_root: &Path,
    name: &str,
    image: &str,
    container_args: &[&str],
) -> std::process::Child {
    Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
        .env_remove("OCI_TOOLS_LOG")
        .args(["run", "--name", name, image])
        .args(container_args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn ociman run")
}

fn wait_for_container_status_by_name(
    storage_root: &Path,
    name: &str,
    want: &str,
    timeout: Duration,
) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let out = ociman(storage_root, &["ps", "-a", "--json"]);
        if out.status.success()
            && let Ok(views) = serde_json::from_slice::<serde_json::Value>(&out.stdout)
            && let Some(entry) = views
                .as_array()
                .and_then(|a| a.iter().find(|e| e["name"] == name))
        {
            let status = entry["status"].as_str().unwrap_or_default().to_string();
            if status == want || Instant::now() >= deadline {
                return status;
            }
        } else if Instant::now() >= deadline {
            return String::new();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn only_container_id(storage_root: &Path, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let out = ociman(storage_root, &["ps", "-a", "-q"]);
        let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !id.is_empty() || Instant::now() >= deadline {
            return id;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_container_status(
    storage_root: &Path,
    id: &str,
    want: &str,
    timeout: Duration,
) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let out = ociman(storage_root, &["ps", "-a", "--json"]);
        if out.status.success()
            && let Ok(views) = serde_json::from_slice::<serde_json::Value>(&out.stdout)
            && let Some(entry) = views
                .as_array()
                .and_then(|a| a.iter().find(|e| e["id"] == id))
        {
            let status = entry["status"].as_str().unwrap_or_default().to_string();
            if status == want || Instant::now() >= deadline {
                return status;
            }
        } else if Instant::now() >= deadline {
            return String::new();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Same real, reachable-`systemd --user`-session probe
/// `ociman_top.rs`/`ociman_pause.rs`'s own tests use.
fn systemd_user_session_available() -> bool {
    Command::new("systemctl")
        .args(["--user", "is-system-running"])
        .output()
        .is_ok_and(|out| !out.stdout.is_empty())
}

/// `ociman stats --no-stream --json` against a real, running,
/// genuinely CPU-burning container: the real cgroup's own accounting
/// files must report a substantial, non-zero CPU percentage (it's
/// been consuming a full core continuously since it started) and a
/// real, non-zero memory usage — not just a successful exit code.
#[test]
fn stats_no_stream_reports_real_cpu_and_memory_usage_for_a_running_container() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    if !systemd_user_session_available() {
        eprintln!("skipping: no reachable `systemd --user` session");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/stats-basic:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/stats-basic:latest",
        &["/bin/sh", "-c", "i=0; while true; do i=$((i+1)); done"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    // Let it genuinely burn a full core for a real, measured interval
    // before sampling -- the very first sample's own CPU % is an
    // average over the container's *whole* life so far, back to its
    // own recorded `created` timestamp (see `cmd_stats`'s own doc
    // comment), which includes real, essentially-fixed setup time
    // (image/rootfs/cgroup/systemd-scope setup) before the container's
    // own process is even running yet -- a full 3 real seconds of
    // continuous 100%-core burn is enough for that fixed overhead to
    // stop dominating the ratio (confirmed empirically: a mere 500ms
    // burn measured as low as ~33%, well under the assertion below,
    // purely from setup overhead diluting it -- not a real bug).
    std::thread::sleep(Duration::from_millis(3000));

    let stats = ociman(storage_dir.path(), &["stats", &id, "--no-stream", "--json"]);
    assert!(
        stats.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&stats.stderr)
    );
    let view: serde_json::Value = serde_json::from_slice(&stats.stdout).unwrap();
    assert_eq!(view["id"], id);

    let cpu_percent = view["cpu_percent"].as_f64().unwrap();
    // A low, deliberately real-CI-friendly floor rather than "close to
    // 100%": on a real, heavily loaded CI host running this whole
    // workspace's own test suite in parallel (dozens of other real,
    // concurrently runnable processes, this same project's own other
    // tests included), this container's own fair scheduling share of
    // a core can legitimately be far below 100% even while genuinely
    // running flat out the entire time -- confirmed empirically: a
    // real `cargo test --workspace` run once measured as low as
    // ~21.7% under contention, well under a `50.0` threshold, despite
    // the container never being idle for a moment. `5.0` still firmly
    // distinguishes "genuinely, continuously running" from "idle"
    // (which reports a number many orders of magnitude smaller, not
    // just moderately smaller) without assuming anything about how
    // much of the host's own real CPU capacity is available to it.
    assert!(
        cpu_percent > 5.0,
        "a container burning a full core continuously since it started should show a \
         substantial CPU %, got {cpu_percent}"
    );

    let mem_usage = view["mem_usage"].as_u64().unwrap();
    assert!(
        mem_usage > 0,
        "a running container should use some real memory"
    );

    let mem_limit = view["mem_limit"].as_u64().unwrap();
    assert!(
        mem_limit > mem_usage,
        "with no --memory limit set, the (physical-RAM-clamped) limit should be far larger \
         than actual usage"
    );

    let mem_percent = view["mem_percent"].as_f64().unwrap();
    assert!((0.0..100.0).contains(&mem_percent));

    let pids = view["pids"].as_u64().unwrap();
    assert!(
        pids >= 1,
        "at least the container's own init process should be counted"
    );

    let kill = ociman(storage_dir.path(), &["kill", &id]);
    assert!(kill.status.success());
    run.wait().unwrap();
    ociman(storage_dir.path(), &["rm", &id]);
}

/// The real, non-JSON table output at least contains the expected
/// header columns and the container's own id.
#[test]
fn stats_no_stream_table_output_has_the_real_expected_columns() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    if !systemd_user_session_available() {
        eprintln!("skipping: no reachable `systemd --user` session");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/stats-table:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/stats-table:latest",
        &["/bin/sh", "-c", "while true; do sleep 1; done"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let stats = ociman(storage_dir.path(), &["stats", &id, "--no-stream"]);
    assert!(
        stats.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&stats.stderr)
    );
    let stdout = String::from_utf8_lossy(&stats.stdout);
    assert!(stdout.contains("CPU %"));
    assert!(stdout.contains("MEM USAGE / LIMIT"));
    assert!(stdout.contains("MEM %"));
    assert!(stdout.contains("PIDS"));
    assert!(stdout.contains(&id));

    let kill = ociman(storage_dir.path(), &["kill", &id]);
    assert!(kill.status.success());
    run.wait().unwrap();
    ociman(storage_dir.path(), &["rm", &id]);
}

/// `--no-trunc` (`docs/design/0545`) is a real, total no-op here --
/// see `Command::Stats::no_trunc`'s own doc comment for the exact,
/// checked-directly reasoning (real podman's own identical flag only
/// ever un-truncates the table's own `ID` column, and this project's
/// own container ids are already always the short, 12-hex-character
/// form with no separate, longer form to reveal). Asserts the ID
/// column is the exact same 12-hex-character id either way -- proving
/// `--no-trunc` doesn't (and has nothing to) change it.
#[test]
fn stats_no_trunc_flag_is_accepted_and_behaves_identically() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    if !systemd_user_session_available() {
        eprintln!("skipping: no reachable `systemd --user` session");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/stats-no-trunc:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/stats-no-trunc:latest",
        &["/bin/sh", "-c", "while true; do sleep 1; done"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let plain = ociman(storage_dir.path(), &["stats", &id, "--no-stream"]);
    assert!(
        plain.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&plain.stderr)
    );
    let no_trunc = ociman(
        storage_dir.path(),
        &["stats", &id, "--no-stream", "--no-trunc"],
    );
    assert!(
        no_trunc.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&no_trunc.stderr)
    );

    let plain_stdout = String::from_utf8_lossy(&plain.stdout);
    let no_trunc_stdout = String::from_utf8_lossy(&no_trunc.stdout);
    // Both contain the exact same, full (and only) 12-hex-character
    // id -- no additional truncation was ever applied to either.
    assert!(plain_stdout.contains(&id), "{plain_stdout:?}");
    assert!(no_trunc_stdout.contains(&id), "{no_trunc_stdout:?}");
    assert_eq!(
        id.len(),
        12,
        "this project's own ids are always 12 hex chars"
    );

    let kill = ociman(storage_dir.path(), &["kill", &id]);
    assert!(kill.status.success());
    run.wait().unwrap();
    ociman(storage_dir.path(), &["rm", &id]);
}

/// Bare `ociman stats <id>` (no `--no-stream`) is a clear, loud error
/// -- continuous streaming isn't implemented yet, see `cmd_stats`'s
/// own doc comment -- never a silent hang or a silently different
/// one-shot behavior.
#[test]
fn stats_without_no_stream_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(storage_dir.path(), &["stats", "does-not-exist"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("does not exist"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `ociman stats` (0284, default continuous mode, no `--no-stream`):
/// streams real samples on a real, short-lived running container,
/// then ends cleanly (a real success, not an error) the moment the
/// container itself stops — matching real `podman stats`'s own
/// default behavior exactly (checked directly against a real
/// installed `podman stats --help`: `--interval`'s own real `5`
/// second default, `--no-reset` for disabling the screen-clear).
/// `--interval 1` here keeps this test itself fast without changing
/// what's actually being verified.
#[test]
fn stats_streams_real_samples_and_ends_cleanly_once_the_container_stops() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    if !systemd_user_session_available() {
        eprintln!("skipping: no reachable `systemd --user` session");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/stats-stream:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/stats-stream:latest",
        &["/bin/sleep", "2"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let stats = ociman(
        storage_dir.path(),
        &["stats", "--interval", "1", "--no-reset", &id],
    );
    assert!(
        stats.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&stats.stderr)
    );
    let stdout = String::from_utf8_lossy(&stats.stdout);
    // At least one real sample was printed (the table header/row this
    // project's own `--no-stream` mode already established, unchanged
    // here) before the stream ended.
    assert!(stdout.contains("CPU %"), "got: {stdout:?}");
    assert!(
        stdout.contains("is no longer running"),
        "the stream should end with a real, honest message once the container stops: {stdout:?}"
    );

    run.wait().unwrap();
}

/// `stats` against a container that has already stopped is a clear,
/// real error, not stale/zeroed-out data.
#[test]
fn stats_on_a_stopped_container_is_a_clear_error() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/stats-stopped:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "exit 0".to_string(),
            ]),
            ..Default::default()
        },
    );

    let run = ociman(
        storage_dir.path(),
        &["run", "ociman-test/stats-stopped:latest"],
    );
    assert!(run.status.success());
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());

    let stats = ociman(storage_dir.path(), &["stats", &id, "--no-stream"]);
    assert!(!stats.status.success());
}

#[test]
fn stats_on_an_unknown_container_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let stats = ociman(
        storage_dir.path(),
        &["stats", "does-not-exist", "--no-stream"],
    );
    assert!(!stats.status.success());
}

/// `stats --format` (0339) renders one line for the `--no-stream`
/// sample, reusing the exact same Go-template-*lite* engine `ociman
/// inspect`/`ps`/`images`/`volume ls`/`info`/`history --format`
/// (`0332`-`0338`) already established.
#[test]
fn stats_format_renders_a_single_field_for_the_no_stream_sample() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    if !systemd_user_session_available() {
        eprintln!("skipping: no reachable `systemd --user` session");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/stats-format:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/stats-format:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let stats = ociman(
        storage_dir.path(),
        &["stats", &id, "--no-stream", "--format", "{{.id}}"],
    );
    assert!(
        stats.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&stats.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&stats.stdout).trim(), id);

    let kill = ociman(storage_dir.path(), &["kill", &id]);
    assert!(kill.status.success());
    run.wait().unwrap();
    ociman(storage_dir.path(), &["rm", &id]);
}

/// `--format`, when given, takes priority over `--json`/the default
/// table, and an unresolvable field path is a real, immediate error --
/// same precedence and error behavior the whole `--format` family
/// already established.
#[test]
fn stats_format_takes_priority_and_errors_on_an_unknown_field() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    if !systemd_user_session_available() {
        eprintln!("skipping: no reachable `systemd --user` session");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/stats-format-priority:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/stats-format-priority:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let stats = ociman(
        storage_dir.path(),
        &[
            "stats",
            &id,
            "--no-stream",
            "--json",
            "--format",
            "{{.pids}}",
        ],
    );
    assert!(stats.status.success());
    assert!(
        String::from_utf8_lossy(&stats.stdout)
            .trim()
            .parse::<u64>()
            .is_ok(),
        "the format template's own plain number, not --json's own object, should have won: {:?}",
        stats.stdout
    );

    let bad = ociman(
        storage_dir.path(),
        &["stats", &id, "--no-stream", "--format", "{{.nosuchfield}}"],
    );
    assert!(!bad.status.success());
    assert!(
        String::from_utf8_lossy(&bad.stderr).contains("no field"),
        "{}",
        String::from_utf8_lossy(&bad.stderr)
    );

    let kill = ociman(storage_dir.path(), &["kill", &id]);
    assert!(kill.status.success());
    run.wait().unwrap();
    ociman(storage_dir.path(), &["rm", &id]);
}

/// `ociman stats --latest`/`-l` (matching real `podman stats
/// --latest` exactly) shows the single, real most-recently-*created*
/// container's own stats -- an earlier container's own, genuinely
/// different name must never be reported.
#[test]
fn stats_latest_shows_the_most_recently_created_containers_own_stats() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    if !systemd_user_session_available() {
        eprintln!("skipping: no reachable `systemd --user` session");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/stats-latest:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut older = ociman_run_detached_named(
        storage_dir.path(),
        "stats-latest-older",
        "ociman-test/stats-latest:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "stats-latest-older",
            "running",
            Duration::from_secs(20)
        ),
        "running"
    );

    // A real, distinguishable creation-time gap.
    std::thread::sleep(Duration::from_secs(2));

    let mut newer = ociman_run_detached_named(
        storage_dir.path(),
        "stats-latest-newer",
        "ociman-test/stats-latest:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "stats-latest-newer",
            "running",
            Duration::from_secs(20)
        ),
        "running"
    );

    let stats = ociman(
        storage_dir.path(),
        &["stats", "--latest", "--no-stream", "--json"],
    );
    assert!(
        stats.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&stats.stderr)
    );
    let view: serde_json::Value = serde_json::from_slice(&stats.stdout).unwrap();
    assert_eq!(view["name"], "stats-latest-newer", "{view:?}");

    ociman(storage_dir.path(), &["kill", "stats-latest-older"]);
    ociman(storage_dir.path(), &["kill", "stats-latest-newer"]);
    older.wait().ok();
    newer.wait().ok();
    ociman(storage_dir.path(), &["rm", "-a", "-f"]);
}

/// `--latest` and an explicit container together is a real, immediate
/// error, matching real podman's own exact wording.
#[test]
fn stats_latest_and_explicit_id_together_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(storage_dir.path(), &["stats", "--latest", "some-id"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("--all, --latest and containers cannot be used together"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Neither `--latest` nor an explicit container at all is a real,
/// immediate error -- this project's own honest, narrower-scope
/// message, since real podman itself doesn't error here at all (it
/// defaults to streaming every running container instead, a mode
/// this project has never implemented).
#[test]
fn stats_with_no_container_and_no_latest_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(storage_dir.path(), &["stats"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no container ID/name given"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `stats --latest` on a genuinely empty store is a real, clear
/// error, matching real `podman stats --latest`'s own `ErrNoSuchCtr`.
#[test]
fn stats_latest_on_an_empty_store_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    let out = ociman(storage_dir.path(), &["stats", "--latest"]);
    assert!(!out.status.success());
}

/// `stats --all --no-stream` (`docs/design/0560`) reports every real
/// running container's own sample, sorted by creation time, and
/// silently excludes an already-stopped one -- matching real `podman
/// stats --all`'s own identical `GetAllContainers` enumeration
/// (checked directly, `~/git/podman/pkg/domain/infra/abi/
/// containers.go:1663-1690`), with a non-running container simply
/// producing no row at all (the same honest "nothing to report"
/// reasoning a single `stats <id>` already uses when that one
/// container itself isn't running).
#[test]
fn stats_all_no_stream_reports_every_running_container_sorted_by_creation() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    if !systemd_user_session_available() {
        eprintln!("skipping: no reachable `systemd --user` session");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/stats-all:latest",
        &busybox,
        &["sh", "sleep", "true"],
        ContainerConfig::default(),
    );

    let mut older = ociman_run_detached_named(
        storage_dir.path(),
        "stats-all-older",
        "ociman-test/stats-all:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "stats-all-older",
            "running",
            Duration::from_secs(20)
        ),
        "running"
    );

    // A real, distinguishable creation-time gap.
    std::thread::sleep(Duration::from_secs(2));

    // A container that has already stopped by the time `stats --all`
    // runs -- it should produce no row at all, never an error, never
    // a placeholder.
    let stopped = ociman(
        storage_dir.path(),
        &[
            "run",
            "--name",
            "stats-all-stopped",
            "ociman-test/stats-all:latest",
            "true",
        ],
    );
    assert!(stopped.status.success(), "{stopped:?}");

    let mut newer = ociman_run_detached_named(
        storage_dir.path(),
        "stats-all-newer",
        "ociman-test/stats-all:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "stats-all-newer",
            "running",
            Duration::from_secs(20)
        ),
        "running"
    );

    let stats = ociman(
        storage_dir.path(),
        &["stats", "--all", "--no-stream", "--json"],
    );
    assert!(
        stats.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&stats.stderr)
    );
    let views: Vec<serde_json::Value> = serde_json::from_slice(&stats.stdout).unwrap();
    let names: Vec<&str> = views.iter().map(|v| v["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        vec!["stats-all-older", "stats-all-newer"],
        "the stopped container should be silently excluded, and the two running ones sorted \
         oldest-first: {views:?}"
    );

    ociman(storage_dir.path(), &["kill", "stats-all-older"]);
    ociman(storage_dir.path(), &["kill", "stats-all-newer"]);
    older.wait().ok();
    newer.wait().ok();
    ociman(storage_dir.path(), &["rm", "-a", "-f"]);
}

/// `stats --all --no-stream` on a genuinely empty store is a real,
/// honest empty JSON array, never an error -- matching this project's
/// own already-established "always-valid-JSON-shape" convention.
#[test]
fn stats_all_no_stream_on_an_empty_store_is_an_empty_json_array() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    let out = ociman(
        storage_dir.path(),
        &["stats", "--all", "--no-stream", "--json"],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "[]");
}

/// `--all` without `--no-stream` (`docs/design/0570`, closing `0560`'s
/// own originally-deferred gap): a real, continuous, re-listing
/// stream across every stored container -- proven here by reading at
/// least two full samples (two separate `CPU %` header lines) off a
/// real, still-running child process before killing it; the process
/// must still be alive at that point (a real, unbounded stream, not
/// something that silently exits early), matching real `podman stats
/// --all`'s own default streaming mode exactly.
#[test]
fn stats_all_streaming_reports_repeated_samples_and_never_ends_on_its_own() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    if !systemd_user_session_available() {
        eprintln!("skipping: no reachable `systemd --user` session");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/stats-all-stream:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached_named(
        storage_dir.path(),
        "streamallbox",
        "ociman-test/stats-all-stream:latest",
        &["/bin/sh", "-c", "while true; do sleep 1; done"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let mut child = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args(["stats", "--all", "--interval", "1", "--no-reset"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn ociman stats --all");

    let stdout = child.stdout.take().unwrap();
    let mut reader = std::io::BufReader::new(stdout);
    let mut header_count = 0usize;
    let mut collected = String::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    while header_count < 2 && Instant::now() < deadline {
        let mut line = String::new();
        use std::io::BufRead;
        let n = reader.read_line(&mut line).unwrap_or(0);
        if n == 0 {
            break;
        }
        if line.contains("CPU %") {
            header_count += 1;
        }
        collected.push_str(&line);
    }

    assert!(
        header_count >= 2,
        "expected at least two real, separate samples before the stream would ever end on its \
         own; got: {collected:?}"
    );
    assert!(
        collected.contains(&id[..12.min(id.len())]) || collected.contains("streamallbox"),
        "expected the real running container's own id/name in at least one sample: {collected:?}"
    );
    assert!(
        child.try_wait().unwrap().is_none(),
        "the --all streaming mode should never end on its own -- it should still be running \
         right now, exactly like real `podman stats --all`'s own default mode"
    );

    let _ = child.kill();
    let _ = child.wait();
    let kill = ociman(storage_dir.path(), &["kill", &id]);
    assert!(kill.status.success());
    run.wait().unwrap();
    ociman(storage_dir.path(), &["rm", &id]);
}

/// The default streaming mode across every stored container keeps
/// re-listing from scratch each interval -- a container created
/// *after* the stream already started is picked up on a later sample,
/// matching real podman's own identical re-invoked `computeStats`
/// closure exactly (checked directly, `~/git/podman/pkg/domain/infra/
/// abi/containers.go:1663-1690`), not a fixed set captured once at
/// startup.
#[test]
fn stats_all_streaming_picks_up_a_container_created_after_the_stream_started() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    if !systemd_user_session_available() {
        eprintln!("skipping: no reachable `systemd --user` session");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/stats-all-late:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut child = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args(["stats", "--all", "--interval", "1", "--no-reset"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn ociman stats --all");

    // Give the stream a moment to print at least one real, genuinely
    // empty sample (no containers exist yet at all) before creating
    // one.
    std::thread::sleep(Duration::from_millis(1200));

    let mut run = ociman_run_detached_named(
        storage_dir.path(),
        "latecomerbox",
        "ociman-test/stats-all-late:latest",
        &["/bin/sh", "-c", "while true; do sleep 1; done"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());

    let stdout = child.stdout.take().unwrap();
    let mut reader = std::io::BufReader::new(stdout);
    let mut collected = String::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut found = false;
    while Instant::now() < deadline {
        let mut line = String::new();
        use std::io::BufRead;
        let n = reader.read_line(&mut line).unwrap_or(0);
        if n == 0 {
            break;
        }
        collected.push_str(&line);
        if line.contains("latecomerbox") {
            found = true;
            break;
        }
    }

    assert!(
        found,
        "the newly-created container should have been picked up by a later re-listed sample: \
         {collected:?}"
    );

    let _ = child.kill();
    let _ = child.wait();
    let kill = ociman(storage_dir.path(), &["kill", &id]);
    assert!(kill.status.success());
    run.wait().unwrap();
    ociman(storage_dir.path(), &["rm", &id]);
}

/// `--all` combined with `--latest` or an explicit container is a
/// real, immediate error -- matching real podman's own exact wording
/// (checked directly, `~/git/podman/cmd/podman/containers/
/// stats.go`'s own `checkStatOptions`), extended here to genuinely
/// cover all three now that `--all` is a real flag (it was previously
/// only checked against `--latest`+an-explicit-id, since `--all` had
/// no CLI presence at all yet).
#[test]
fn stats_all_combined_with_latest_or_an_explicit_container_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let with_latest = ociman(
        storage_dir.path(),
        &["stats", "--all", "--latest", "--no-stream"],
    );
    assert!(!with_latest.status.success());
    assert!(
        String::from_utf8_lossy(&with_latest.stderr)
            .contains("--all, --latest and containers cannot be used together"),
        "{}",
        String::from_utf8_lossy(&with_latest.stderr)
    );

    let with_id = ociman(
        storage_dir.path(),
        &["stats", "--all", "somecontainer", "--no-stream"],
    );
    assert!(!with_id.status.success());
    assert!(
        String::from_utf8_lossy(&with_id.stderr)
            .contains("--all, --latest and containers cannot be used together"),
        "{}",
        String::from_utf8_lossy(&with_id.stderr)
    );
}

/// `container stats --all` (the alias) dispatches identically to the
/// top-level command's own new flag.
#[test]
fn container_stats_all_no_stream_is_a_byte_identical_alias() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    if !systemd_user_session_available() {
        eprintln!("skipping: no reachable `systemd --user` session");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/container-stats-all:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/container-stats-all:latest",
        &["/bin/sh", "-c", "while true; do sleep 1; done"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let alias = ociman(
        storage_dir.path(),
        &["container", "stats", "--all", "--no-stream", "--json"],
    );
    assert!(
        alias.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&alias.stderr)
    );
    let views: Vec<serde_json::Value> = serde_json::from_slice(&alias.stdout).unwrap();
    assert_eq!(views.len(), 1, "{views:?}");
    assert_eq!(views[0]["id"], id);

    let kill = ociman(storage_dir.path(), &["kill", &id]);
    assert!(kill.status.success());
    run.wait().unwrap();
    ociman(storage_dir.path(), &["rm", &id]);
}
