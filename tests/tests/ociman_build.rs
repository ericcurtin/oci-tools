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

#[test]
fn healthcheck_cmd_with_every_flag_is_stored_in_the_built_images_config() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/healthcheck-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/healthcheck-base:latest\n\
         HEALTHCHECK --interval=5s --timeout=3s --start-period=30s \
         --retries=3 CMD [\"echo\", \"ok\"]\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/healthcheck-built:latest",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let record = store
        .resolve_image("docker.io/ociman-test/healthcheck-built:latest")
        .unwrap()
        .unwrap();
    let config = store.image_config(&record).unwrap();
    let healthcheck = config
        .config
        .expect("container config")
        .healthcheck
        .expect("HEALTHCHECK should be recorded");
    assert_eq!(
        healthcheck.test,
        vec!["CMD".to_string(), "echo".to_string(), "ok".to_string()]
    );
    assert_eq!(healthcheck.interval, 5_000_000_000);
    assert_eq!(healthcheck.timeout, 3_000_000_000);
    assert_eq!(healthcheck.start_period, 30_000_000_000);
    assert_eq!(healthcheck.start_interval, 0);
    assert_eq!(healthcheck.retries, 3);
}

#[test]
fn healthcheck_none_overrides_a_base_images_own_healthcheck() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/healthcheck-none-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            healthcheck: Some(oci_spec_types::image::HealthcheckConfig {
                test: vec!["CMD-SHELL".to_string(), "true".to_string()],
                interval: 1_000_000_000,
                ..Default::default()
            }),
            ..Default::default()
        },
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/healthcheck-none-base:latest\nHEALTHCHECK NONE\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/healthcheck-none-built:latest",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let record = store
        .resolve_image("docker.io/ociman-test/healthcheck-none-built:latest")
        .unwrap()
        .unwrap();
    let config = store.image_config(&record).unwrap();
    let healthcheck = config
        .config
        .expect("container config")
        .healthcheck
        .expect("HEALTHCHECK NONE should still be recorded, not just dropped");
    assert_eq!(healthcheck.test, vec!["NONE".to_string()]);
    assert_eq!(healthcheck.interval, 0);
}

#[test]
fn healthcheck_with_an_invalid_flag_is_a_clear_build_error() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/healthcheck-bad-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/healthcheck-bad-base:latest\n\
         HEALTHCHECK --retries=-1 CMD [\"true\"]\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/healthcheck-bad-built:latest",
        ],
    );
    assert!(!build.status.success());
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(stderr.contains("cannot be negative"), "{stderr}");
}

/// End-to-end, real cross-build `ONBUILD`: the trigger is stored (not
/// run) by the build that declares it, then actually fires -- a real
/// new layer, a real file the trigger's own `RUN` created -- the
/// moment a *later*, separate build's own `FROM` resolves to that
/// image, matching real `docker build`/`podman build` exactly.
#[test]
fn onbuild_trigger_fires_in_a_later_build_using_this_image_as_its_base() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/onbuild-plain-base:latest",
        &busybox,
        &["sh", "cat"],
        ContainerConfig::default(),
    );

    // First build: declares ONBUILD, never runs it.
    let declaring_context = tempfile::tempdir().unwrap();
    write_containerfile(
        declaring_context.path(),
        "FROM ociman-test/onbuild-plain-base:latest\n\
         ONBUILD RUN echo hi > /onbuild-marker.txt\n",
    );
    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            declaring_context.path().to_str().unwrap(),
            "-t",
            "ociman-test/onbuild-declaring:latest",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let declaring_record = store
        .resolve_image("docker.io/ociman-test/onbuild-declaring:latest")
        .unwrap()
        .unwrap();
    let declaring_config = store.image_config(&declaring_record).unwrap();
    assert_eq!(
        declaring_config.config.unwrap().on_build,
        vec!["RUN echo hi > /onbuild-marker.txt".to_string()],
        "the declaring build itself must only ever store the trigger, never run it"
    );
    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "--rm",
            "ociman-test/onbuild-declaring:latest",
            "--",
            "/bin/sh",
            "-c",
            "test -f /onbuild-marker.txt && echo present || echo absent",
        ],
    );
    assert!(run.status.success());
    assert_eq!(String::from_utf8_lossy(&run.stdout), "absent\n");

    // Second, separate build: FROM the first build's own result, no
    // instructions of its own at all -- the ONBUILD trigger must
    // still fire, producing a real new layer with the real file its
    // own RUN step created.
    let child_context = tempfile::tempdir().unwrap();
    write_containerfile(
        child_context.path(),
        "FROM ociman-test/onbuild-declaring:latest\n",
    );
    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            child_context.path().to_str().unwrap(),
            "-t",
            "ociman-test/onbuild-child:latest",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let child_record = store
        .resolve_image("docker.io/ociman-test/onbuild-child:latest")
        .unwrap()
        .unwrap();
    let child_manifest = store.image_manifest(&child_record).unwrap();
    let declaring_manifest = store.image_manifest(&declaring_record).unwrap();
    assert_eq!(
        child_manifest.layers.len(),
        declaring_manifest.layers.len() + 1,
        "the fired ONBUILD trigger must produce exactly one real new layer"
    );
    // Consumed exactly once -- the child's own config carries no
    // ONBUILD of its own, so it never propagates to a hypothetical
    // grandchild build.
    let child_config = store.image_config(&child_record).unwrap();
    assert!(
        child_config
            .config
            .map(|cc| cc.on_build)
            .unwrap_or_default()
            .is_empty()
    );

    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "--rm",
            "ociman-test/onbuild-child:latest",
            "--",
            "/bin/cat",
            "/onbuild-marker.txt",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "hi\n");
}

/// A malformed `ONBUILD` trigger (anything past the trigger keyword
/// itself is never validated at declare time -- see `parse_onbuild`'s
/// own doc comment) surfaces as a real, clear error only once a later
/// build actually re-parses and fires it, not silently ignored or
/// misapplied.
#[test]
fn onbuild_trigger_with_an_unparseable_body_is_a_clear_error_when_it_fires() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/onbuild-bad-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let declaring_context = tempfile::tempdir().unwrap();
    write_containerfile(
        declaring_context.path(),
        "FROM ociman-test/onbuild-bad-base:latest\n\
         ONBUILD FROBNICATE something\n",
    );
    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            declaring_context.path().to_str().unwrap(),
            "-t",
            "ociman-test/onbuild-bad-declaring:latest",
        ],
    );
    assert!(
        build.status.success(),
        "declaring an ONBUILD trigger is never itself validated beyond its own keyword: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let child_context = tempfile::tempdir().unwrap();
    write_containerfile(
        child_context.path(),
        "FROM ociman-test/onbuild-bad-declaring:latest\n",
    );
    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            child_context.path().to_str().unwrap(),
            "-t",
            "ociman-test/onbuild-bad-child:latest",
        ],
    );
    assert!(!build.status.success());
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(
        stderr.contains("FROBNICATE") || stderr.contains("unknown instruction"),
        "{stderr}"
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

/// `--target <name>` builds only that named stage (and whatever it
/// depends on), never the stages after it in the file -- proven the
/// same way `an_unreferenced_stage_is_pruned_and_never_built` proves
/// ordinary pruning: the *later*, non-targeted stage has a `FROM`
/// reference to an image that doesn't exist anywhere, so the build
/// would fail outright if it were built at all.
#[test]
fn target_builds_only_the_named_stage_and_prunes_everything_after_it() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/target-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/target-base:latest AS builder\n\
         LABEL stage=builder\n\
         FROM this-image-does-not-exist-anywhere:latest AS final\n\
         LABEL stage=final\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/target-result:latest",
            "--target",
            "BUILDER", // deliberately mixed case -- matches real
                       // BuildKit's own case-insensitive stage-name
                       // matching.
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let record = store
        .resolve_image("docker.io/ociman-test/target-result:latest")
        .unwrap()
        .unwrap();
    let config = store.image_config(&record).unwrap();
    let labels = config.config.unwrap().labels;
    assert_eq!(labels.get("stage").map(String::as_str), Some("builder"));
}

/// A `--target` naming no real stage at all is a real, clear, surfaced
/// error -- matching real BuildKit's own `"target stage %q could not
/// be found"` wording -- not silently falling back to the last stage.
#[test]
fn target_naming_no_real_stage_is_a_clear_error() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/target-missing-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/target-missing-base:latest AS builder\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/target-missing-result:latest",
            "--target",
            "no-such-stage",
        ],
    );
    assert!(!build.status.success());
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(
        stderr.contains("target stage \"no-such-stage\" could not be found"),
        "{stderr}"
    );
    assert!(
        store
            .resolve_image("docker.io/ociman-test/target-missing-result:latest")
            .unwrap()
            .is_none(),
        "a failed build must not leave a partial image tagged"
    );
}

#[test]
fn from_scratch_builds_a_real_zero_base_layer_image_matching_real_docker_podman() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();

    let context_dir = tempfile::tempdir().unwrap();
    std::fs::copy(&busybox, context_dir.path().join("busybox")).unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM scratch\n\
         COPY busybox /bin/busybox\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/scratch-built:latest",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let record = store
        .resolve_image("docker.io/ociman-test/scratch-built:latest")
        .unwrap()
        .expect("built image should be recorded in the store");
    let manifest = store.image_manifest(&record).unwrap();
    // No base image at all -- the one real `COPY` is the *only* layer.
    assert_eq!(manifest.layers.len(), 1);

    let config = store.image_config(&record).unwrap();
    // Real, running-host platform info -- there is no base manifest to
    // have inherited it from the way every other stage's own config
    // does.
    assert_eq!(
        config.architecture.as_deref(),
        Some(
            oci_spec_types::image::Platform::host()
                .architecture
                .as_str()
        )
    );
    assert_eq!(config.os.as_deref(), Some("linux"));
    let cc = config.config.expect("container config");
    // Matches real `docker build`/`podman build`'s own observed
    // behavior: even a `FROM scratch` image gets a default `PATH`
    // baked in (checked directly against both real tools' own
    // `inspect` output for a real `FROM scratch` build).
    assert_eq!(
        cc.env,
        vec!["PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string()]
    );

    // Not just metadata -- the copied binary actually runs, in a
    // rootfs that started with nothing at all beyond what this one
    // `COPY` put there.
    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "--rm",
            "ociman-test/scratch-built:latest",
            "--",
            "/bin/busybox",
            "echo",
            "hello from scratch",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "hello from scratch\n");
}

#[test]
fn from_scratch_with_no_filesystem_touching_instructions_still_builds_a_real_empty_image() {
    // No `RUN`/`COPY`/`ADD` at all -- matches real `docker build`/
    // `podman build`: a `FROM scratch` stage with only metadata
    // instructions still produces a real, valid (if useless) image
    // with zero layers, rather than being rejected.
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM scratch\n\
         LABEL empty=\"yes\"\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/scratch-empty:latest",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let record = store
        .resolve_image("docker.io/ociman-test/scratch-empty:latest")
        .unwrap()
        .unwrap();
    let manifest = store.image_manifest(&record).unwrap();
    assert_eq!(manifest.layers.len(), 0);
    let config = store.image_config(&record).unwrap();
    assert_eq!(
        config
            .config
            .unwrap()
            .labels
            .get("empty")
            .map(String::as_str),
        Some("yes")
    );
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
        ("COPY --chmod=not-octal a.txt /a.txt\n", "--chmod"),
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

/// `COPY --chmod=<octal>` applies the exact same literal mode,
/// recursively, to every copied file *and* directory -- checked
/// directly against a real Docker daemon's own observed behavior (see
/// `docs/design/0079`): a directory source's own top-level directory,
/// every subdirectory, and every file inside all come back the exact
/// same mode, not just the top-level entry.
#[test]
fn copy_chmod_applies_the_same_octal_mode_recursively_to_every_copied_entry() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/chmod-copy-base:latest",
        &busybox,
        &["sh", "stat"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(context_dir.path().join("dir/sub")).unwrap();
    std::fs::write(context_dir.path().join("dir/top.txt"), "top").unwrap();
    std::fs::write(context_dir.path().join("dir/sub/nested.txt"), "nested").unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/chmod-copy-base:latest\nCOPY --chmod=0741 dir /copied\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/chmod-copy-result:latest",
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
            "ociman-test/chmod-copy-result:latest",
            "--",
            "/bin/sh",
            "-c",
            "stat -c '%a' /copied /copied/top.txt /copied/sub /copied/sub/nested.txt",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "741\n741\n741\n741\n");
}

/// `--chmod` is deliberately *not* applied when a local `ADD` source
/// is auto-extracted as a real archive -- checked directly against a
/// real Docker daemon's own observed behavior (see
/// `docs/design/0079`): flattening a real archive's own varied,
/// individually-meaningful per-entry permissions to one single mode
/// would be destructive, not a real feature.
#[test]
fn add_chmod_does_not_apply_to_auto_extracted_archive_contents() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/chmod-archive-base:latest",
        &busybox,
        &["sh", "stat"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    let archive_path = context_dir.path().join("payload.tar.gz");
    write_gzip_tar(&archive_path, &[("inside.txt", b"archived content")]);
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/chmod-archive-base:latest\n\
         ADD --chmod=0741 payload.tar.gz /extracted\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/chmod-archive-result:latest",
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
            "ociman-test/chmod-archive-result:latest",
            "--",
            "/bin/sh",
            "-c",
            "stat -c '%a' /extracted/inside.txt",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    // The tar entry's own mode (0644, set by `write_gzip_tar`), not
    // 0741 -- proves --chmod was correctly *not* applied here.
    assert_eq!(String::from_utf8_lossy(&run.stdout), "644\n");
}

/// `--chown=<uid>:<gid>` resolves numerically (no `/etc/passwd`
/// lookup needed) and is reflected in the committed layer's own real
/// tar header — checked directly against a real Docker daemon on this
/// host before writing this test (`docs/design/0097`). Uses the
/// *calling test process's own* real uid/gid: the only value
/// guaranteed to succeed regardless of whether this test happens to
/// run rootless or as real root (an arbitrary different uid needs
/// `CAP_CHOWN`, tolerated-not-fatal when missing — see
/// `chown_to_a_different_uid_is_tolerated_not_fatal_when_unprivileged`
/// below for that case).
#[test]
fn copy_chown_is_reflected_in_the_committed_layers_own_tar_header() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/chown-copy-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let uid = rustix::process::getuid().as_raw();
    let gid = rustix::process::getgid().as_raw();

    let context_dir = tempfile::tempdir().unwrap();
    std::fs::write(context_dir.path().join("file.txt"), "hello").unwrap();
    write_containerfile(
        context_dir.path(),
        &format!(
            "FROM ociman-test/chown-copy-base:latest\nCOPY --chown={uid}:{gid} file.txt /file.txt\n"
        ),
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/chown-copy-result:latest",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let entries = last_layer_tar_entries(&store, "ociman-test/chown-copy-result:latest");
    let entry = entries
        .iter()
        .find(|(path, _, _)| path == "file.txt")
        .unwrap_or_else(|| panic!("no file.txt entry in {entries:?}"));
    assert_eq!(entry.1, uid, "{entries:?}");
    assert_eq!(entry.2, gid, "{entries:?}");
}

/// A rootless build's `--chown` to a uid that isn't the calling
/// process's own is tolerated, not fatal — squarely this project's
/// own already-established rootless single-uid-mapping limitation
/// (the same one `-v`/`--volume`'s own bind-mount ownership and
/// `oci_layer::apply`'s own extraction-time ownership already have),
/// not a new one. Skipped outright when running as real root (uid 0),
/// where the chown would simply succeed instead.
#[test]
fn chown_to_a_different_uid_is_tolerated_not_fatal_when_unprivileged() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    if rustix::process::getuid().as_raw() == 0 {
        eprintln!("skipping: running as real root, --chown would simply succeed");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/chown-unprivileged-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    std::fs::write(context_dir.path().join("file.txt"), "hello").unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/chown-unprivileged-base:latest\n\
         COPY --chown=54321:54321 file.txt /file.txt\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/chown-unprivileged-result:latest",
        ],
    );
    assert!(
        build.status.success(),
        "an unprivileged --chown to a different uid must be tolerated, not fail the build: {}",
        String::from_utf8_lossy(&build.stderr)
    );
}

/// Unlike `--chmod` (deliberately *not* applied to an archive's own
/// extracted contents), real Docker's own `--chown` **does** apply to
/// `ADD`'s auto-extracted archive contents — checked directly against
/// a real Docker daemon on this host before writing this test
/// (`ADD --chown=2000:2000 some.tar.gz /dest` overrides the archive's
/// own recorded per-entry ownership throughout `/dest`, unlike
/// `--chmod`'s own real, verified "leaves per-entry modes alone"
/// behavior). Uses the calling process's own real uid/gid, for the
/// same reason `copy_chown_is_reflected_in_the_committed_layers_own_
/// tar_header` does.
#[test]
fn add_chown_does_apply_to_auto_extracted_archive_contents() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/chown-archive-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let uid = rustix::process::getuid().as_raw();
    let gid = rustix::process::getgid().as_raw();

    let context_dir = tempfile::tempdir().unwrap();
    let archive_path = context_dir.path().join("payload.tar.gz");
    write_gzip_tar(&archive_path, &[("inside.txt", b"archived content")]);
    write_containerfile(
        context_dir.path(),
        &format!(
            "FROM ociman-test/chown-archive-base:latest\n\
             ADD --chown={uid}:{gid} payload.tar.gz /extracted\n"
        ),
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/chown-archive-result:latest",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let entries = last_layer_tar_entries(&store, "ociman-test/chown-archive-result:latest");
    for path in ["extracted", "extracted/inside.txt"] {
        let entry = entries
            .iter()
            .find(|(p, _, _)| p.trim_end_matches('/') == path)
            .unwrap_or_else(|| panic!("no {path:?} entry in {entries:?}"));
        assert_eq!(entry.1, uid, "{path}: {entries:?}");
        assert_eq!(entry.2, gid, "{path}: {entries:?}");
    }
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

/// Read `reference`'s own *last* real committed layer back — the
/// exact same real, gzip-compressed tar bytes `ociman build` itself
/// wrote via `commit_layer` — returning `(path, uid, gid)` for every
/// real entry. Used to verify `--chown` landed in the committed
/// layer's own tar header: running the *built* container itself would
/// never show this, since `oci_layer::apply`'s own already-documented
/// extraction-time limitation never `chown`s on the way in (see
/// `set_owner`'s own doc comment in `build.rs`) — the committed
/// layer's own bytes are the only place `--chown`'s real effect is
/// actually observable from outside this project.
fn last_layer_tar_entries(store: &Store, reference: &str) -> Vec<(String, u32, u32)> {
    // `resolve_image` looks up the *stored*, already-normalized
    // reference exactly (`docker.io/<repo>:<tag>`, matching how
    // `Reference::parse(...).to_string()` stores it in the first
    // place) -- re-normalize here too, so callers can pass the same
    // short form they gave `-t` on the command line.
    let normalized = oci_spec_types::Reference::parse(reference)
        .unwrap()
        .to_string();
    let record = store.resolve_image(&normalized).unwrap().unwrap();
    let manifest = store.image_manifest(&record).unwrap();
    let digest = &manifest.layers.last().unwrap().digest;
    let blob = store.open_blob(digest).unwrap();
    let decoder = flate2::read::GzDecoder::new(blob);
    let mut archive = tar::Archive::new(decoder);
    archive
        .entries()
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let path = entry.path().unwrap().to_string_lossy().into_owned();
            let uid = entry.header().uid().unwrap() as u32;
            let gid = entry.header().gid().unwrap() as u32;
            (path, uid, gid)
        })
        .collect()
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

/// `--chmod` overrides the default `0o600` mode for a downloaded `ADD`
/// URL source too -- checked directly against a real Docker daemon's
/// own observed behavior (see `docs/design/0079`).
#[test]
fn add_chmod_overrides_the_default_mode_for_a_downloaded_url_source() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/chmod-url-base:latest",
        &busybox,
        &["sh", "stat"],
        ContainerConfig::default(),
    );

    let addr =
        serve_one_response("HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc");

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        &format!(
            "FROM ociman-test/chmod-url-base:latest\n\
             ADD --chmod=0741 http://{addr}/greeting.txt /downloaded.txt\n"
        ),
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/chmod-url-result:latest",
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
            "ociman-test/chmod-url-result:latest",
            "--",
            "/bin/sh",
            "-c",
            "stat -c '%a' /downloaded.txt",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "741\n");
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

/// The build cache (`build_cache` module): rebuilding the exact same
/// `RUN`/`COPY` steps against the exact same base image and build
/// context reuses the *first* build's own already-stored layers
/// verbatim, rather than recomputing them.
///
/// This is checked the only way that actually proves reuse happened
/// rather than merely "producing the same result again by
/// coincidence": a `RUN` step's own committed layer is a real tar
/// archive, and `/proc/sys/kernel/random/uuid` is a real kernel
/// interface that returns a genuinely fresh random UUID on every
/// single read (unlike, say, a PID -- which a fresh, single-process
/// container's own shell would almost always see as `1` regardless of
/// how many times it's really launched, or a whole-second mtime,
/// which two builds running within the same wall-clock second could
/// otherwise coincidentally share) -- so two genuinely *separate* real
/// executions of `cat /proc/sys/kernel/random/uuid > /marker.txt` are
/// certain to produce two different layer digests. If the second
/// build's own manifest layer digests come back byte-for-byte
/// identical to the first build's own, the second `RUN` provably
/// never actually ran a second time.
#[test]
fn rebuilding_the_same_containerfile_reuses_previously_built_layers() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/cache-base:latest",
        &busybox,
        &["sh", "cat"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    std::fs::write(context_dir.path().join("app.txt"), b"hello cache").unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/cache-base:latest\n\
         RUN cat /proc/sys/kernel/random/uuid > /marker.txt\n\
         COPY app.txt /app.txt\n",
    );

    let first = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/cache-first:latest",
        ],
    );
    assert!(
        first.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );

    // A real, separate process boundary between the two builds --
    // not strictly required for the PID/mtime argument above, but
    // makes "these really are two independent invocations" explicit.
    let second = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/cache-second:latest",
        ],
    );
    assert!(
        second.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );

    let first_record = store
        .resolve_image("docker.io/ociman-test/cache-first:latest")
        .unwrap()
        .unwrap();
    let second_record = store
        .resolve_image("docker.io/ociman-test/cache-second:latest")
        .unwrap()
        .unwrap();
    let first_manifest = store.image_manifest(&first_record).unwrap();
    let second_manifest = store.image_manifest(&second_record).unwrap();

    assert_eq!(
        first_manifest.layers, second_manifest.layers,
        "the second build should have reused every one of the first build's own layers, not \
         recomputed them"
    );

    let first_config = store.image_config(&first_record).unwrap();
    let second_config = store.image_config(&second_record).unwrap();
    assert_eq!(first_config.rootfs.diff_ids, second_config.rootfs.diff_ids);
    assert_eq!(first_config.history.len(), 2);
    assert_eq!(second_config.history.len(), 2);
    assert_eq!(first_config.history, second_config.history);
}

/// `--no-cache` disables the reuse [`rebuilding_the_same_containerfile_
/// reuses_previously_built_layers`] just proved happens by default:
/// with `--no-cache`, a rebuild of the identical `RUN` step actually
/// re-executes, producing a genuinely different layer digest (same
/// real, genuinely-random `/proc/sys/kernel/random/uuid` argument as
/// that test, in reverse).
#[test]
fn no_cache_forces_a_real_re_execution_instead_of_reusing_a_cached_layer() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/no-cache-base:latest",
        &busybox,
        &["sh", "cat"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/no-cache-base:latest\nRUN cat /proc/sys/kernel/random/uuid > /marker.txt\n",
    );

    let first = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/no-cache-first:latest",
        ],
    );
    assert!(first.status.success());

    let second = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/no-cache-second:latest",
            "--no-cache",
        ],
    );
    assert!(second.status.success());

    let first_record = store
        .resolve_image("docker.io/ociman-test/no-cache-first:latest")
        .unwrap()
        .unwrap();
    let second_record = store
        .resolve_image("docker.io/ociman-test/no-cache-second:latest")
        .unwrap()
        .unwrap();
    let first_manifest = store.image_manifest(&first_record).unwrap();
    let second_manifest = store.image_manifest(&second_record).unwrap();

    assert_ne!(
        first_manifest.layers, second_manifest.layers,
        "--no-cache must re-run RUN for real, producing a genuinely new layer"
    );
}

/// A `COPY` whose source content actually changed between two builds
/// must never be served from the cache, even though the instruction's
/// own literal text (`COPY app.txt /app.txt`) is unchanged — the real
/// content digest folded into `created_by` (see the `build_cache`
/// module's own doc comment) is what makes this distinction, not the
/// Containerfile text alone.
#[test]
fn a_copy_source_whose_content_changed_is_not_served_from_the_cache() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/copy-cache-base:latest",
        &busybox,
        &["sh", "cat"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    std::fs::write(context_dir.path().join("app.txt"), b"version one").unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/copy-cache-base:latest\nCOPY app.txt /app.txt\n",
    );

    let first = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/copy-cache-first:latest",
        ],
    );
    assert!(first.status.success());

    // Same instruction text, genuinely different content.
    std::fs::write(
        context_dir.path().join("app.txt"),
        b"version two -- changed!",
    )
    .unwrap();
    let second = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/copy-cache-second:latest",
        ],
    );
    assert!(second.status.success());

    let first_record = store
        .resolve_image("docker.io/ociman-test/copy-cache-first:latest")
        .unwrap()
        .unwrap();
    let second_record = store
        .resolve_image("docker.io/ociman-test/copy-cache-second:latest")
        .unwrap()
        .unwrap();
    let first_manifest = store.image_manifest(&first_record).unwrap();
    let second_manifest = store.image_manifest(&second_record).unwrap();

    assert_ne!(
        first_manifest.layers, second_manifest.layers,
        "changed COPY source content must bust the cache, not reuse the stale layer"
    );

    // And the built image really does contain the *new* content, not
    // a stale cached copy.
    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "--rm",
            "ociman-test/copy-cache-second:latest",
            "--",
            "cat",
            "/app.txt",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "version two -- changed!"
    );
}

/// A change earlier in the instruction sequence (a different `RUN`)
/// is never served from the cache — matching real Docker/BuildKit's
/// own "one miss invalidates everything after it" cache semantics
/// (real buildah's own `historyAndDiffIDsMatch` is a full-history-
/// prefix match, not a per-instruction lookup independent of
/// position; see the `build_cache` module's own doc comment).
///
/// Only the changed `RUN` step's own layer is asserted here, not the
/// unchanged `COPY` after it: unlike `RUN`'s own real, essentially
/// unique-per-execution output (see this file's own other cache
/// tests), copying the exact same, unchanged source file always
/// produces byte-identical tar bytes regardless of whether real
/// copying happened again or a cache lookup skipped it — a
/// genuinely-re-run `COPY` of unchanged content is indistinguishable
/// from a cache hit by digest alone, in *any* per-instruction-diff
/// architecture (this project's own included), so it isn't a useful
/// signal for "was the cache consulted here" the way `RUN`'s own
/// PID-dependent output is.
#[test]
fn a_change_earlier_in_the_file_busts_the_cache_for_every_later_step_too() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/prefix-cache-base:latest",
        &busybox,
        &["sh", "mkdir"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    std::fs::write(context_dir.path().join("app.txt"), b"unchanged content").unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/prefix-cache-base:latest\n\
         RUN mkdir -p /one\n\
         COPY app.txt /app.txt\n",
    );

    let first = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/prefix-cache-first:latest",
        ],
    );
    assert!(first.status.success());

    // Change only the *first* RUN's own command text; the COPY step
    // (and its source content) is completely unchanged.
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/prefix-cache-base:latest\n\
         RUN mkdir -p /one-different\n\
         COPY app.txt /app.txt\n",
    );
    let second = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/prefix-cache-second:latest",
        ],
    );
    assert!(second.status.success());

    let first_record = store
        .resolve_image("docker.io/ociman-test/prefix-cache-first:latest")
        .unwrap()
        .unwrap();
    let second_record = store
        .resolve_image("docker.io/ociman-test/prefix-cache-second:latest")
        .unwrap()
        .unwrap();
    let first_manifest = store.image_manifest(&first_record).unwrap();
    let second_manifest = store.image_manifest(&second_record).unwrap();

    // The changed RUN step is never served from the cache -- it's a
    // real, genuinely different instruction, so there's nothing to
    // even consider reusing.
    assert_ne!(first_manifest.layers[1], second_manifest.layers[1]);
}
