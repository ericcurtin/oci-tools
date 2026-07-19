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

use oci_tools_tests::{bin_path, busybox_path, seed_image, seed_image_with_files};

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

/// Multiple explicit sources in one `COPY`, matching real Docker's own
/// documented rule: each source lands under `dest` by its own
/// basename, and (checked directly against the real source,
/// `performCopyForInfo` in `copy.go`) a directory source's own
/// contents are still flattened directly into `dest`, never nested
/// under the directory's own basename even with other sources
/// alongside it.
#[test]
fn copy_with_multiple_sources_places_each_under_its_own_basename() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/copy-multi-base:latest",
        &busybox,
        &["sh", "cat"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    std::fs::write(context_dir.path().join("a.txt"), "aaa\n").unwrap();
    std::fs::write(context_dir.path().join("b.txt"), "bbb\n").unwrap();
    std::fs::create_dir(context_dir.path().join("subdir")).unwrap();
    std::fs::write(
        context_dir.path().join("subdir").join("nested.txt"),
        "nested\n",
    )
    .unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/copy-multi-base:latest\n\
         COPY a.txt b.txt subdir /app/\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/copy-multi-result:latest",
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
            "ociman-test/copy-multi-result:latest",
            "--",
            "/bin/sh",
            "-c",
            "cat /app/a.txt /app/b.txt /app/nested.txt",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "aaa\nbbb\nnested\n");
}

/// `COPY *.txt /dest/` matches real BuildKit's own documented glob
/// semantics exactly (`oci_dockerfile::{contains_wildcards,
/// match_pattern}`, exhaustively verified against the real Go
/// toolchain's own official test suite): `*` never crosses a `/`, so
/// a top-level pattern like `*.txt` matches only top-level files, not
/// `subdir/nested.txt` -- confirmed here by checking the *absence* of
/// the nested file just as carefully as the presence of the matched
/// ones, and that a differently-suffixed file (`c.md`) is correctly
/// excluded too.
#[test]
fn copy_expands_a_glob_pattern_against_the_build_context() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/copy-glob-base:latest",
        &busybox,
        &["sh", "find"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    std::fs::write(context_dir.path().join("a.txt"), "aaa\n").unwrap();
    std::fs::write(context_dir.path().join("b.txt"), "bbb\n").unwrap();
    std::fs::write(context_dir.path().join("c.md"), "ccc\n").unwrap();
    std::fs::create_dir(context_dir.path().join("subdir")).unwrap();
    std::fs::write(
        context_dir.path().join("subdir").join("nested.txt"),
        "nested\n",
    )
    .unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/copy-glob-base:latest\n\
         COPY *.txt /app/\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/copy-glob-result:latest",
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
            "ociman-test/copy-glob-result:latest",
            "--",
            "/bin/sh",
            "-c",
            "find /app -type f | sort",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "/app/a.txt\n/app/b.txt\n"
    );
}

/// A glob pattern containing a real `/` (e.g. `subdir/*.txt`) matches
/// entries at that exact nested depth -- BuildKit's own
/// `copyWithWildcards` walks the *entire* source tree, not just the
/// top level, testing each visited entry's own path relative to the
/// source root.
#[test]
fn copy_expands_a_glob_pattern_that_reaches_into_a_subdirectory() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/copy-glob-nested-base:latest",
        &busybox,
        &["sh", "cat"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(context_dir.path().join("subdir")).unwrap();
    std::fs::write(
        context_dir.path().join("subdir").join("nested.txt"),
        "nested content\n",
    )
    .unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/copy-glob-nested-base:latest\n\
         COPY subdir/*.txt /app.txt\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/copy-glob-nested-result:latest",
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
            "ociman-test/copy-glob-nested-result:latest",
            "--",
            "/bin/sh",
            "-c",
            "cat /app.txt",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "nested content\n");
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
fn copy_rejects_unsupported_flags_and_bad_glob_patterns() {
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
        (
            "COPY a.txt b.txt /a.txt\n",
            "must be a directory and end with a /",
        ),
        // A malformed glob pattern (an unterminated `[...]` character
        // class) is still a real, surfaced error -- unlike a
        // well-formed glob pattern, which is genuinely supported now
        // (see `copy_expands_a_glob_pattern_against_the_build_context`).
        ("COPY a[ /dest/\n", "invalid glob pattern"),
        // A well-formed glob pattern matching zero real files is a
        // real, surfaced error too, matching real BuildKit's own "no
        // source files were specified".
        ("COPY *.nonexistent /dest/\n", "matched no files"),
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

fn write_gzip_tar(path: &Path, files: &[(&str, &[u8])]) {
    let gz_file = std::fs::File::create(path).unwrap();
    let encoder = flate2::write::GzEncoder::new(gz_file, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    for (name, content) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, name, *content).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap();
}

/// `ADD` real docker's own documented archive-auto-extraction
/// behavior: a local gzip-compressed tar archive is unpacked into the
/// destination directory (created, along with any missing parents),
/// not copied verbatim as one file the way `COPY`/a non-archive `ADD`
/// source would be.
#[test]
fn add_extracts_a_local_gzip_tar_archive_into_the_destination_directory() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/add-archive-base:latest",
        &busybox,
        &["sh", "cat", "find"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_gzip_tar(
        &context_dir.path().join("payload.tar.gz"),
        &[
            ("file1.txt", b"hello from archive\n"),
            ("subdir/file2.txt", b"nested\n"),
        ],
    );
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/add-archive-base:latest\n\
         ADD payload.tar.gz /extracted\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/add-archive-result:latest",
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
            "ociman-test/add-archive-result:latest",
            "--",
            "/bin/sh",
            "-c",
            "cat /extracted/file1.txt && cat /extracted/subdir/file2.txt",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "hello from archive\nnested\n"
    );
}

/// `ADD` of a source that isn't a recognized archive behaves exactly
/// like `COPY` -- including a gzip-compressed file that just isn't
/// secretly a tar archive, the real false-positive plain magic-byte
/// sniffing alone would miss (`oci_layer::detect_archive`'s own tests
/// already cover the sniffing logic directly; this confirms
/// `ociman build` itself wires it correctly end to end).
#[test]
fn add_copies_a_non_archive_source_like_copy() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/add-plain-base:latest",
        &busybox,
        &["sh", "cat", "zcat"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    std::fs::write(context_dir.path().join("plain.txt"), "plain content\n").unwrap();
    // A real gzip stream (correct magic bytes, genuinely decompresses)
    // whose content is deliberately *not* a tar archive.
    let gz_path = context_dir.path().join("notarchive.txt.gz");
    let gz_file = std::fs::File::create(&gz_path).unwrap();
    let mut encoder = flate2::write::GzEncoder::new(gz_file, flate2::Compression::default());
    std::io::Write::write_all(&mut encoder, b"just gzipped text, not a tar\n").unwrap();
    encoder.finish().unwrap();

    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/add-plain-base:latest\n\
         ADD plain.txt /copied.txt\n\
         ADD notarchive.txt.gz /still-gzipped.gz\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/add-plain-result:latest",
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
            "ociman-test/add-plain-result:latest",
            "--",
            "/bin/sh",
            "-c",
            "cat /copied.txt && zcat /still-gzipped.gz",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "plain content\njust gzipped text, not a tar\n"
    );
}

/// A tiny, single-response HTTP/1.1 mock -- the same real-loopback-
/// socket pattern `oci-registry`'s own `client.rs` test module
/// established, reused here rather than a fake transport.
fn serve_one_response(response: &'static str) -> std::net::SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        use std::io::{BufRead as _, BufReader, Write as _};
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        loop {
            let mut header_line = String::new();
            reader.read_line(&mut header_line).unwrap();
            if header_line.trim().is_empty() {
                break;
            }
        }
        stream.write_all(response.as_bytes()).unwrap();
    });
    addr
}

/// `ADD` from a remote URL fetches the real content over a real
/// socket and places it at an explicit, non-`/`-ending destination
/// verbatim -- never auto-extracted even though this body is a real
/// gzip stream, matching real BuildKit's own `noDecompress` for
/// exactly this source kind (see `add_instruction`'s own doc comment).
#[test]
fn add_from_remote_url_places_the_downloaded_file_at_an_explicit_destination() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/add-url-base:latest",
        &busybox,
        &["sh", "cat", "wc"],
        ContainerConfig::default(),
    );

    let addr = serve_one_response(
        "HTTP/1.1 200 OK\r\nContent-Length: 12\r\nConnection: close\r\n\r\nhello world!",
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        &format!(
            "FROM ociman-test/add-url-base:latest\n\
             ADD http://{addr}/greeting.txt /downloaded.txt\n"
        ),
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/add-url-result:latest",
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
            "ociman-test/add-url-result:latest",
            "--",
            "/bin/cat",
            "/downloaded.txt",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "hello world!");
}

/// `ADD` from a remote URL into a directory destination derives the
/// file name from the URL's own path, matching real BuildKit's own
/// `getFilenameForDownload`.
#[test]
fn add_from_remote_url_into_a_directory_derives_the_filename_from_the_url_path() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/add-url-dir-base:latest",
        &busybox,
        &["sh", "cat"],
        ContainerConfig::default(),
    );

    let addr =
        serve_one_response("HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc");

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        &format!(
            "FROM ociman-test/add-url-dir-base:latest\n\
             ADD http://{addr}/data/report.txt /app/\n"
        ),
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/add-url-dir-result:latest",
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
            "ociman-test/add-url-dir-result:latest",
            "--",
            "/bin/cat",
            "/app/report.txt",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "abc");
}

/// A directory destination with a URL that gives no derivable file
/// name at all (no path segment, no `Content-Disposition`) is a real,
/// clear, surfaced error -- matching real BuildKit's own `"cannot
/// determine filename for source"`.
#[test]
fn add_from_remote_url_with_no_derivable_filename_into_a_directory_is_an_error() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/add-url-noname-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let addr =
        serve_one_response("HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc");

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        &format!(
            "FROM ociman-test/add-url-noname-base:latest\n\
             ADD http://{addr}/ /app/\n"
        ),
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/add-url-noname-result:latest",
        ],
    );
    assert!(!build.status.success());
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(stderr.contains("cannot determine a file name"), "{stderr}");
    assert!(
        store
            .resolve_image("docker.io/ociman-test/add-url-noname-result:latest")
            .unwrap()
            .is_none(),
        "a failed build must not leave a partial image tagged"
    );
}

/// `ADD` shares `COPY`'s own multi-source support exactly, by design
/// (see `bin/ociman/src/build.rs`'s own module doc comment): multiple
/// explicit sources, each landing under the destination by its own
/// basename, requiring a trailing `/` on the destination.
#[test]
fn add_with_multiple_sources_places_each_under_its_own_basename() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/add-multi-base:latest",
        &busybox,
        &["sh", "cat"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    std::fs::write(context_dir.path().join("a.txt"), "aaa\n").unwrap();
    std::fs::write(context_dir.path().join("b.txt"), "bbb\n").unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/add-multi-base:latest\n\
         ADD a.txt b.txt /app/\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/add-multi-result:latest",
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
            "ociman-test/add-multi-result:latest",
            "--",
            "/bin/sh",
            "-c",
            "cat /app/a.txt /app/b.txt",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "aaa\nbbb\n");
}

/// The classic multi-stage pattern: build an artifact in one stage,
/// then `COPY --from=<that stage>` just the artifact into a fresh
/// final stage. The final image's own manifest must have exactly one
/// new layer beyond its own (fresh) base -- `builder`'s own separate
/// `RUN` layer never becomes part of the final image at all, and
/// `builder`'s other files (its own base image content aside) never
/// leak into the final rootfs either.
#[test]
fn copy_from_an_earlier_stage_copies_a_real_file_and_discards_the_rest_of_that_stage() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/copyfrom-base:latest",
        &busybox,
        &["sh", "cat", "ls"],
        ContainerConfig::default(),
    );
    let base_record = store
        .resolve_image("docker.io/ociman-test/copyfrom-base:latest")
        .unwrap()
        .unwrap();
    let base_manifest = store.image_manifest(&base_record).unwrap();

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/copyfrom-base:latest AS builder\n\
         RUN echo built-artifact > /app.bin\n\
         FROM ociman-test/copyfrom-base:latest\n\
         COPY --from=builder /app.bin /usr/local/bin/app.bin\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/copyfrom-result:latest",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let record = store
        .resolve_image("docker.io/ociman-test/copyfrom-result:latest")
        .unwrap()
        .unwrap();
    let manifest = store.image_manifest(&record).unwrap();
    // Exactly one new layer (the COPY) beyond the (fresh, unmodified)
    // base -- `builder`'s own RUN layer is never part of this image.
    assert_eq!(manifest.layers.len(), base_manifest.layers.len() + 1);

    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "--rm",
            "ociman-test/copyfrom-result:latest",
            "--",
            "/bin/sh",
            "-c",
            "cat /usr/local/bin/app.bin && (test -f /app.bin && echo LEAKED || echo CLEAN)",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "built-artifact\nCLEAN\n"
    );
}

#[test]
fn copy_from_rejects_a_name_that_is_not_any_earlier_stage() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/copyfrom-reject-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/copyfrom-reject-base:latest\n\
         COPY --from=Invalid_Reference_UPPERCASE!! /a.txt /b.txt\n",
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
    // Neither an earlier stage name nor a valid image reference --
    // `COPY --from=<external-image>` (see `copy_from_an_external_
    // image_pulls_and_copies_a_real_file` below) is genuinely
    // supported now, so a name that isn't a stage is only rejected
    // once it *also* fails to parse as a real image reference.
    assert!(
        stderr.contains("is neither an earlier stage")
            && stderr.contains("nor a valid image reference"),
        "{stderr}"
    );
}

/// `COPY --from=<external-image>` (a name that isn't any earlier
/// stage) pulls that image for real and copies from its own rootfs --
/// matching real BuildKit's own support for exactly this (`dispatchCopy`
/// resolves `--from` as a stage name first and otherwise falls through
/// to an ordinary image pull). Exercised entirely offline: the
/// "external" image is seeded into the same isolated test store ahead
/// of time (`seed_image_with_files`), so `resolve_or_pull` finds it
/// already present and never touches the network -- the same
/// established pattern every other test in this file already uses for
/// `FROM`.
#[test]
fn copy_from_an_external_image_pulls_and_copies_a_real_file() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/copyfrom-external-consumer:latest",
        &busybox,
        &["sh", "cat"],
        ContainerConfig::default(),
    );
    seed_image_with_files(
        &store,
        "ociman-test/copyfrom-external-source:latest",
        &busybox,
        &["sh"],
        &[("etc/distinctive-marker.txt", b"from the external image\n")],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/copyfrom-external-consumer:latest\n\
         COPY --from=ociman-test/copyfrom-external-source:latest \
         /etc/distinctive-marker.txt /marker.txt\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/copyfrom-external-result:latest",
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
            "ociman-test/copyfrom-external-result:latest",
            "--",
            "/bin/sh",
            "-c",
            "cat /marker.txt",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "from the external image\n"
    );
}

/// A stage referenced only via `COPY --from=` (not `FROM`) is still
/// built (it must be, for the `COPY` to have anything to read), but a
/// *third* stage nothing at all references is still pruned exactly
/// like the `FROM`-only pruning test above -- both kinds of cross-
/// stage reference feed the very same `stages_needed_for` pruning.
#[test]
fn an_unreferenced_stage_is_pruned_even_when_another_stage_is_used_only_via_copy_from() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/copyfrom-prune-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/copyfrom-prune-base:latest AS builder\n\
         RUN echo hi > /out.txt\n\
         FROM this-image-does-not-exist-anywhere:latest AS unrelated\n\
         FROM ociman-test/copyfrom-prune-base:latest\n\
         COPY --from=builder /out.txt /out.txt\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/copyfrom-prune-result:latest",
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
            "ociman-test/copyfrom-prune-result:latest",
            "--",
            "/bin/sh",
            "-c",
            "cat /out.txt",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "hi\n");
}

/// A real, end-to-end `--build-arg KEY=value`: overrides a declared
/// `ARG`'s own inline default, verbatim (not affecting an
/// un-overridden sibling `ARG`'s own default), confirmed by actually
/// running the built image and reading back the resulting `ENV` value
/// it flowed into.
#[test]
fn build_arg_overrides_a_declared_args_own_default() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/build-arg-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/build-arg-base:latest\n\
         ARG VERSION=1.0\n\
         ARG UNUSED_ARG=default\n\
         ENV APP_VERSION=${VERSION}\n\
         ENV UNUSED=${UNUSED_ARG}\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/build-arg-result:latest",
            "--build-arg",
            "VERSION=2.5",
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
            "ociman-test/build-arg-result:latest",
            "--",
            "/bin/sh",
            "-c",
            "echo $APP_VERSION $UNUSED",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "2.5 default\n");
}

/// A `--build-arg` for a name nothing in the Containerfile ever
/// declares has no effect at all (real `docker build`/`podman build`
/// both just silently ignore it for the purposes of expansion,
/// printing an "unconsumed build-arg" warning this project doesn't
/// implement yet -- see `bin/ociman/src/build.rs`'s own module doc
/// comment) -- the build still succeeds normally, using the
/// declared `ARG`'s own original default.
#[test]
fn build_arg_for_an_undeclared_name_has_no_effect() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/build-arg-undeclared-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/build-arg-undeclared-base:latest\n\
         ARG VERSION=1.0\n\
         ENV APP_VERSION=${VERSION}\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/build-arg-undeclared-result:latest",
            "--build-arg",
            "NEVER_DECLARED=xyz",
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
            "ociman-test/build-arg-undeclared-result:latest",
            "--",
            "/bin/sh",
            "-c",
            "echo $APP_VERSION",
        ],
    );
    assert!(run.status.success());
    assert_eq!(String::from_utf8_lossy(&run.stdout), "1.0\n");
}

/// A `--build-arg` for a name nothing declares also prints real
/// `docker build`/`podman build`'s own well-known `"[Warning] one or
/// more build-args ... were not consumed"` message (to stderr, not
/// mixed into the build's own stdout output) -- a successful build
/// still gets tagged; this is a warning, not a hard error.
#[test]
fn build_arg_for_an_undeclared_name_prints_the_real_unused_warning() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/build-arg-warn-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/build-arg-warn-base:latest\nARG VERSION=1.0\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/build-arg-warn-result:latest",
            "--build-arg",
            "NEVER_DECLARED=xyz",
            "--build-arg",
            "VERSION=2.0",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(
        stderr.contains("[Warning]") && stderr.contains("NEVER_DECLARED"),
        "{stderr}"
    );
    // The consumed `VERSION` override must *not* be listed as unused.
    assert!(!stderr.contains("\"VERSION\""), "{stderr}");
    assert!(
        store
            .resolve_image("docker.io/ociman-test/build-arg-warn-result:latest")
            .unwrap()
            .is_some(),
        "a build that only warns must still tag the image"
    );
}

/// The common, unremarkable case: every `--build-arg` given is
/// actually consumed, so no warning is printed at all.
#[test]
fn build_arg_prints_no_warning_when_every_override_is_consumed() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/build-arg-no-warn-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/build-arg-no-warn-base:latest\nARG VERSION=1.0\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/build-arg-no-warn-result:latest",
            "--build-arg",
            "VERSION=2.0",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    assert!(!String::from_utf8_lossy(&build.stderr).contains("[Warning]"));
}
