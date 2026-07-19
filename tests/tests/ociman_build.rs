//! `ociman build` integration tests: the first working end-to-end
//! `build` command (see `bin/ociman/src/build.rs`'s own doc comment
//! for this first increment's deliberately narrow scope). Same fully
//! offline approach as `ociman_ps.rs`/`ociman_run.rs` (a synthetic-
//! but-structurally-real seeded base image via `seed_image`, no
//! registry access needed).

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

fn write_containerfile(dir: &Path, contents: &str) {
    std::fs::write(dir.join("Containerfile"), contents).unwrap();
}

#[test]
fn builds_a_metadata_only_image_and_applies_every_supported_instruction() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/build-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            env: vec!["PATH=/bin".to_string()],
            ..Default::default()
        },
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/build-base:latest\n\
         LABEL maintainer=\"someone@example.com\"\n\
         ENV FOO=bar\n\
         WORKDIR /app\n\
         WORKDIR sub\n\
         USER 1000\n\
         EXPOSE 8080\n\
         VOLUME /data\n\
         STOPSIGNAL SIGTERM\n\
         CMD [\"/bin/sh\", \"-c\", \"echo hi\"]\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/built:latest",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let stdout = String::from_utf8_lossy(&build.stdout);
    assert!(
        stdout.contains("tagged: docker.io/ociman-test/built:latest"),
        "{stdout}"
    );

    let record = store
        .resolve_image("docker.io/ociman-test/built:latest")
        .unwrap()
        .expect("built image should be recorded in the store");
    let config = store.image_config(&record).unwrap();
    let cc = config.config.expect("container config");

    assert_eq!(cc.env, vec!["PATH=/bin".to_string(), "FOO=bar".to_string()]);
    assert_eq!(cc.working_dir.as_deref(), Some("/app/sub"));
    assert_eq!(cc.user.as_deref(), Some("1000"));
    assert_eq!(
        cc.cmd,
        Some(vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "echo hi".to_string()
        ])
    );
    assert_eq!(
        cc.labels.get("maintainer").map(String::as_str),
        Some("someone@example.com")
    );
    assert!(cc.exposed_ports.contains_key("8080"));
    assert!(cc.volumes.contains_key("/data"));
    assert_eq!(cc.stop_signal.as_deref(), Some("SIGTERM"));

    // No RUN/COPY/ADD in this Containerfile -- the built image's own
    // layers are identical to its base image's (nothing here can
    // produce a new one yet).
    let base_record = store
        .resolve_image("docker.io/ociman-test/build-base:latest")
        .unwrap()
        .unwrap();
    let base_manifest = store.image_manifest(&base_record).unwrap();
    let built_manifest = store.image_manifest(&record).unwrap();
    assert_eq!(built_manifest.layers, base_manifest.layers);
    assert_eq!(
        config.rootfs.diff_ids,
        store.image_config(&base_record).unwrap().rootfs.diff_ids
    );

    // A real history entry was recorded for each of the 9 metadata
    // instructions above (LABEL, ENV, two WORKDIRs, USER, EXPOSE,
    // VOLUME, STOPSIGNAL, CMD -- the seeded base image itself starts
    // with no history at all, see `seed_image`), every one of them
    // `empty_layer: true` since none of them is `RUN`/`COPY`/`ADD`.
    assert_eq!(config.history.len(), 9);
    assert!(config.history.iter().all(|h| h.empty_layer));
}

#[test]
fn env_updates_an_existing_key_in_place_rather_than_duplicating() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/env-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            env: vec!["PATH=/bin".to_string(), "FOO=original".to_string()],
            ..Default::default()
        },
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/env-base:latest\nENV FOO=updated\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/env-updated:latest",
        ],
    );
    assert!(build.status.success());

    let record = store
        .resolve_image("docker.io/ociman-test/env-updated:latest")
        .unwrap()
        .unwrap();
    let config = store.image_config(&record).unwrap();
    let cc = config.config.unwrap();
    // Same length and position -- FOO was updated in place, not
    // duplicated or moved to the end.
    assert_eq!(
        cc.env,
        vec!["PATH=/bin".to_string(), "FOO=updated".to_string()]
    );
}

#[test]
fn rejects_a_run_instruction_with_a_clear_error() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/run-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/run-base:latest\nRUN echo hi\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/should-fail:latest",
        ],
    );
    assert!(!build.status.success());
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(stderr.contains("RUN is not yet supported"), "{stderr}");
    assert!(
        store
            .resolve_image("docker.io/ociman-test/should-fail:latest")
            .unwrap()
            .is_none(),
        "a failed build must not leave a partial image tagged"
    );
}

#[test]
fn rejects_a_multi_stage_dockerfile_with_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM busybox AS builder\nFROM busybox\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "x:latest",
        ],
    );
    assert!(!build.status.success());
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(
        stderr.contains("multi-stage Dockerfiles are not yet supported"),
        "{stderr}"
    );
}

#[test]
fn rejects_from_scratch_with_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(context_dir.path(), "FROM scratch\n");

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "x:latest",
        ],
    );
    assert!(!build.status.success());
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(stderr.contains("FROM scratch"), "{stderr}");
}

#[test]
fn requires_a_tag() {
    let storage_dir = tempfile::tempdir().unwrap();
    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(context_dir.path(), "FROM busybox\n");

    let build = ociman(
        storage_dir.path(),
        &["build", context_dir.path().to_str().unwrap()],
    );
    assert!(!build.status.success());
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(stderr.contains("-t/--tag is required"), "{stderr}");
}

#[test]
fn containerfile_is_preferred_over_dockerfile_when_both_exist() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/pref-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        context_dir.path().join("Dockerfile"),
        "FROM ociman-test/pref-base:latest\nLABEL which=dockerfile\n",
    )
    .unwrap();
    std::fs::write(
        context_dir.path().join("Containerfile"),
        "FROM ociman-test/pref-base:latest\nLABEL which=containerfile\n",
    )
    .unwrap();

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/pref-result:latest",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let record = store
        .resolve_image("docker.io/ociman-test/pref-result:latest")
        .unwrap()
        .unwrap();
    let config = store.image_config(&record).unwrap();
    assert_eq!(
        config
            .config
            .unwrap()
            .labels
            .get("which")
            .map(String::as_str),
        Some("containerfile")
    );
}

#[test]
fn explicit_file_flag_overrides_the_default_search() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/explicit-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        context_dir.path().join("my.Containerfile"),
        "FROM ociman-test/explicit-base:latest\nLABEL which=explicit\n",
    )
    .unwrap();

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-f",
            "my.Containerfile",
            "-t",
            "ociman-test/explicit-result:latest",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let record = store
        .resolve_image("docker.io/ociman-test/explicit-result:latest")
        .unwrap()
        .unwrap();
    let config = store.image_config(&record).unwrap();
    assert_eq!(
        config
            .config
            .unwrap()
            .labels
            .get("which")
            .map(String::as_str),
        Some("explicit")
    );
}
