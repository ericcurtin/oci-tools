//! `ociman info` integration tests (0163): a deliberately narrow first
//! slice of real `podman info`'s own huge report — see `Command::
//! Info`'s own doc comment for exactly why. Verifies the real, honest
//! values this project can actually report, and that `store.containers`/
//! `store.images` genuinely reflect real, current local storage state,
//! not just a fixed shape.

use std::path::Path;
use std::process::Command;

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

#[test]
fn info_plain_text_reports_the_real_expected_sections() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(storage_dir.path(), &["info"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with("Host:\n"), "got: {stdout:?}");
    assert!(stdout.contains("Hostname:"), "got: {stdout:?}");
    assert!(stdout.contains("Kernel:"), "got: {stdout:?}");
    assert!(stdout.contains("OS/Arch:        linux/"), "got: {stdout:?}");
    assert!(stdout.contains("CgroupVersion:  v2"), "got: {stdout:?}");
    assert!(stdout.contains("\nStore:\n"), "got: {stdout:?}");
    assert!(
        stdout.contains(&format!("GraphRoot:      {}", storage_dir.path().display())),
        "got: {stdout:?}"
    );
    assert!(stdout.contains("\nVersion:\n"), "got: {stdout:?}");
    assert!(
        stdout.contains(&format!("Version:        {}\n", env!("CARGO_PKG_VERSION"))),
        "got: {stdout:?}"
    );
}

#[test]
fn info_json_reports_real_sane_host_values() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(storage_dir.path(), &["info", "--json"]);
    assert!(out.status.success());
    let view: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();

    let host = &view["host"];
    assert!(!host["hostname"].as_str().unwrap().is_empty());
    assert!(!host["kernel"].as_str().unwrap().is_empty());
    assert!(host["os_arch"].as_str().unwrap().starts_with("linux/"));
    assert!(host["cpus"].as_u64().unwrap() >= 1);
    assert!(host["mem_total"].as_u64().unwrap() > 0);
    // A real, running host always has *some* free memory at the
    // instant this runs, but asserting a tight bound would be flaky --
    // just confirm it's a real, present, non-negative number alongside
    // a total that's at least as large.
    assert!(host["mem_free"].as_u64().unwrap() <= host["mem_total"].as_u64().unwrap());
    assert_eq!(host["cgroup_version"], "v2");

    assert_eq!(
        view["store"]["graph_root"],
        storage_dir.path().display().to_string()
    );
    assert_eq!(view["version"]["version"], env!("CARGO_PKG_VERSION"));
}

#[test]
fn info_container_and_image_counts_reflect_real_current_storage_state() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        storage_dir.path().join(".rootless-overlay-supported"),
        "false",
    )
    .unwrap();

    let before = ociman(storage_dir.path(), &["info", "--json"]);
    let before_view: serde_json::Value = serde_json::from_slice(&before.stdout).unwrap();
    assert_eq!(before_view["store"]["images"], 0);
    assert_eq!(before_view["store"]["containers"], 0);

    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/info-counts:latest",
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
        &["run", "ociman-test/info-counts:latest"],
    );
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );

    let after = ociman(storage_dir.path(), &["info", "--json"]);
    let after_view: serde_json::Value = serde_json::from_slice(&after.stdout).unwrap();
    assert_eq!(after_view["store"]["images"], 1);
    assert_eq!(after_view["store"]["containers"], 1);
}

/// `info --format` (0337) renders a single (possibly nested) field
/// with no surrounding JSON quoting -- reusing the exact same
/// Go-template-*lite* engine `ociman inspect`/`ps`/`images`/`volume
/// ls --format` (`0332`-`0335`) already established.
#[test]
fn info_format_renders_a_nested_field() {
    let storage_dir = tempfile::tempdir().unwrap();

    let format = ociman(
        storage_dir.path(),
        &["info", "--format", "{{.version.version}}"],
    );
    assert!(
        format.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&format.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&format.stdout).trim(),
        env!("CARGO_PKG_VERSION")
    );
}

/// Multiple placeholders (from different top-level sections) mixed
/// with literal text all get substituted, and `--format` takes
/// priority over `--json`/the default plain-text report when both are
/// given -- same precedence `inspect`/`ps`/`images`/`volume ls
/// --format` already established.
#[test]
fn info_format_supports_multiple_placeholders_and_takes_priority_over_json() {
    let storage_dir = tempfile::tempdir().unwrap();

    let format = ociman(
        storage_dir.path(),
        &[
            "info",
            "--json",
            "--format",
            "images={{.store.images}} containers={{.store.containers}}",
        ],
    );
    assert!(format.status.success());
    assert_eq!(
        String::from_utf8_lossy(&format.stdout).trim(),
        "images=0 containers=0",
        "the format template's own plain output, not --json's own object, should have won"
    );
}

/// An unresolvable field path is a real, immediate error, matching
/// real Go templates' own "can't evaluate field" failure for a typo'd
/// field name rather than a silent empty string.
#[test]
fn info_format_of_an_unknown_field_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();

    let bad = ociman(
        storage_dir.path(),
        &["info", "--format", "{{.nosuchfield}}"],
    );
    assert!(!bad.status.success());
    assert!(
        String::from_utf8_lossy(&bad.stderr).contains("no field"),
        "{}",
        String::from_utf8_lossy(&bad.stderr)
    );
}
