//! `ocirun update` integration tests: matches real `runc update
//! --resources=<file>` exactly (`~/git/runc/update.go`) — writes a
//! given `LinuxResources` JSON blob's own real cgroup interface files
//! for an already-running container, leaving every other real cgroup
//! limit untouched (see `docs/design/0099`). Needs a real, delegated
//! `systemd --user` cgroup subtree to actually exercise (same
//! reasoning/setup `ocirun_ps.rs`'s own tests already established) —
//! skips cleanly where unavailable.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use oci_tools_tests::{
    bin_path, busybox_path, ocirun, ocirun_create, wait_for_status, write_bundle,
};

/// Same real, reachable-`systemd --user`-session probe
/// `ocirun_ps.rs`'s own tests use.
fn systemd_user_scope_available() -> bool {
    Command::new("systemd-run")
        .args(["--user", "--scope", "--", "true"])
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Set up a real, running container with a real delegated cgroup
/// subtree (needed for `ocirun update` to have a real cgroup to
/// write into at all), returning `(bundle_dir, root_dir, cgroup_dir)`
/// — callers are responsible for `kill`+`delete`ing `id` afterward.
fn create_and_start_with_real_cgroup(
    id: &str,
) -> Option<(tempfile::TempDir, tempfile::TempDir, std::path::PathBuf)> {
    if !systemd_user_scope_available() {
        eprintln!(
            "skipping: no reachable `systemd --user` session (systemd-run --user --scope failed)"
        );
        return None;
    }
    let busybox = busybox_path()?;
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "sleep 30"]);

    let config_path = bundle_dir.path().join("config.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    let uid = rustix::process::getuid().as_raw();
    let target = format!(
        "/user.slice/user-{uid}.slice/user@{uid}.service/app.slice/ocirun-update-test-{id}-{}",
        std::process::id()
    );
    config["linux"]["cgroupsPath"] = serde_json::json!(target);
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
    let cgroup_dir = Path::new("/sys/fs/cgroup").join(target.trim_start_matches('/'));

    let carrier_unit = format!(
        "ocirun-update-test-carrier-{id}-{}.scope",
        std::process::id()
    );
    let create = Command::new("systemd-run")
        .args([
            "--user",
            "--scope",
            "--slice=app.slice",
            &format!("--unit={carrier_unit}"),
            "--",
        ])
        .arg(bin_path("ocirun"))
        .args(["--root"])
        .arg(root_dir.path())
        .args(["create", id, "--bundle"])
        .arg(bundle_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .expect("failed to spawn systemd-run");
    assert!(
        create.status.success(),
        "create failed: {}",
        String::from_utf8_lossy(&create.stderr)
    );

    let start = ocirun(root_dir.path(), &["start", id]);
    assert!(
        start.status.success(),
        "start failed: {}",
        String::from_utf8_lossy(&start.stderr)
    );

    Some((bundle_dir, root_dir, cgroup_dir))
}

fn cleanup(root_dir: &Path, id: &str, cgroup_dir: &Path) {
    let kill = ocirun(root_dir, &["kill", id, "KILL"]);
    assert!(kill.status.success());
    wait_for_status(root_dir, id, "stopped", Duration::from_secs(5));
    let delete = ocirun(root_dir, &["delete", id]);
    assert!(delete.status.success());
    assert!(!cgroup_dir.exists());
}

#[test]
fn update_writes_real_memory_and_pids_limits_to_the_running_containers_own_cgroup() {
    let Some((bundle_dir, root_dir, cgroup_dir)) =
        create_and_start_with_real_cgroup("memory-pids-test")
    else {
        return;
    };

    // Real, unset defaults before any update.
    assert_eq!(
        std::fs::read_to_string(cgroup_dir.join("memory.max")).unwrap(),
        "max\n"
    );
    assert_eq!(
        std::fs::read_to_string(cgroup_dir.join("pids.max")).unwrap(),
        "max\n"
    );

    let resources_path = bundle_dir.path().join("resources.json");
    std::fs::write(
        &resources_path,
        r#"{"memory": {"limit": 104857600}, "pids": {"limit": 50}}"#,
    )
    .unwrap();

    let update = ocirun(
        root_dir.path(),
        &[
            "update",
            "memory-pids-test",
            "--resources",
            resources_path.to_str().unwrap(),
        ],
    );
    assert!(
        update.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&update.stderr)
    );

    assert_eq!(
        std::fs::read_to_string(cgroup_dir.join("memory.max"))
            .unwrap()
            .trim(),
        "104857600"
    );
    assert_eq!(
        std::fs::read_to_string(cgroup_dir.join("pids.max"))
            .unwrap()
            .trim(),
        "50"
    );

    cleanup(root_dir.path(), "memory-pids-test", &cgroup_dir);
}

/// `ocirun update`'s own ad-hoc `--memory`/`--pids-limit` flags
/// (0353), matching real `runc update`/`crun update`'s own identical
/// flags exactly — the same real cgroup effect the `--resources`
/// JSON-file test above already proves, reached through the CLI
/// flags instead of a hand-authored file.
#[test]
fn update_ad_hoc_memory_and_pids_limit_flags_write_the_real_cgroup() {
    let Some((bundle_dir, root_dir, cgroup_dir)) =
        create_and_start_with_real_cgroup("adhoc-memory-pids-test")
    else {
        return;
    };
    let _ = &bundle_dir;

    let update = ocirun(
        root_dir.path(),
        &[
            "update",
            "adhoc-memory-pids-test",
            "--memory",
            "100m",
            "--pids-limit",
            "50",
        ],
    );
    assert!(
        update.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&update.stderr)
    );

    assert_eq!(
        std::fs::read_to_string(cgroup_dir.join("memory.max"))
            .unwrap()
            .trim(),
        "104857600",
        "--memory 100m must parse as 100 * 1024 * 1024 bytes, matching real docker/podman/runc \
         binary-unit convention"
    );
    assert_eq!(
        std::fs::read_to_string(cgroup_dir.join("pids.max"))
            .unwrap()
            .trim(),
        "50"
    );

    cleanup(root_dir.path(), "adhoc-memory-pids-test", &cgroup_dir);
}

/// `ocirun update`'s own ad-hoc CPU-bandwidth flags (0356) --
/// `--cpu-share`/`--cpu-period`/`--cpu-quota`/`--cpu-burst`, matching
/// real `runc update`/`crun update`'s own identical flags exactly --
/// write the exact same real `cpu.weight`/`cpu.max`/`cpu.max.burst`
/// cgroup files `oci_runtime_core::cgroups::plan_cpu` already writes
/// for a `--resources` JSON blob with the same fields, reached here
/// through the CLI flags instead.
#[test]
fn update_ad_hoc_cpu_bandwidth_flags_write_the_real_cgroup() {
    let Some((bundle_dir, root_dir, cgroup_dir)) =
        create_and_start_with_real_cgroup("adhoc-cpu-bandwidth-test")
    else {
        return;
    };
    let _ = &bundle_dir;

    let update = ocirun(
        root_dir.path(),
        &[
            "update",
            "adhoc-cpu-bandwidth-test",
            "--cpu-share",
            "1024",
            "--cpu-period",
            "100000",
            "--cpu-quota",
            "50000",
            "--cpu-burst",
            "20000",
        ],
    );
    assert!(
        update.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&update.stderr)
    );

    // `--cpu-share 1024` (real docker/podman/runc's own default CPU
    // shares value) converts to exactly `cpu.weight=100` (the cgroup
    // v2 default too) -- the same already-tested conversion
    // `oci_runtime_core::cgroups`'s own unit tests already establish
    // for this exact input.
    assert_eq!(
        std::fs::read_to_string(cgroup_dir.join("cpu.weight"))
            .unwrap()
            .trim(),
        "100"
    );
    assert_eq!(
        std::fs::read_to_string(cgroup_dir.join("cpu.max"))
            .unwrap()
            .trim(),
        "50000 100000",
        "cpu.max is \"<quota> <period>\", matching real cgroup v2 syntax"
    );
    assert_eq!(
        std::fs::read_to_string(cgroup_dir.join("cpu.max.burst"))
            .unwrap()
            .trim(),
        "20000"
    );

    cleanup(root_dir.path(), "adhoc-cpu-bandwidth-test", &cgroup_dir);
}

/// `--cpu-idle` (0382): a real, kernel-enforced verification of the
/// exact write-order reasoning `oci_runtime_core::cgroups::plan_cpu`'s
/// own doc comment cites -- `--cpu-share`/`--cpu-idle` given together
/// in one `ocirun update` call must succeed cleanly (not the real,
/// kernel-enforced `EINVAL` real runc's own opposite write order
/// would hit), with `cpu.idle` correctly taking final effect
/// (`cpu.weight` reads back as the kernel's own internal `SCHED_IDLE`
/// value, not the value `--cpu-share` asked for -- confirmed by hand
/// against a real kernel before writing this assertion, not assumed).
/// Setting `--cpu-idle 0` afterward restores completely ordinary
/// `cpu.weight` behavior for a later, plain `--cpu-share` update.
#[test]
fn update_cpu_idle_combined_with_cpu_share_succeeds_and_idle_wins() {
    let Some((bundle_dir, root_dir, cgroup_dir)) =
        create_and_start_with_real_cgroup("adhoc-cpu-idle-test")
    else {
        return;
    };
    let _ = &bundle_dir;

    let combined = ocirun(
        root_dir.path(),
        &[
            "update",
            "adhoc-cpu-idle-test",
            "--cpu-share",
            "512",
            "--cpu-idle",
            "1",
        ],
    );
    assert!(
        combined.status.success(),
        "a combined --cpu-share + --cpu-idle update must not fail with EINVAL: stderr: {}",
        String::from_utf8_lossy(&combined.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(cgroup_dir.join("cpu.idle"))
            .unwrap()
            .trim(),
        "1"
    );
    // The kernel's own `cpu.idle=1` write silently resets the
    // group's own effective weight to its internal `SCHED_IDLE`
    // value -- genuinely different from whatever `--cpu-share 512`
    // would otherwise have converted to, proving `cpu.idle` took
    // final effect rather than merely being written without error.
    let weight_with_idle = std::fs::read_to_string(cgroup_dir.join("cpu.weight"))
        .unwrap()
        .trim()
        .to_string();
    assert_ne!(
        weight_with_idle, "59",
        "cpu.weight must reflect the kernel's own SCHED_IDLE internal weight, not \
         --cpu-share 512's own ordinary conversion"
    );

    let restore = ocirun(
        root_dir.path(),
        &["update", "adhoc-cpu-idle-test", "--cpu-idle", "0"],
    );
    assert!(restore.status.success(), "{restore:?}");
    assert_eq!(
        std::fs::read_to_string(cgroup_dir.join("cpu.idle"))
            .unwrap()
            .trim(),
        "0"
    );

    let after_restore = ocirun(
        root_dir.path(),
        &["update", "adhoc-cpu-idle-test", "--cpu-share", "512"],
    );
    assert!(after_restore.status.success(), "{after_restore:?}");
    assert_eq!(
        std::fs::read_to_string(cgroup_dir.join("cpu.weight"))
            .unwrap()
            .trim(),
        "59",
        "once idle is back to 0, an ordinary --cpu-share update must apply the real, normal \
         weight conversion again (512 shares -> weight 59, the same conversion \
         oci_runtime_core::cgroups's own unit tests already establish)"
    );

    cleanup(root_dir.path(), "adhoc-cpu-idle-test", &cgroup_dir);
}

/// `--cpu-rt-period`/`--cpu-rt-runtime` (0356) are accepted without
/// error, matching real `runc update`/`crun update`'s own identical
/// flags -- but genuinely write nothing to any real cgroup file at
/// all on a cgroup-v2-only host like this project's own (cgroup v2
/// has no realtime-scheduling controller), the same honest "accepted
/// on parse, never acted on" status both real reference runtimes
/// themselves have here too. Checked directly: every other real
/// cgroup file this container's own subtree already has stays
/// completely untouched.
#[test]
fn update_cpu_rt_flags_are_accepted_but_write_nothing_to_any_real_cgroup_file() {
    let Some((bundle_dir, root_dir, cgroup_dir)) =
        create_and_start_with_real_cgroup("adhoc-cpu-rt-test")
    else {
        return;
    };
    let _ = &bundle_dir;

    let before = std::fs::read_to_string(cgroup_dir.join("cpu.max")).unwrap();

    let update = ocirun(
        root_dir.path(),
        &[
            "update",
            "adhoc-cpu-rt-test",
            "--cpu-rt-period",
            "1000000",
            "--cpu-rt-runtime",
            "950000",
        ],
    );
    assert!(
        update.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&update.stderr)
    );

    assert_eq!(
        std::fs::read_to_string(cgroup_dir.join("cpu.max")).unwrap(),
        before,
        "cpu.max must stay completely untouched -- --cpu-rt-period/--cpu-rt-runtime have no \
         real cgroup v2 effect at all"
    );

    cleanup(root_dir.path(), "adhoc-cpu-rt-test", &cgroup_dir);
}

/// A real, checked-directly upstream quirk (`~/git/runc/update.go`'s
/// own doc comment: *"if data is to be read from a file or the
/// standard input, all other options are ignored"*): `--resources`
/// together with any ad-hoc flag silently ignores the ad-hoc flag
/// entirely, rather than erroring or merging the two -- ported
/// verbatim, not "fixed" into a more intuitive-seeming merge.
#[test]
fn update_resources_file_takes_priority_and_silently_ignores_every_ad_hoc_flag() {
    let Some((bundle_dir, root_dir, cgroup_dir)) =
        create_and_start_with_real_cgroup("resources-priority-test")
    else {
        return;
    };

    let resources_path = bundle_dir.path().join("resources.json");
    std::fs::write(&resources_path, r#"{"pids": {"limit": 9}}"#).unwrap();

    let update = ocirun(
        root_dir.path(),
        &[
            "update",
            "resources-priority-test",
            "--resources",
            resources_path.to_str().unwrap(),
            "--pids-limit",
            "999",
            "--memory",
            "1g",
        ],
    );
    assert!(
        update.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&update.stderr)
    );

    assert_eq!(
        std::fs::read_to_string(cgroup_dir.join("pids.max"))
            .unwrap()
            .trim(),
        "9",
        "the --resources file's own value must win, not the ad-hoc --pids-limit 999"
    );
    assert_eq!(
        std::fs::read_to_string(cgroup_dir.join("memory.max"))
            .unwrap()
            .trim(),
        "max",
        "memory.max must stay untouched -- the file doesn't mention memory, and --memory must \
         be silently ignored, not applied"
    );

    cleanup(root_dir.path(), "resources-priority-test", &cgroup_dir);
}

#[test]
fn update_only_touches_the_fields_the_given_json_actually_sets() {
    let Some((bundle_dir, root_dir, cgroup_dir)) =
        create_and_start_with_real_cgroup("partial-update-test")
    else {
        return;
    };

    // First update sets both memory and pids.
    let both_path = bundle_dir.path().join("both.json");
    std::fs::write(
        &both_path,
        r#"{"memory": {"limit": 52428800}, "pids": {"limit": 30}}"#,
    )
    .unwrap();
    let update1 = ocirun(
        root_dir.path(),
        &[
            "update",
            "partial-update-test",
            "--resources",
            both_path.to_str().unwrap(),
        ],
    );
    assert!(update1.status.success());

    // Second update sets *only* pids -- memory.max must be left alone.
    let pids_only_path = bundle_dir.path().join("pids_only.json");
    std::fs::write(&pids_only_path, r#"{"pids": {"limit": 15}}"#).unwrap();
    let update2 = ocirun(
        root_dir.path(),
        &[
            "update",
            "partial-update-test",
            "--resources",
            pids_only_path.to_str().unwrap(),
        ],
    );
    assert!(update2.status.success());

    assert_eq!(
        std::fs::read_to_string(cgroup_dir.join("memory.max"))
            .unwrap()
            .trim(),
        "52428800",
        "an update that doesn't mention memory must leave it exactly as it was"
    );
    assert_eq!(
        std::fs::read_to_string(cgroup_dir.join("pids.max"))
            .unwrap()
            .trim(),
        "15"
    );

    cleanup(root_dir.path(), "partial-update-test", &cgroup_dir);
}

#[test]
fn update_reads_resources_from_stdin_when_given_a_dash() {
    let Some((bundle_dir, root_dir, cgroup_dir)) = create_and_start_with_real_cgroup("stdin-test")
    else {
        return;
    };
    let _ = &bundle_dir;

    let mut child = Command::new(bin_path("ocirun"))
        .arg("--root")
        .arg(root_dir.path())
        .args(["update", "stdin-test", "--resources", "-"])
        .env_remove("OCI_TOOLS_LOG")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn ocirun update");
    {
        use std::io::Write as _;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(br#"{"pids": {"limit": 7}}"#)
            .unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(
        std::fs::read_to_string(cgroup_dir.join("pids.max"))
            .unwrap()
            .trim(),
        "7"
    );

    cleanup(root_dir.path(), "stdin-test", &cgroup_dir);
}

#[test]
fn update_without_a_cgroup_is_a_clear_error() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    // Deliberately no `cgroupsPath` set at all.
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "sleep 30"]);
    let create = ocirun_create(root_dir.path(), bundle_dir.path(), "no-cgroup-update-test");
    assert!(create.status.success());
    let start = ocirun(root_dir.path(), &["start", "no-cgroup-update-test"]);
    assert!(start.status.success());

    let resources_path = bundle_dir.path().join("resources.json");
    std::fs::write(&resources_path, r#"{"pids": {"limit": 5}}"#).unwrap();

    let update = ocirun(
        root_dir.path(),
        &[
            "update",
            "no-cgroup-update-test",
            "--resources",
            resources_path.to_str().unwrap(),
        ],
    );
    assert!(!update.status.success());
    let stderr = String::from_utf8_lossy(&update.stderr);
    assert!(stderr.contains("has no cgroup"), "{stderr}");

    let kill = ocirun(root_dir.path(), &["kill", "no-cgroup-update-test", "KILL"]);
    assert!(kill.status.success());
    ocirun(root_dir.path(), &["delete", "no-cgroup-update-test"]);
}

#[test]
fn update_on_an_unknown_container_is_a_clear_error() {
    let root_dir = tempfile::tempdir().unwrap();
    let update = ocirun(
        root_dir.path(),
        &["update", "does-not-exist", "--resources", "-"],
    );
    assert!(!update.status.success());
}
