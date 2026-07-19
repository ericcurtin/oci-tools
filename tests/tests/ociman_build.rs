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

/// A real, end-to-end `RUN` step: runs a real command in a real
/// rootless container against the base image's own materialized
/// layers, diffs what changed, and commits it as a genuinely new
/// stored layer -- not a mock or a dry run.
#[test]
fn run_executes_a_real_command_and_commits_a_real_new_layer() {
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
        &["sh", "mkdir", "cat"],
        ContainerConfig::default(),
    );
    let base_record = store
        .resolve_image("docker.io/ociman-test/run-base:latest")
        .unwrap()
        .unwrap();
    let base_manifest = store.image_manifest(&base_record).unwrap();

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/run-base:latest\n\
         RUN echo hello > /marker.txt\n\
         RUN mkdir -p /app && echo world > /app/second.txt\n\
         ENV BUILT=yes\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/run-result:latest",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let record = store
        .resolve_image("docker.io/ociman-test/run-result:latest")
        .unwrap()
        .unwrap();
    let manifest = store.image_manifest(&record).unwrap();
    // Two real RUN steps -> two new layers on top of the base image's
    // own (this seeded image has exactly one).
    assert_eq!(manifest.layers.len(), base_manifest.layers.len() + 2);
    assert_eq!(manifest.layers[0], base_manifest.layers[0]);

    let config = store.image_config(&record).unwrap();
    assert_eq!(config.rootfs.diff_ids.len(), manifest.layers.len());
    // Real, non-empty-layer history entries recorded for both RUN
    // steps, in order, plus the ENV instruction's own empty-layer one.
    assert_eq!(config.history.len(), 3);
    assert_eq!(
        config.history[0].created_by.as_deref(),
        Some("RUN /bin/sh -c echo hello > /marker.txt")
    );
    assert!(!config.history[0].empty_layer);
    assert_eq!(
        config.history[1].created_by.as_deref(),
        Some("RUN /bin/sh -c mkdir -p /app && echo world > /app/second.txt")
    );
    assert!(!config.history[1].empty_layer);
    assert!(config.history[2].empty_layer);

    // The most convincing check: actually run the built image and
    // confirm both files a RUN step wrote are really there, with the
    // right content, and the ENV instruction's own var is set too --
    // not just that *some* new layer blobs exist, but that they
    // apply back into a real, working container.
    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "--rm",
            "ociman-test/run-result:latest",
            "--",
            "/bin/sh",
            "-c",
            "cat /marker.txt && cat /app/second.txt && echo BUILT=$BUILT",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert_eq!(stdout, "hello\nworld\nBUILT=yes\n");
}

/// A `RUN` step is a real container's own process -- a nonzero exit
/// aborts the whole build, matching real `docker build`/`podman
/// build`, and leaves nothing tagged (same "no partial image" contract
/// every other rejection path in this file already checks).
#[test]
fn a_failing_run_aborts_the_build_and_leaves_nothing_tagged() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/run-fail-base:latest",
        &busybox,
        &["sh", "false"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/run-fail-base:latest\nRUN false\n",
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
    assert!(
        stderr.contains("RUN /bin/sh -c false failed with exit code"),
        "{stderr}"
    );
    assert!(
        store
            .resolve_image("docker.io/ociman-test/should-fail:latest")
            .unwrap()
            .is_none(),
        "a failed build must not leave a partial image tagged"
    );
}

/// A real, end-to-end multi-stage build: the target stage's own `FROM
/// builder` inherits `builder`'s own already-committed layer (from a
/// real `RUN` step) and its own config (`ENV FOO=bar`), then adds its
/// own `ENV BAZ=qux` on top -- no re-pulling, no re-running anything
/// from `builder`, and the built image's own layer list has exactly
/// one layer beyond the base (`builder`'s own `RUN`, not a re-run of
/// it), confirmed both by inspecting the manifest and by actually
/// running the final image.
#[test]
fn multi_stage_from_an_earlier_stage_inherits_its_layers_and_config() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/multi-base:latest",
        &busybox,
        &["sh", "cat"],
        ContainerConfig::default(),
    );
    let base_record = store
        .resolve_image("docker.io/ociman-test/multi-base:latest")
        .unwrap()
        .unwrap();
    let base_manifest = store.image_manifest(&base_record).unwrap();

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/multi-base:latest AS builder\n\
         ENV FOO=bar\n\
         RUN echo hello > /marker.txt\n\
         FROM builder\n\
         ENV BAZ=qux\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/multi-result:latest",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let record = store
        .resolve_image("docker.io/ociman-test/multi-result:latest")
        .unwrap()
        .unwrap();
    let manifest = store.image_manifest(&record).unwrap();
    // `builder`'s own one real RUN layer, on top of the base image's
    // own -- never re-run, never re-committed a second time.
    assert_eq!(manifest.layers.len(), base_manifest.layers.len() + 1);

    let config = store.image_config(&record).unwrap();
    let cc = config.config.unwrap();
    assert!(cc.env.contains(&"FOO=bar".to_string()));
    assert!(cc.env.contains(&"BAZ=qux".to_string()));

    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "--rm",
            "ociman-test/multi-result:latest",
            "--",
            "/bin/sh",
            "-c",
            "cat /marker.txt && echo FOO=$FOO BAZ=$BAZ",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "hello\nFOO=bar BAZ=qux\n"
    );
}

/// A stage nothing later depends on (neither as a `FROM` base nor,
/// once supported, a `COPY --from=`) is pruned by `stages_needed_for`
/// and never built at all -- proven here by giving the unrelated
/// stage a `FROM` reference to an image that doesn't exist anywhere
/// (not seeded, not a real registry reference this test could ever
/// pull): if it were built, the whole command would fail; since it
/// isn't, the build succeeds using only the target stage's own real
/// base.
#[test]
fn an_unreferenced_stage_is_pruned_and_never_built() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/prune-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM this-image-does-not-exist-anywhere:latest AS unrelated\n\
         LABEL unused=true\n\
         FROM ociman-test/prune-base:latest\n\
         LABEL used=true\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/prune-result:latest",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let record = store
        .resolve_image("docker.io/ociman-test/prune-result:latest")
        .unwrap()
        .unwrap();
    let config = store.image_config(&record).unwrap();
    let labels = config.config.unwrap().labels;
    assert_eq!(labels.get("used").map(String::as_str), Some("true"));
    assert!(!labels.contains_key("unused"));
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

/// A real, end-to-end `COPY`: a plain file, a whole directory (whose
/// own *contents*, not the directory itself, must land inside the
/// destination), and a relative destination resolved against a prior
/// `WORKDIR` -- each committed as its own real new layer, then the
/// built image is actually run to confirm every file really is there
/// with the right content.
#[test]
fn copy_copies_real_files_and_directories_from_the_build_context() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/copy-base:latest",
        &busybox,
        &["sh", "cat", "ls"],
        ContainerConfig::default(),
    );
    let base_record = store
        .resolve_image("docker.io/ociman-test/copy-base:latest")
        .unwrap()
        .unwrap();
    let base_manifest = store.image_manifest(&base_record).unwrap();

    let context_dir = tempfile::tempdir().unwrap();
    std::fs::write(context_dir.path().join("hello.txt"), "file content\n").unwrap();
    std::fs::create_dir(context_dir.path().join("subdir")).unwrap();
    std::fs::write(
        context_dir.path().join("subdir").join("nested.txt"),
        "nested content\n",
    )
    .unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/copy-base:latest\n\
         COPY hello.txt /hello.txt\n\
         COPY subdir /app/subdir\n\
         WORKDIR /app\n\
         COPY hello.txt into-workdir.txt\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/copy-result:latest",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let record = store
        .resolve_image("docker.io/ociman-test/copy-result:latest")
        .unwrap()
        .unwrap();
    let manifest = store.image_manifest(&record).unwrap();
    // Three real COPY steps -> three new layers on top of the base
    // image's own (this seeded image has exactly one).
    assert_eq!(manifest.layers.len(), base_manifest.layers.len() + 3);

    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "--rm",
            "ociman-test/copy-result:latest",
            "--",
            "/bin/sh",
            "-c",
            "cat /hello.txt && cat /app/subdir/nested.txt && cat /app/into-workdir.txt",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "file content\nnested content\nfile content\n"
    );
}

/// Copying a file into a destination that already exists as a
/// directory (no trailing `/` needed) places it inside, under its own
/// basename -- matching real Docker/`cp` semantics, exercised
/// separately from the "destination doesn't exist yet" case above.
#[test]
fn copy_into_an_existing_directory_keeps_the_sources_own_basename() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/copy-existing-base:latest",
        &busybox,
        &["sh", "cat"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    std::fs::write(context_dir.path().join("hello.txt"), "file content\n").unwrap();
    std::fs::create_dir(context_dir.path().join("subdir")).unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/copy-existing-base:latest\n\
         COPY subdir /app/subdir\n\
         COPY hello.txt /app/subdir\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/copy-existing-result:latest",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "--rm",
            "ociman-test/copy-existing-result:latest",
            "--",
            "/bin/sh",
            "-c",
            "cat /app/subdir/hello.txt",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "file content\n");
}

#[test]
fn copy_rejects_a_source_that_escapes_the_build_context() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/copy-escape-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/copy-escape-base:latest\nCOPY ../outside.txt /outside.txt\n",
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
    assert!(stderr.contains("escapes its own root"), "{stderr}");
}

#[test]
fn copy_rejects_unsupported_flags_multiple_sources_and_globs() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/copy-reject-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let cases = [
        ("COPY --chown=1000:1000 a.txt /a.txt\n", "--chown"),
        ("COPY --chmod=755 a.txt /a.txt\n", "--chmod"),
        ("COPY --from=builder a.txt /a.txt\n", "--from"),
        ("COPY a.txt b.txt /dest/\n", "more than one source"),
        ("COPY *.txt /dest/\n", "wildcard"),
    ];
    for (instruction, expected_error_fragment) in cases {
        let context_dir = tempfile::tempdir().unwrap();
        std::fs::write(context_dir.path().join("a.txt"), "a").unwrap();
        std::fs::write(context_dir.path().join("b.txt"), "b").unwrap();
        write_containerfile(
            context_dir.path(),
            &format!("FROM ociman-test/copy-reject-base:latest\n{instruction}"),
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
        assert!(!build.status.success(), "{instruction:?} should fail");
        let stderr = String::from_utf8_lossy(&build.stderr);
        assert!(
            stderr.contains(expected_error_fragment),
            "{instruction:?}: expected {expected_error_fragment:?} in {stderr}"
        );
    }
}

#[test]
fn copy_rejects_a_missing_source() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/copy-missing-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/copy-missing-base:latest\nCOPY does-not-exist.txt /foo.txt\n",
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
    assert!(stderr.contains("does not exist"), "{stderr}");
    assert!(
        store
            .resolve_image("docker.io/ociman-test/should-fail:latest")
            .unwrap()
            .is_none(),
        "a failed build must not leave a partial image tagged"
    );
}
