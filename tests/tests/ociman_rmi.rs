//! `ociman rmi` integration tests: removing an image's own tag/digest
//! pointer from local storage, matching real `docker rmi`/`podman
//! rmi` — including the "refuses while a container still depends on
//! it, unless `--force`" policy (see `docs/design/0102`). Same fully
//! offline seeded-image approach `ociman_run.rs`/`ociman_inspect.rs`
//! established.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use oci_spec_types::image::ContainerConfig;
use oci_store::{ImageRecord, Store};

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

fn wait_for_status(storage_root: &Path, id: &str, want: &str, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let out = ociman(storage_root, &["ps", "-a", "--json"]);
        if let Ok(views) = serde_json::from_slice::<serde_json::Value>(&out.stdout)
            && let Some(status) = views
                .as_array()
                .and_then(|entries| entries.iter().find(|e| e["id"] == id))
                .and_then(|e| e["status"].as_str())
        {
            if status == want || Instant::now() >= deadline {
                return status.to_string();
            }
        } else if Instant::now() >= deadline {
            return String::new();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn rmi_removes_a_real_image_no_longer_resolvable_afterward() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/rmi-basic:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    assert!(
        store
            .resolve_image("docker.io/ociman-test/rmi-basic:latest")
            .unwrap()
            .is_some()
    );

    let rmi = ociman(storage_dir.path(), &["rmi", "ociman-test/rmi-basic:latest"]);
    assert!(
        rmi.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rmi.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&rmi.stdout).trim(),
        "docker.io/ociman-test/rmi-basic:latest"
    );

    // The real, on-disk store no longer resolves it -- not just "the
    // CLI printed success", but the actual pointer is gone.
    assert!(
        store
            .resolve_image("docker.io/ociman-test/rmi-basic:latest")
            .unwrap()
            .is_none()
    );

    // And `ociman images`/`inspect` agree.
    let images = ociman(storage_dir.path(), &["images", "--json"]);
    let views: serde_json::Value = serde_json::from_slice(&images.stdout).unwrap();
    assert!(views.as_array().unwrap().is_empty(), "{views:?}");

    let inspect = ociman(
        storage_dir.path(),
        &["inspect", "ociman-test/rmi-basic:latest"],
    );
    assert!(!inspect.status.success());
}

#[test]
fn rmi_of_an_unknown_reference_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let rmi = ociman(
        storage_dir.path(),
        &["rmi", "ociman-test/never-pulled:latest"],
    );
    assert!(!rmi.status.success());
    assert!(
        String::from_utf8_lossy(&rmi.stderr).contains("no such image"),
        "{}",
        String::from_utf8_lossy(&rmi.stderr)
    );
}

/// Real docker/podman rule, checked directly: `rmi` resolves by image
/// ID too, not just a tag reference -- the exact short digest `ociman
/// images`' own `DIGEST` column already prints. A single-tagged image
/// removed this way needs no `--force` at all (no ambiguity: exactly
/// one tag to remove).
#[test]
fn rmi_removes_a_real_image_by_its_own_short_id() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/rmi-by-id:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let record = store
        .resolve_image("docker.io/ociman-test/rmi-by-id:latest")
        .unwrap()
        .unwrap();
    let short_id = record.manifest_digest.hex()[..12].to_string();

    let rmi = ociman(storage_dir.path(), &["rmi", &short_id]);
    assert!(
        rmi.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rmi.stderr)
    );
    assert!(
        store
            .resolve_image("docker.io/ociman-test/rmi-by-id:latest")
            .unwrap()
            .is_none()
    );
}

/// Real `podman rmi`'s own exact policy, checked directly against a
/// real installed `podman` before implementing this: removing *by ID*
/// when more than one tag points at that exact image refuses without
/// `--force` (listing every tag in the error), and removes all of them
/// with it.
#[test]
fn rmi_by_id_with_multiple_tags_needs_force_and_then_removes_every_tag() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/rmi-multi-tag:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let tag = ociman(
        storage_dir.path(),
        &[
            "tag",
            "ociman-test/rmi-multi-tag:latest",
            "ociman-test/rmi-multi-tag:aliased",
        ],
    );
    assert!(tag.status.success());

    let record = store
        .resolve_image("docker.io/ociman-test/rmi-multi-tag:latest")
        .unwrap()
        .unwrap();
    let short_id = record.manifest_digest.hex()[..12].to_string();

    let rmi = ociman(storage_dir.path(), &["rmi", &short_id]);
    assert!(!rmi.status.success());
    let stderr = String::from_utf8_lossy(&rmi.stderr);
    assert!(stderr.contains("more than one tag"), "{stderr}");
    assert!(stderr.contains("please force removal"), "{stderr}");
    // Neither tag was touched by the refused attempt.
    assert!(
        store
            .resolve_image("docker.io/ociman-test/rmi-multi-tag:latest")
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .resolve_image("docker.io/ociman-test/rmi-multi-tag:aliased")
            .unwrap()
            .is_some()
    );

    let rmi_forced = ociman(storage_dir.path(), &["rmi", "--force", &short_id]);
    assert!(
        rmi_forced.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rmi_forced.stderr)
    );
    assert!(
        store
            .resolve_image("docker.io/ociman-test/rmi-multi-tag:latest")
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .resolve_image("docker.io/ociman-test/rmi-multi-tag:aliased")
            .unwrap()
            .is_none()
    );
}

/// Removing by an exact *tag* (not an ID) never needs `--force` just
/// because a sibling tag exists -- real docker/podman both only ever
/// untag the one name given that way, checked directly the same way.
#[test]
fn rmi_by_an_exact_tag_never_needs_force_even_with_a_sibling_tag() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/rmi-tag-not-id:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let tag = ociman(
        storage_dir.path(),
        &[
            "tag",
            "ociman-test/rmi-tag-not-id:latest",
            "ociman-test/rmi-tag-not-id:aliased",
        ],
    );
    assert!(tag.status.success());

    let rmi = ociman(
        storage_dir.path(),
        &["rmi", "ociman-test/rmi-tag-not-id:latest"],
    );
    assert!(
        rmi.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rmi.stderr)
    );
    assert!(
        store
            .resolve_image("docker.io/ociman-test/rmi-tag-not-id:latest")
            .unwrap()
            .is_none()
    );
    // The sibling tag survives untouched.
    assert!(
        store
            .resolve_image("docker.io/ociman-test/rmi-tag-not-id:aliased")
            .unwrap()
            .is_some()
    );
}

#[test]
fn rmi_refuses_an_image_still_used_by_a_stopped_container_without_force() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/rmi-in-use:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "true".to_string(),
            ]),
            ..Default::default()
        },
    );

    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "--name",
            "rmi-dependent",
            "ociman-test/rmi-in-use:latest",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    let rmi = ociman(
        storage_dir.path(),
        &["rmi", "ociman-test/rmi-in-use:latest"],
    );
    assert!(!rmi.status.success());
    let stderr = String::from_utf8_lossy(&rmi.stderr);
    assert!(stderr.contains("in use"), "{stderr}");
    assert!(stderr.contains("--force"), "{stderr}");

    // Refused, so the image and the container are both still there.
    assert!(
        store
            .resolve_image("docker.io/ociman-test/rmi-in-use:latest")
            .unwrap()
            .is_some()
    );
    let ps = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    assert!(!String::from_utf8_lossy(&ps.stdout).trim().is_empty());
}

#[test]
fn rmi_force_removes_a_stopped_dependent_container_and_the_image() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/rmi-force-stopped:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "true".to_string(),
            ]),
            ..Default::default()
        },
    );

    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "--name",
            "rmi-force-stopped",
            "ociman-test/rmi-force-stopped:latest",
        ],
    );
    assert!(run.status.success());

    let rmi = ociman(
        storage_dir.path(),
        &["rmi", "--force", "ociman-test/rmi-force-stopped:latest"],
    );
    assert!(
        rmi.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rmi.stderr)
    );

    assert!(
        store
            .resolve_image("docker.io/ociman-test/rmi-force-stopped:latest")
            .unwrap()
            .is_none()
    );
    let ps = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    assert!(
        String::from_utf8_lossy(&ps.stdout).trim().is_empty(),
        "the dependent container should have been removed too"
    );
}

#[test]
fn rmi_force_kills_and_removes_a_still_running_dependent_container() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/rmi-force-running:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut child = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/rmi-force-running:latest",
        &["--name", "rmi-force-running", "--", "sleep", "30"],
    );
    // 20s, matching the established generous ceiling every other
    // `wait_for_status`-style poll in this test suite uses (`ociman_
    // kill.rs`/`ociman_stop.rs`) — a tight one is genuinely flaky
    // under CI/parallel-test-suite CPU contention, not a bug in the
    // container reaching "running" itself (see git history: "loosen
    // the run -d timing assertion").
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty(), "container never appeared in `ps`");
    let status = wait_for_status(storage_dir.path(), &id, "running", Duration::from_secs(20));
    assert_eq!(status, "running");

    let rmi = ociman(
        storage_dir.path(),
        &["rmi", "--force", "ociman-test/rmi-force-running:latest"],
    );
    assert!(
        rmi.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rmi.stderr)
    );

    assert!(
        store
            .resolve_image("docker.io/ociman-test/rmi-force-running:latest")
            .unwrap()
            .is_none()
    );
    let ps = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    assert!(
        String::from_utf8_lossy(&ps.stdout).trim().is_empty(),
        "the still-running dependent container should have been killed and removed too"
    );

    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn rmi_json_reports_the_canonical_reference_and_any_removed_containers() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/rmi-json:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "true".to_string(),
            ]),
            ..Default::default()
        },
    );
    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "--name",
            "rmi-json-dep",
            "ociman-test/rmi-json:latest",
        ],
    );
    assert!(run.status.success());
    let dependent_id = only_container_id(storage_dir.path(), Duration::from_secs(10));

    let rmi = ociman(
        storage_dir.path(),
        &["--json", "rmi", "--force", "ociman-test/rmi-json:latest"],
    );
    assert!(
        rmi.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rmi.stderr)
    );
    let view: serde_json::Value = serde_json::from_slice(&rmi.stdout).unwrap();
    assert_eq!(view["reference"], "docker.io/ociman-test/rmi-json:latest");
    assert_eq!(
        view["removed_containers"].as_array().unwrap(),
        &[serde_json::Value::String(dependent_id)]
    );
}

/// Resolving by ID with siblings that include this project's own
/// internal untagged-image sentinel (0179 -- e.g. a real tag plus an
/// earlier untagged build of the exact same image) shows `<none>`,
/// never the raw sentinel string, both in the "more than one tag"
/// refusal and in the actual removal listing (text and `--json`
/// alike) -- 0179's own "what this doesn't do yet" flagged this
/// display gap directly, closed here.
#[test]
fn rmi_by_id_shows_none_not_the_raw_sentinel_for_an_untagged_sibling() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/rmi-untagged-sibling:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let record = store
        .resolve_image("docker.io/ociman-test/rmi-untagged-sibling:latest")
        .unwrap()
        .unwrap();

    // A second, untagged pointer at the exact same digest -- the same
    // sentinel shape `ociman build`/`ociman commit` with no tag at all
    // record (0179/0180): the manifest digest, verbatim, no `/` at
    // all.
    let sentinel = record.manifest_digest.to_string();
    store
        .put_image(&ImageRecord {
            reference: sentinel.clone(),
            manifest_digest: record.manifest_digest.clone(),
        })
        .unwrap();

    let short_id = record.manifest_digest.hex()[..12].to_string();

    let rmi = ociman(storage_dir.path(), &["rmi", &short_id]);
    assert!(!rmi.status.success());
    let stderr = String::from_utf8_lossy(&rmi.stderr);
    assert!(stderr.contains("more than one tag"), "{stderr}");
    assert!(
        stderr.contains("<none>"),
        "the untagged sibling should show as <none>, not the raw sentinel: {stderr}"
    );
    assert!(
        !stderr.contains(&sentinel),
        "the raw internal sentinel string should never leak into user-facing output: {stderr}"
    );

    let rmi_json = ociman(storage_dir.path(), &["--json", "rmi", "--force", &short_id]);
    assert!(
        rmi_json.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rmi_json.stderr)
    );
    let view: serde_json::Value = serde_json::from_slice(&rmi_json.stdout).unwrap();
    // Alphabetically, "docker.io/..." sorts before "sha256:...", so
    // the real tag is primary and the sentinel is the one and only
    // "additional" reference here.
    assert_eq!(
        view["reference"],
        "docker.io/ociman-test/rmi-untagged-sibling:latest"
    );
    assert_eq!(
        view["additional_references_removed"],
        serde_json::json!([null]),
        "the untagged sibling should serialize as null, not the raw sentinel string: {view:?}"
    );

    assert!(
        store
            .resolve_image("docker.io/ociman-test/rmi-untagged-sibling:latest")
            .unwrap()
            .is_none()
    );
    assert!(store.resolve_image(&sentinel).unwrap().is_none());
}

/// `ociman rmi ref1 ref2` (0269) removes multiple explicit image
/// references in one call, matching real `podman rmi ref1 ref2`
/// exactly.
#[test]
fn rmi_accepts_multiple_references_and_removes_them_all() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/rmi-multi-a:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    seed_image(
        &store,
        "ociman-test/rmi-multi-b:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let rmi = ociman(
        storage_dir.path(),
        &[
            "rmi",
            "ociman-test/rmi-multi-a:latest",
            "ociman-test/rmi-multi-b:latest",
        ],
    );
    assert!(
        rmi.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rmi.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&rmi.stdout).trim().lines().count(),
        2,
        "{rmi:?}"
    );

    assert!(
        store
            .resolve_image("docker.io/ociman-test/rmi-multi-a:latest")
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .resolve_image("docker.io/ociman-test/rmi-multi-b:latest")
            .unwrap()
            .is_none()
    );
}

/// A real, checked-directly *different* policy than `ociman rm`'s own
/// all-or-nothing preflight (0267): one unresolvable reference among
/// otherwise-valid ones does *not* block removing the others — real
/// `podman rmi valid1 bogus valid2` (verified directly) still removes
/// both `valid1` and `valid2`, only refusing `bogus`.
#[test]
fn rmi_with_one_unresolvable_reference_still_removes_the_others() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/rmi-multi-bogus-a:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    seed_image(
        &store,
        "ociman-test/rmi-multi-bogus-b:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let rmi = ociman(
        storage_dir.path(),
        &[
            "rmi",
            "ociman-test/rmi-multi-bogus-a:latest",
            "ociman-test/does-not-exist:latest",
            "ociman-test/rmi-multi-bogus-b:latest",
        ],
    );
    assert!(
        !rmi.status.success(),
        "the one unresolvable reference's own failure should still surface"
    );
    assert!(
        String::from_utf8_lossy(&rmi.stderr).contains("no such image"),
        "{}",
        String::from_utf8_lossy(&rmi.stderr)
    );

    // Both real, valid images are still removed despite the bogus one
    // in between them -- a genuinely different policy than `ociman
    // rm`'s own all-or-nothing preflight for multiple container IDs.
    assert!(
        store
            .resolve_image("docker.io/ociman-test/rmi-multi-bogus-a:latest")
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .resolve_image("docker.io/ociman-test/rmi-multi-bogus-b:latest")
            .unwrap()
            .is_none()
    );
}

/// `ociman --json rmi ref1 ref2` with more than one reference prints a
/// JSON *array* of results (one per reference), while the single-
/// reference case (every pre-existing test above) keeps its original,
/// unwrapped single-object shape unchanged for backward compatibility.
#[test]
fn rmi_json_with_multiple_references_prints_an_array() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/rmi-multi-json-a:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    seed_image(
        &store,
        "ociman-test/rmi-multi-json-b:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let rmi = ociman(
        storage_dir.path(),
        &[
            "--json",
            "rmi",
            "ociman-test/rmi-multi-json-a:latest",
            "ociman-test/rmi-multi-json-b:latest",
        ],
    );
    assert!(
        rmi.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rmi.stderr)
    );
    let view: serde_json::Value = serde_json::from_slice(&rmi.stdout).unwrap();
    let array = view.as_array().expect("a JSON array for multiple refs");
    assert_eq!(array.len(), 2, "{view:?}");
    assert_eq!(
        array[0]["reference"],
        "docker.io/ociman-test/rmi-multi-json-a:latest"
    );
    assert_eq!(
        array[1]["reference"],
        "docker.io/ociman-test/rmi-multi-json-b:latest"
    );
}

/// `ociman rmi --ignore` (0270): a reference that doesn't resolve to
/// any real image is a silent no-op, matching real `podman rmi
/// --ignore`/`-i` exactly (checked directly: without `--ignore`, the
/// identical call is a clear error, see
/// `rmi_of_an_unknown_reference_is_a_clear_error`).
#[test]
fn rmi_ignore_silently_succeeds_on_a_nonexistent_reference() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let rmi = ociman(
        storage_dir.path(),
        &["rmi", "--ignore", "does-not-exist:latest"],
    );
    assert!(
        rmi.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rmi.stderr)
    );
    assert!(rmi.stdout.is_empty(), "{rmi:?}");
    assert!(rmi.stderr.is_empty(), "{rmi:?}");
}

/// `--force` implies `--ignore` too, matching real `podman rmi
/// --force`'s own checked-directly behavior exactly: a nonexistent
/// reference is a silent no-op under `--force` alone, with no
/// `--ignore` given at all.
#[test]
fn rmi_force_alone_also_silently_succeeds_on_a_nonexistent_reference() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let rmi = ociman(
        storage_dir.path(),
        &["rmi", "--force", "does-not-exist:latest"],
    );
    assert!(
        rmi.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rmi.stderr)
    );
}

/// `--ignore` only ever silences the "doesn't resolve to anything at
/// all" case -- checked directly against a real installed `podman
/// rmi --ignore`, an in-use-by-container refusal is still reported.
#[test]
fn rmi_ignore_does_not_silence_an_in_use_by_container_error() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/rmi-ignore-in-use:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "true".to_string(),
            ]),
            ..Default::default()
        },
    );
    let run = ociman(
        storage_dir.path(),
        &["run", "ociman-test/rmi-ignore-in-use:latest"],
    );
    assert!(run.status.success());

    let rmi = ociman(
        storage_dir.path(),
        &["rmi", "--ignore", "ociman-test/rmi-ignore-in-use:latest"],
    );
    assert!(
        !rmi.status.success(),
        "an in-use-by-container error should still surface even with --ignore"
    );
    assert!(
        String::from_utf8_lossy(&rmi.stderr).contains("in use"),
        "{}",
        String::from_utf8_lossy(&rmi.stderr)
    );
    // The image survives, untouched by the refused attempt.
    assert!(
        store
            .resolve_image("docker.io/ociman-test/rmi-ignore-in-use:latest")
            .unwrap()
            .is_some()
    );
}

/// `--ignore` combined with a mix of a real, valid reference and a
/// nonexistent one: the valid one is removed, the nonexistent one is
/// silently skipped, and the overall call succeeds.
#[test]
fn rmi_ignore_removes_the_valid_reference_and_skips_the_nonexistent_one() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/rmi-ignore-mixed:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let rmi = ociman(
        storage_dir.path(),
        &[
            "rmi",
            "--ignore",
            "ociman-test/rmi-ignore-mixed:latest",
            "does-not-exist:latest",
        ],
    );
    assert!(
        rmi.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rmi.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&rmi.stdout).trim(),
        "docker.io/ociman-test/rmi-ignore-mixed:latest"
    );
    assert!(
        store
            .resolve_image("docker.io/ociman-test/rmi-ignore-mixed:latest")
            .unwrap()
            .is_none()
    );
}

/// `ociman rmi --all` (0271) removes every image in local storage,
/// matching real `podman rmi --all` exactly.
#[test]
fn rmi_all_removes_every_image() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/rmi-all-a:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    seed_image(
        &store,
        "ociman-test/rmi-all-b:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let rmi = ociman(storage_dir.path(), &["rmi", "--all"]);
    assert!(
        rmi.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rmi.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&rmi.stdout).trim().lines().count(),
        2,
        "{rmi:?}"
    );

    let images = ociman(storage_dir.path(), &["images", "--json"]);
    let views: serde_json::Value = serde_json::from_slice(&images.stdout).unwrap();
    assert!(views.as_array().unwrap().is_empty(), "{views:?}");

    // A real, silent no-op on an already-empty store, matching this
    // project's own established convention (`ociman rm --all`/`ociman
    // prune --all`'s own identical rule).
    let rmi_again = ociman(storage_dir.path(), &["rmi", "--all"]);
    assert!(rmi_again.status.success());
    assert!(String::from_utf8_lossy(&rmi_again.stdout).trim().is_empty());
}

/// `--all` and an explicit reference together is a clear error, never
/// an ambiguous silent choice between the two (matching this
/// project's own `ociman rm --all`'s own identical rule).
#[test]
fn rmi_all_and_an_explicit_reference_together_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    let out = ociman(storage_dir.path(), &["rmi", "--all", "some-image:latest"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("cannot give both"),
        "{out:?}"
    );
}

/// `rmi --all` without `--force` still refuses an image a container
/// depends on (real `podman rmi --all` alone, without `--force`,
/// leaves it untouched too), but every *other* image is still
/// attempted, matching real `podman rmi`'s own multi-target behavior.
#[test]
fn rmi_all_without_force_skips_an_in_use_image_but_still_removes_the_rest() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/rmi-all-mixed-free:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "true".to_string(),
            ]),
            ..Default::default()
        },
    );
    seed_image(
        &store,
        "ociman-test/rmi-all-mixed-inuse:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "true".to_string(),
            ]),
            ..Default::default()
        },
    );
    let run = ociman(
        storage_dir.path(),
        &["run", "ociman-test/rmi-all-mixed-inuse:latest"],
    );
    assert!(run.status.success(), "{run:?}");

    let rmi = ociman(storage_dir.path(), &["rmi", "--all"]);
    assert!(
        !rmi.status.success(),
        "the one in-use image's own failure should still surface"
    );
    assert!(
        String::from_utf8_lossy(&rmi.stderr).contains("in use"),
        "{}",
        String::from_utf8_lossy(&rmi.stderr)
    );

    // The free image is gone; the in-use one survives untouched.
    assert!(
        store
            .resolve_image("docker.io/ociman-test/rmi-all-mixed-free:latest")
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .resolve_image("docker.io/ociman-test/rmi-all-mixed-inuse:latest")
            .unwrap()
            .is_some()
    );

    let forced = ociman(storage_dir.path(), &["rmi", "--all", "--force"]);
    assert!(forced.status.success(), "{forced:?}");
    assert!(
        store
            .resolve_image("docker.io/ociman-test/rmi-all-mixed-inuse:latest")
            .unwrap()
            .is_none()
    );
}

/// The specific real edge case `0271`'s own design note flagged and
/// verified by hand: a single manifest digest with *both* several
/// real tags *and* an untagged sentinel record (`0179`) present at
/// once. `--all` must remove every one of them without ever tripping
/// the by-ID sibling-tag-ambiguity gate (`rmi <id>` needs `--force`
/// for more than one tag) that only applies to *user-supplied*
/// ambiguous spec resolution, never to `--all`'s own "remove every
/// already-enumerated record independently" mode.
#[test]
fn rmi_all_removes_a_digest_with_both_multiple_tags_and_an_untagged_sibling() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/rmi-all-siblings:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let record = store
        .resolve_image("docker.io/ociman-test/rmi-all-siblings:latest")
        .unwrap()
        .unwrap();

    // A second real tag, plus an untagged sentinel record (0179),
    // sharing the exact same manifest digest.
    store
        .put_image(&ImageRecord {
            reference: "docker.io/ociman-test/rmi-all-siblings-second:latest".to_string(),
            manifest_digest: record.manifest_digest.clone(),
        })
        .unwrap();
    let sentinel = record.manifest_digest.to_string();
    store
        .put_image(&ImageRecord {
            reference: sentinel.clone(),
            manifest_digest: record.manifest_digest.clone(),
        })
        .unwrap();

    let rmi = ociman(storage_dir.path(), &["rmi", "--all"]);
    assert!(
        rmi.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rmi.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&rmi.stdout).trim().lines().count(),
        3,
        "all three records sharing the digest should be removed: {rmi:?}"
    );

    assert!(
        store
            .resolve_image("docker.io/ociman-test/rmi-all-siblings:latest")
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .resolve_image("docker.io/ociman-test/rmi-all-siblings-second:latest")
            .unwrap()
            .is_none()
    );
    assert!(store.resolve_image(&sentinel).unwrap().is_none());
}

/// `ociman image rm` (0480) is a real, genuine alias for `ociman
/// rmi` itself -- matching real `podman image rm`'s own checked-
/// directly identical `RunE`/flag set as top-level `podman rmi`
/// exactly (`~/git/podman/cmd/podman/images/rm.go`; note real
/// podman's own naming for this specific pair is the reverse of what
/// "nested vs. top-level" might otherwise suggest -- `rm` is the
/// nested one, `rmi` the top-level one).
#[test]
fn image_rm_is_a_byte_identical_alias_for_rmi() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/image-rm-alias:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let rm = ociman(
        storage_dir.path(),
        &["image", "rm", "ociman-test/image-rm-alias:latest"],
    );
    assert!(
        rm.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rm.stderr)
    );
    assert!(
        store
            .resolve_image("docker.io/ociman-test/image-rm-alias:latest")
            .unwrap()
            .is_none()
    );
}
