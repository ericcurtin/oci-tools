//! `ociman build` integration tests: the first working end-to-end
//! `build` command (see `bin/ociman/src/build.rs`'s own doc comment
//! for this first increment's deliberately narrow scope). Same fully
//! offline approach as `ociman_ps.rs`/`ociman_run.rs` (a synthetic-
//! but-structurally-real seeded base image via `seed_image`, no
//! registry access needed).

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

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

/// `--label KEY=VALUE` (repeatable) applies *after* every real
/// Containerfile `LABEL` instruction -- overriding a same-key `LABEL`
/// already there, adding any brand new key, and leaving every other
/// `LABEL` untouched -- matching real `podman build --label` exactly,
/// confirmed directly.
#[test]
fn label_flag_overrides_a_same_key_dockerfile_label_and_adds_a_new_one() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/label-flag-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/label-flag-base:latest\n\
         LABEL foo=from-dockerfile\n\
         LABEL untouched=still-here\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/label-flag-result:latest",
            "--label",
            "foo=from-cli",
            "--label",
            "brand-new=only-from-cli",
            "--label",
            "bareword",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let record = store
        .resolve_image("docker.io/ociman-test/label-flag-result:latest")
        .unwrap()
        .unwrap();
    let config = store.image_config(&record).unwrap();
    let labels = config.config.unwrap().labels;
    assert_eq!(labels.get("foo").map(String::as_str), Some("from-cli"));
    assert_eq!(
        labels.get("untouched").map(String::as_str),
        Some("still-here")
    );
    assert_eq!(
        labels.get("brand-new").map(String::as_str),
        Some("only-from-cli")
    );
    assert_eq!(labels.get("bareword").map(String::as_str), Some(""));
}

/// No `--label` at all leaves the built image's own history exactly
/// as it would have been without this flag at all -- no extra, empty
/// "LABEL " history entry ever gets recorded when there's nothing to
/// apply.
#[test]
fn no_label_flag_at_all_adds_no_extra_history_entry() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/no-label-flag-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/no-label-flag-base:latest\nLABEL foo=bar\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/no-label-flag-result:latest",
        ],
    );
    assert!(build.status.success());

    let record = store
        .resolve_image("docker.io/ociman-test/no-label-flag-result:latest")
        .unwrap()
        .unwrap();
    let config = store.image_config(&record).unwrap();
    let label_history_entries: Vec<&str> = config
        .history
        .iter()
        .filter_map(|h| h.created_by.as_deref())
        .filter(|created_by| created_by.starts_with("LABEL "))
        .collect();
    // Exactly the one real `LABEL foo=bar` from the Containerfile
    // itself -- no second, synthetic `--label`-driven entry.
    assert_eq!(label_history_entries, vec!["LABEL foo=bar"]);
}

/// `--annotation KEY=VALUE` (repeatable, bare `KEY` means an empty
/// value) sets the built *manifest's* own top-level `annotations` --
/// distinct from `--label`, which sets `Config.Labels` instead --
/// matching real `podman build --annotation` exactly, confirmed
/// directly against the real pushed manifest's own raw JSON.
#[test]
fn annotation_flag_sets_the_built_manifests_own_top_level_annotations() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/annotation-flag-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/annotation-flag-base:latest\nLABEL foo=a-label-not-an-annotation\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/annotation-flag-result:latest",
            "--annotation",
            "foo=bar",
            "--annotation",
            "bareword",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let record = store
        .resolve_image("docker.io/ociman-test/annotation-flag-result:latest")
        .unwrap()
        .unwrap();
    let manifest = store.image_manifest(&record).unwrap();
    assert_eq!(
        manifest.annotations.get("foo").map(String::as_str),
        Some("bar")
    );
    assert_eq!(
        manifest.annotations.get("bareword").map(String::as_str),
        Some("")
    );

    // `--annotation` never touches `Config.Labels` at all -- the real
    // `LABEL foo=...` from the Containerfile stays exactly what it
    // was, untouched by the *different* `foo=bar` given to
    // `--annotation`.
    let config = store.image_config(&record).unwrap();
    assert_eq!(
        config.config.unwrap().labels.get("foo").map(String::as_str),
        Some("a-label-not-an-annotation")
    );
}

/// No `--annotation` at all leaves the built manifest's own
/// `annotations` empty, same as before this flag existed.
#[test]
fn no_annotation_flag_at_all_leaves_manifest_annotations_empty() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/no-annotation-flag-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/no-annotation-flag-base:latest\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/no-annotation-flag-result:latest",
        ],
    );
    assert!(build.status.success());

    let record = store
        .resolve_image("docker.io/ociman-test/no-annotation-flag-result:latest")
        .unwrap()
        .unwrap();
    let manifest = store.image_manifest(&record).unwrap();
    assert!(manifest.annotations.is_empty());
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

/// `-t`/`--tag` is optional — matching real `docker build`/`podman
/// build` with no `-t` at all: the build still succeeds, the image is
/// still fully usable (by ID), it just has no tag pointing at it (see
/// `docs/design/0179`). The human-readable output shows only the
/// digest, with no "tagged: ..." line at all (nothing to report);
/// `--json` shows `"reference": null`, never this project's own
/// internal untagged-sentinel string.
#[test]
fn build_with_no_tag_at_all_still_succeeds_and_records_an_untagged_image() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/untagged-build-base:latest",
        &busybox,
        &["sh", "cat"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/untagged-build-base:latest\nRUN echo hi > /hi.txt\n",
    );

    let build = ociman(
        storage_dir.path(),
        &["build", context_dir.path().to_str().unwrap()],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let stdout = String::from_utf8_lossy(&build.stdout);
    assert!(
        !stdout.contains("tagged:"),
        "an untagged build should never print a \"tagged: ...\" line: {stdout:?}"
    );
    let digest = stdout.trim().to_string();
    assert!(digest.starts_with("sha256:"), "{stdout:?}");

    let json_build = ociman(
        storage_dir.path(),
        &["build", "--json", context_dir.path().to_str().unwrap()],
    );
    assert!(json_build.status.success());
    let view: serde_json::Value = serde_json::from_slice(&json_build.stdout).unwrap();
    assert_eq!(view["reference"], serde_json::Value::Null);

    // Findable by ID afterward -- `inspect`'s own existing ID fallback
    // (0122) needs no changes at all to already work here.
    let short_id = &digest.trim_start_matches("sha256:")[..12];
    let inspect = ociman(storage_dir.path(), &["inspect", short_id]);
    assert!(
        inspect.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&inspect.stderr)
    );

    // `ociman images` shows the real, honest "<none>" placeholder,
    // never this project's own internal sentinel string, matching
    // real `docker images`/`podman images`'s own identical convention
    // for an untagged image.
    let images = ociman(storage_dir.path(), &["images"]);
    assert!(images.status.success());
    let stdout = String::from_utf8_lossy(&images.stdout);
    assert!(stdout.contains("<none>"), "{stdout}");
    assert!(!stdout.contains("sha256:"), "{stdout}");

    let images_json = ociman(storage_dir.path(), &["images", "--json"]);
    assert!(images_json.status.success());
    let views: serde_json::Value = serde_json::from_slice(&images_json.stdout).unwrap();
    let untagged = views
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["digest"] == digest)
        .expect("the untagged image should still show up in the listing");
    assert_eq!(untagged["reference"], serde_json::Value::Null);
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

/// `ADD --checksum=sha256:<hex>` on a remote-URL source: a matching
/// checksum lets the build succeed exactly like it would without
/// `--checksum` at all.
#[test]
fn add_checksum_matching_the_real_content_succeeds() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/add-checksum-ok-base:latest",
        &busybox,
        &["sh", "cat"],
        ContainerConfig::default(),
    );

    let addr = serve_one_response(
        "HTTP/1.1 200 OK\r\nContent-Length: 12\r\nConnection: close\r\n\r\nhello world!",
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        &format!(
            "FROM ociman-test/add-checksum-ok-base:latest\n\
             ADD --checksum=sha256:7509e5bda0c762d2bac7f90d758b5b2263fa01ccbc542ab5e3df163be08e6ca9 \
             http://{addr}/greeting.txt /downloaded.txt\n"
        ),
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/add-checksum-ok-result:latest",
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
            "ociman-test/add-checksum-ok-result:latest",
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

/// `ADD --checksum=` on a remote-URL source whose real content doesn't
/// match is a hard build error -- no partial write, no layer
/// committed, no image tagged -- matching real buildah's own identical
/// all-or-nothing behavior (checked directly against `~/git/podman/
/// vendor/go.podman.io/buildah/add.go`'s own `getURL`).
#[test]
fn add_checksum_mismatch_is_a_clear_error_and_taints_nothing() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/add-checksum-bad-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let addr = serve_one_response(
        "HTTP/1.1 200 OK\r\nContent-Length: 12\r\nConnection: close\r\n\r\nhello world!",
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        &format!(
            "FROM ociman-test/add-checksum-bad-base:latest\n\
             ADD --checksum=sha256:0000000000000000000000000000000000000000000000000000000000000000 \
             http://{addr}/greeting.txt /downloaded.txt\n"
        ),
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/add-checksum-bad-result:latest",
        ],
    );
    assert!(!build.status.success());
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(stderr.contains("checksum mismatch"), "{stderr}");
    assert!(
        store
            .resolve_image("docker.io/ociman-test/add-checksum-bad-result:latest")
            .unwrap()
            .is_none(),
        "a failed checksum must not leave a partial image tagged"
    );
}

/// `--checksum` is only ever legal for exactly one, remote-URL source
/// -- combined with a local build-context source, it's a clear,
/// structural build error raised before anything is even downloaded,
/// matching real BuildKit's own `dispatchCopy` (`~/git/moby/vendor/
/// github.com/moby/buildkit/dockerfile2llb/convert_copy.go`).
#[test]
fn add_checksum_with_a_local_source_is_a_clear_error() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/add-checksum-local-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    std::fs::write(context_dir.path().join("local.txt"), "local content").unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/add-checksum-local-base:latest\n\
         ADD --checksum=sha256:0000000000000000000000000000000000000000000000000000000000000000 \
         local.txt /dest.txt\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/add-checksum-local-result:latest",
        ],
    );
    assert!(!build.status.success());
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(
        stderr.contains("can only be specified for a single, remote-URL source"),
        "{stderr}"
    );
}

/// `--checksum` combined with more than one remote-URL source is the
/// same structural error, not silently applied to just the first one.
#[test]
fn add_checksum_with_multiple_sources_is_a_clear_error() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/add-checksum-multi-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let addr1 =
        serve_one_response("HTTP/1.1 200 OK\r\nContent-Length: 1\r\nConnection: close\r\n\r\na");
    let addr2 =
        serve_one_response("HTTP/1.1 200 OK\r\nContent-Length: 1\r\nConnection: close\r\n\r\nb");

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        &format!(
            "FROM ociman-test/add-checksum-multi-base:latest\n\
             ADD --checksum=sha256:0000000000000000000000000000000000000000000000000000000000000000 \
             http://{addr1}/a.txt http://{addr2}/b.txt /dest/\n"
        ),
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/add-checksum-multi-result:latest",
        ],
    );
    assert!(!build.status.success());
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(
        stderr.contains("can only be specified for a single, remote-URL source"),
        "{stderr}"
    );
}

/// A malformed `--checksum` value fails fast, before any network
/// access at all (only one mock server started, and it's never even
/// contacted if the flag itself is bad).
#[test]
fn add_checksum_with_bad_syntax_is_a_clear_error() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/add-checksum-badsyntax-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/add-checksum-badsyntax-base:latest\n\
         ADD --checksum=not-a-real-digest http://example.invalid/f /dest.txt\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/add-checksum-badsyntax-result:latest",
        ],
    );
    assert!(!build.status.success());
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(stderr.contains("--checksum"), "{stderr}");
}

/// `--checksum=sha512:...` is structurally a valid digest (`Digest::
/// parse` accepts it) but this project's own `oci_spec_types::digest`
/// deliberately never produces a `sha512` hash at all -- matching real
/// Docker's own public documentation, which states `--checksum`
/// "currently only" supports `sha256`, this is a clear, immediate
/// error rather than a silently-unenforceable checksum.
#[test]
fn add_checksum_sha512_is_a_clear_unsupported_algorithm_error() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/add-checksum-sha512-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        &format!(
            "FROM ociman-test/add-checksum-sha512-base:latest\n\
             ADD --checksum=sha512:{} http://example.invalid/f /dest.txt\n",
            "0".repeat(128)
        ),
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/add-checksum-sha512-result:latest",
        ],
    );
    assert!(!build.status.success());
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(stderr.contains("only sha256 is supported"), "{stderr}");
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

/// Real Docker/BuildKit rule, checked directly against real `podman
/// build` too (an identical Containerfile against a real installed
/// `podman`, `$VERSION` really does resolve to `1.0` inside the `RUN`
/// step): a declared `ARG`'s own value is injected into a *later*
/// `RUN` step's own temporary process environment, so the shell
/// running inside it can `$VAR`-expand it -- `oci_dockerfile::
/// expand_stage` deliberately never touches `RUN`'s own command-line
/// text at build-time (same as real Docker), so this only works if
/// `ociman build` actually sets up that runtime environment, not
/// through any string substitution.
#[test]
fn run_step_sees_a_declared_args_own_value_in_its_own_shell_environment() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/run-sees-arg-base:latest",
        &busybox,
        &["sh", "cat"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/run-sees-arg-base:latest\n\
         ARG VERSION=1.0\n\
         RUN echo \"VERSION is [$VERSION]\" > /result.txt\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/run-sees-arg-result:latest",
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
            "ociman-test/run-sees-arg-result:latest",
            "--",
            "cat",
            "/result.txt",
        ],
    );
    assert!(run.status.success());
    assert_eq!(String::from_utf8_lossy(&run.stdout), "VERSION is [1.0]\n");

    // Never persisted into the final image's own ENV -- matches real
    // Docker exactly: an ARG's value only survives past the build if
    // a later ENV instruction explicitly re-declares it, which this
    // Containerfile never does.
    let record = store
        .resolve_image("docker.io/ociman-test/run-sees-arg-result:latest")
        .unwrap()
        .unwrap();
    let config = store.image_config(&record).unwrap();
    assert!(
        !config
            .config
            .unwrap()
            .env
            .iter()
            .any(|kv| kv.starts_with("VERSION="))
    );
}

/// A `--build-arg` override changes what a `RUN` step's own shell
/// actually sees, *and* correctly busts the local build cache for it
/// -- rebuilding the exact same Containerfile with a different
/// `--build-arg` value must not silently reuse an earlier build's own
/// now-stale layer (its own file content really did depend on the
/// old value).
#[test]
fn build_arg_override_changes_what_run_sees_and_busts_the_cache() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/run-arg-cache-base:latest",
        &busybox,
        &["sh", "cat"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/run-arg-cache-base:latest\n\
         ARG VERSION=1.0\n\
         RUN echo \"VERSION is [$VERSION]\" > /result.txt\n",
    );

    let build_one = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/run-arg-cache-v1:latest",
        ],
    );
    assert!(build_one.status.success());

    let build_two = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/run-arg-cache-v2:latest",
            "--build-arg",
            "VERSION=2.0",
        ],
    );
    assert!(
        build_two.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build_two.stderr)
    );

    let record_one = store
        .resolve_image("docker.io/ociman-test/run-arg-cache-v1:latest")
        .unwrap()
        .unwrap();
    let record_two = store
        .resolve_image("docker.io/ociman-test/run-arg-cache-v2:latest")
        .unwrap()
        .unwrap();
    let manifest_one = store.image_manifest(&record_one).unwrap();
    let manifest_two = store.image_manifest(&record_two).unwrap();
    assert_ne!(
        manifest_one.layers.last(),
        manifest_two.layers.last(),
        "a different --build-arg value must produce a genuinely different RUN layer, not a stale cache hit"
    );

    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "--rm",
            "ociman-test/run-arg-cache-v2:latest",
            "--",
            "cat",
            "/result.txt",
        ],
    );
    assert!(run.status.success());
    assert_eq!(String::from_utf8_lossy(&run.stdout), "VERSION is [2.0]\n");
}

/// Real Docker rule, checked directly (`BuildArgs.FilterAllowed`): an
/// `ARG` sharing a name with an explicit `ENV` never overrides it in a
/// `RUN` step's own environment -- the `ENV` value always wins.
#[test]
fn arg_never_overrides_an_explicit_env_with_the_same_name_in_a_run_step() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/arg-env-precedence-base:latest",
        &busybox,
        &["sh", "cat"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/arg-env-precedence-base:latest\n\
         ARG FOO=from-arg\n\
         ENV FOO=from-env\n\
         RUN echo \"FOO is [$FOO]\" > /result.txt\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/arg-env-precedence-result:latest",
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
            "ociman-test/arg-env-precedence-result:latest",
            "--",
            "cat",
            "/result.txt",
        ],
    );
    assert!(run.status.success());
    assert_eq!(String::from_utf8_lossy(&run.stdout), "FOO is [from-env]\n");
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

/// A change *inside* a `.dockerignore`-excluded directory must never
/// bust the cache for a `COPY` step that reads from the context --
/// that content is never actually copied in the first place, so
/// there's nothing for the cache to correctly invalidate over (the
/// real bug this test guards against: `build_cache::content_digest`
/// used to hash every byte under a `COPY` source unconditionally,
/// `.dockerignore` or not, wasting real time on content that would
/// never even be read otherwise, and busting the cache for a layer
/// whose own actually-copied content never changed at all).
#[test]
fn a_change_inside_a_dockerignored_directory_never_busts_the_cache() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/dockerignore-cache-base:latest",
        &busybox,
        &["sh", "cat"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    std::fs::write(context_dir.path().join("keep.txt"), b"kept content").unwrap();
    std::fs::create_dir(context_dir.path().join("node_modules")).unwrap();
    std::fs::write(
        context_dir.path().join("node_modules/whatever.js"),
        b"version one",
    )
    .unwrap();
    write_dockerignore(context_dir.path(), "node_modules\n");
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/dockerignore-cache-base:latest\nCOPY . /app\n",
    );

    let first = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/dockerignore-cache-first:latest",
        ],
    );
    assert!(
        first.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );

    // Only the excluded file's own content changes -- `keep.txt`
    // itself, and every other real instruction, stay exactly the
    // same.
    std::fs::write(
        context_dir.path().join("node_modules/whatever.js"),
        b"a completely different version two",
    )
    .unwrap();
    let second = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/dockerignore-cache-second:latest",
        ],
    );
    assert!(
        second.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );

    let first_record = store
        .resolve_image("docker.io/ociman-test/dockerignore-cache-first:latest")
        .unwrap()
        .unwrap();
    let second_record = store
        .resolve_image("docker.io/ociman-test/dockerignore-cache-second:latest")
        .unwrap()
        .unwrap();
    let first_manifest = store.image_manifest(&first_record).unwrap();
    let second_manifest = store.image_manifest(&second_record).unwrap();

    assert_eq!(
        first_manifest.layers, second_manifest.layers,
        "a change inside an excluded directory must never bust the cache -- that content was \
         never actually copied in the first place"
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

fn write_dockerignore(dir: &Path, contents: &str) {
    std::fs::write(dir.join(".dockerignore"), contents).unwrap();
}

fn write_containerignore(dir: &Path, contents: &str) {
    std::fs::write(dir.join(".containerignore"), contents).unwrap();
}

/// `.dockerignore` excludes a named file from a whole-context `COPY .
/// /app` -- the most common real-world use -- while an un-matched
/// file still copies normally. Every non-obvious `.dockerignore` rule
/// exercised across this test group was independently confirmed
/// against a real, installed `podman build` (4.9.3) first -- see
/// `oci_dockerfile::dockerignore`'s own doc comment for the exact
/// transcripts.
#[test]
fn dockerignore_excludes_a_named_file_from_a_whole_context_copy() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/dockerignore-base:latest",
        &busybox,
        &["sh", "find"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    std::fs::write(context_dir.path().join("keep.txt"), "keep\n").unwrap();
    std::fs::write(context_dir.path().join("ignored.txt"), "ignored\n").unwrap();
    write_dockerignore(context_dir.path(), "ignored.txt\n");
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/dockerignore-base:latest\n\
         COPY . /app\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/dockerignore-result:latest",
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
            "ociman-test/dockerignore-result:latest",
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
    // Neither the Containerfile nor the `.dockerignore` itself gets
    // any special always-included treatment either -- confirmed
    // directly against real `podman build` -- so both show up here
    // right alongside `keep.txt`, exactly like real podman's own
    // output would.
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "/app/.dockerignore\n/app/Containerfile\n/app/keep.txt\n"
    );
}

/// A bare pattern with no `**` only ever matches a *top-level* context
/// entry -- confirmed directly against real `podman build`: a bare
/// `*.log` pattern left a nested `subdir/nested.log` file in place.
#[test]
fn dockerignore_bare_pattern_only_matches_top_level_not_nested() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/dockerignore-bare-base:latest",
        &busybox,
        &["sh", "find"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    std::fs::write(context_dir.path().join("top.log"), "top\n").unwrap();
    std::fs::create_dir(context_dir.path().join("subdir")).unwrap();
    std::fs::write(
        context_dir.path().join("subdir").join("nested.log"),
        "nested\n",
    )
    .unwrap();
    write_dockerignore(context_dir.path(), "*.log\n");
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/dockerignore-bare-base:latest\n\
         COPY top.log /top.log.copy\n\
         COPY subdir /app/subdir\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/dockerignore-bare-result:latest",
        ],
    );
    // The explicit, top-level `COPY top.log ...` fails outright --
    // `top.log` really is excluded, matching real `podman build`'s own
    // "does not exist" error for exactly this case (see the dedicated
    // test for this below); this test only cares about the *nested*
    // file surviving, so it uses a wildcard-free `subdir` copy on its
    // own, separately, once that's confirmed.
    assert!(!build.status.success());

    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/dockerignore-bare-base:latest\n\
         COPY subdir /app/subdir\n",
    );
    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/dockerignore-bare-result:latest",
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
            "ociman-test/dockerignore-bare-result:latest",
            "--",
            "/bin/sh",
            "-c",
            "find /app -type f | sort",
        ],
    );
    assert!(run.status.success());
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "/app/subdir/nested.log\n"
    );
}

/// `**/*.log` (unlike a bare `*.log`, see above) matches at any depth
/// -- confirmed directly against real `podman build`.
#[test]
fn dockerignore_double_star_prefix_matches_at_any_depth() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/dockerignore-double-star-base:latest",
        &busybox,
        &["sh", "find"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    std::fs::write(context_dir.path().join("top.log"), "top\n").unwrap();
    std::fs::write(context_dir.path().join("keep.txt"), "keep\n").unwrap();
    std::fs::create_dir(context_dir.path().join("subdir")).unwrap();
    std::fs::write(
        context_dir.path().join("subdir").join("nested.log"),
        "nested\n",
    )
    .unwrap();
    write_dockerignore(context_dir.path(), "**/*.log\n");
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/dockerignore-double-star-base:latest\n\
         COPY . /app\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/dockerignore-double-star-result:latest",
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
            "ociman-test/dockerignore-double-star-result:latest",
            "--",
            "/bin/sh",
            "-c",
            "find /app -type f | sort",
        ],
    );
    assert!(run.status.success());
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "/app/.dockerignore\n/app/Containerfile\n/app/keep.txt\n"
    );
}

/// A later `!`-negated pattern re-includes one specific file even
/// though an earlier pattern excluded its own parent directory --
/// unlike real `.gitignore`'s own early-pruning behavior, confirmed
/// directly against real `podman build`.
#[test]
fn dockerignore_negation_re_includes_one_file_under_an_excluded_directory() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/dockerignore-negation-base:latest",
        &busybox,
        &["sh", "find"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(context_dir.path().join("subdir")).unwrap();
    std::fs::write(
        context_dir.path().join("subdir").join("other.txt"),
        "other\n",
    )
    .unwrap();
    std::fs::write(context_dir.path().join("subdir").join("keep.txt"), "keep\n").unwrap();
    write_dockerignore(context_dir.path(), "subdir\n!subdir/keep.txt\n");
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/dockerignore-negation-base:latest\n\
         COPY . /app\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/dockerignore-negation-result:latest",
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
            "ociman-test/dockerignore-negation-result:latest",
            "--",
            "/bin/sh",
            "-c",
            "find /app -type f | sort",
        ],
    );
    assert!(run.status.success());
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "/app/.dockerignore\n/app/Containerfile\n/app/subdir/keep.txt\n"
    );
}

/// `.dockerignore` applies to `ADD`'s own local (non-URL) sources
/// exactly the same way it does to `COPY`'s -- both read from the
/// same build context, and `ADD`'s own `resolve_sources`/
/// `ensure_sources_exist`/`copy_path_recursive` call sites share the
/// exact same dockerignore-aware code `COPY`'s do (`bin/ociman/src/
/// build.rs`'s own `add_instruction`).
#[test]
fn dockerignore_also_applies_to_add_local_sources() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/dockerignore-add-base:latest",
        &busybox,
        &["sh", "find"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    std::fs::write(context_dir.path().join("keep.txt"), "keep\n").unwrap();
    std::fs::write(context_dir.path().join("ignored.txt"), "ignored\n").unwrap();
    write_dockerignore(context_dir.path(), "ignored.txt\n");
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/dockerignore-add-base:latest\n\
         ADD . /app\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/dockerignore-add-result:latest",
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
            "ociman-test/dockerignore-add-result:latest",
            "--",
            "/bin/sh",
            "-c",
            "find /app -type f | sort",
        ],
    );
    assert!(run.status.success());
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "/app/.dockerignore\n/app/Containerfile\n/app/keep.txt\n"
    );
}

/// An explicitly-named (non-wildcard) `COPY` source excluded by
/// `.dockerignore` fails exactly the same way a genuinely missing
/// source would -- confirmed directly against real `podman build`
/// (its own real error: `"no items matching glob ... copied (1
/// filtered out ...): no such file or directory"`).
#[test]
fn dockerignore_explicit_copy_of_an_ignored_source_is_a_clear_error() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/dockerignore-explicit-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    std::fs::write(context_dir.path().join("ignored.txt"), "ignored\n").unwrap();
    write_dockerignore(context_dir.path(), "ignored.txt\n");
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/dockerignore-explicit-base:latest\n\
         COPY ignored.txt /app/ignored.txt\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/dockerignore-explicit-result:latest",
        ],
    );
    assert!(!build.status.success());
    assert!(
        String::from_utf8_lossy(&build.stderr).contains("does not exist"),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );
}

/// A wildcard `COPY` source silently drops any `.dockerignore`d match
/// from the expanded list (no error), as long as at least one
/// surviving match remains -- confirmed directly against real `podman
/// build`.
#[test]
fn dockerignore_wildcard_copy_silently_skips_ignored_matches() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/dockerignore-wildcard-base:latest",
        &busybox,
        &["sh", "find"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    std::fs::write(context_dir.path().join("a.txt"), "a\n").unwrap();
    std::fs::write(context_dir.path().join("b.txt"), "b\n").unwrap();
    write_dockerignore(context_dir.path(), "b.txt\n");
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/dockerignore-wildcard-base:latest\n\
         COPY *.txt /app/\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/dockerignore-wildcard-result:latest",
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
            "ociman-test/dockerignore-wildcard-result:latest",
            "--",
            "/bin/sh",
            "-c",
            "find /app -type f | sort",
        ],
    );
    assert!(run.status.success());
    assert_eq!(String::from_utf8_lossy(&run.stdout), "/app/a.txt\n");
}

/// `.dockerignore` is purely a build-*context* concept -- it never
/// applies to `COPY --from=<stage>` or `COPY --from=<external-image>`
/// (neither one is "the build context"), matching real `docker
/// build`/`podman build` exactly: a file that would be excluded if it
/// were read from the context copies normally when it's instead read
/// from an earlier stage's own rootfs.
#[test]
fn dockerignore_does_not_apply_to_copy_from_an_earlier_stage() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/dockerignore-from-stage-base:latest",
        &busybox,
        &["sh", "find"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    // Matches an entry in the `.dockerignore` below by name alone --
    // but the `builder` stage never reads it from the context at all
    // (it's created fresh via `RUN`, entirely inside that stage's own
    // rootfs), so `.dockerignore` has no path through which it could
    // ever apply to it; `COPY --from=builder` then reads it from that
    // rootfs directly, same as any other `--from=<stage>` read.
    write_dockerignore(context_dir.path(), "ignored.txt\n");
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/dockerignore-from-stage-base:latest AS builder\n\
         RUN echo ignored > /ignored.txt\n\
         FROM ociman-test/dockerignore-from-stage-base:latest\n\
         COPY --from=builder /ignored.txt /app/ignored.txt\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/dockerignore-from-stage-result:latest",
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
            "ociman-test/dockerignore-from-stage-result:latest",
            "--",
            "/bin/sh",
            "-c",
            "cat /app/ignored.txt",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "ignored\n");
}

/// `.containerignore` alone (no `.dockerignore` at all) works exactly
/// like `.dockerignore` does -- confirmed directly against real
/// `podman build`.
#[test]
fn containerignore_alone_excludes_a_named_file() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/containerignore-base:latest",
        &busybox,
        &["sh", "find"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    std::fs::write(context_dir.path().join("a.txt"), "a\n").unwrap();
    std::fs::write(context_dir.path().join("b.txt"), "b\n").unwrap();
    write_containerignore(context_dir.path(), "b.txt\n");
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/containerignore-base:latest\n\
         COPY . /app\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/containerignore-result:latest",
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
            "ociman-test/containerignore-result:latest",
            "--",
            "/bin/sh",
            "-c",
            "find /app -type f | sort",
        ],
    );
    assert!(run.status.success());
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "/app/.containerignore\n/app/Containerfile\n/app/a.txt\n"
    );
}

/// When both `.containerignore` and `.dockerignore` exist at the
/// context root, `.containerignore` wins outright -- `.dockerignore`
/// is never even consulted, matching real `podman build`/`buildah
/// build`'s own actual current behavior, confirmed directly (real
/// `ContainerIgnoreFile` in `~/git/podman/vendor/go.podman.io/buildah/
/// pkg/parse/parse.go`, and independently with a real `podman build`
/// run against a context with both files present at once): a file
/// `.dockerignore` alone would have excluded survives here, because
/// `.containerignore`'s own pattern list is the only one that's ever
/// read at all.
#[test]
fn containerignore_wins_outright_over_a_dockerignore_present_at_the_same_time() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/containerignore-precedence-base:latest",
        &busybox,
        &["sh", "find"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    std::fs::write(context_dir.path().join("a.txt"), "a\n").unwrap();
    std::fs::write(context_dir.path().join("b.txt"), "b\n").unwrap();
    // `.containerignore` excludes `b.txt`; `.dockerignore` excludes
    // `a.txt` instead -- if `.dockerignore` were consulted at all,
    // `a.txt` would be missing from the result below.
    write_containerignore(context_dir.path(), "b.txt\n");
    write_dockerignore(context_dir.path(), "a.txt\n");
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/containerignore-precedence-base:latest\n\
         COPY . /app\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/containerignore-precedence-result:latest",
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
            "ociman-test/containerignore-precedence-result:latest",
            "--",
            "/bin/sh",
            "-c",
            "find /app -type f | sort",
        ],
    );
    assert!(run.status.success());
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "/app/.containerignore\n/app/.dockerignore\n/app/Containerfile\n/app/a.txt\n"
    );
}

/// `--ignorefile <path>` reads that exact path directly, at any name
/// or location, instead of the usual `.containerignore`-then-
/// `.dockerignore` context-root search -- confirmed directly against
/// real `podman build --ignorefile`.
#[test]
fn ignorefile_flag_reads_an_arbitrarily_named_file_at_an_arbitrary_path() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/ignorefile-base:latest",
        &busybox,
        &["sh", "find"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    std::fs::write(context_dir.path().join("a.txt"), "a\n").unwrap();
    std::fs::write(context_dir.path().join("b.txt"), "b\n").unwrap();
    // Neither the default name nor the default location -- proves
    // `--ignorefile` really does bypass the usual context-root search
    // entirely rather than merely renaming what it looks for there.
    let custom_ignorefile = tempfile::tempdir().unwrap();
    let custom_ignorefile_path = custom_ignorefile.path().join("custom.ignore");
    std::fs::write(&custom_ignorefile_path, "b.txt\n").unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/ignorefile-base:latest\n\
         COPY . /app\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/ignorefile-result:latest",
            "--ignorefile",
            custom_ignorefile_path.to_str().unwrap(),
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
            "ociman-test/ignorefile-result:latest",
            "--",
            "/bin/sh",
            "-c",
            "find /app -type f | sort",
        ],
    );
    assert!(run.status.success());
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "/app/Containerfile\n/app/a.txt\n"
    );
}

/// A nonexistent `--ignorefile` path is a real, fatal build error --
/// never a silent "no patterns" fallback -- confirmed directly against
/// real `podman build --ignorefile /does/not/exist`.
#[test]
fn ignorefile_flag_pointing_at_a_nonexistent_path_is_a_clear_error() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/ignorefile-missing-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    std::fs::write(context_dir.path().join("a.txt"), "a\n").unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/ignorefile-missing-base:latest\n\
         COPY . /app\n",
    );

    let missing_path = context_dir.path().join("does-not-exist.ignore");
    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/ignorefile-missing-result:latest",
            "--ignorefile",
            missing_path.to_str().unwrap(),
        ],
    );
    assert!(!build.status.success());
}

/// `--iidfile <path>` writes the built image's own digest
/// (`sha256:<hex>`, no trailing newline) to that file after a
/// successful build -- matching real `podman build --iidfile`
/// exactly, confirmed directly against a real installed `podman`.
#[test]
fn iidfile_flag_writes_the_built_images_own_digest_with_no_trailing_newline() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/iidfile-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/iidfile-base:latest\nLABEL marker=iidfile-test\n",
    );

    let iidfile_dir = tempfile::tempdir().unwrap();
    let iidfile_path = iidfile_dir.path().join("iid.txt");
    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/iidfile-result:latest",
            "--iidfile",
            iidfile_path.to_str().unwrap(),
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let written = std::fs::read_to_string(&iidfile_path).unwrap();
    assert!(
        !written.ends_with('\n') && !written.ends_with(' '),
        "iidfile content must have no trailing whitespace, got {written:?}"
    );
    assert!(
        written.starts_with("sha256:") && written.len() == "sha256:".len() + 64,
        "iidfile content must be a bare sha256 digest, got {written:?}"
    );

    let record = store
        .resolve_image("docker.io/ociman-test/iidfile-result:latest")
        .unwrap()
        .unwrap();
    assert_eq!(written, record.manifest_digest.to_string());
}

/// Every real, stored tar entry path across *every* layer of `store`'s
/// own `reference` — used to confirm a build-time-only file (e.g. the
/// synthesized `/etc/hosts`, see `docs/design/0148`) never actually
/// landed in any committed layer, not just the last one.
fn all_layer_tar_paths(store: &Store, reference: &str) -> Vec<String> {
    let normalized = oci_spec_types::Reference::parse(reference)
        .unwrap()
        .to_string();
    let record = store.resolve_image(&normalized).unwrap().unwrap();
    let manifest = store.image_manifest(&record).unwrap();
    let mut paths = Vec::new();
    for layer in &manifest.layers {
        let blob = store.open_blob(&layer.digest).unwrap();
        let decoder = flate2::read::GzDecoder::new(blob);
        let mut archive = tar::Archive::new(decoder);
        for entry in archive.entries().unwrap() {
            let entry = entry.unwrap();
            paths.push(entry.path().unwrap().to_string_lossy().into_owned());
        }
    }
    paths
}

/// `ociman build --add-host` makes a real, extra `/etc/hosts` entry
/// visible to every `RUN` step's own process — verified by having a
/// `RUN` step itself capture `/etc/hosts` into a file, then reading
/// that file's own content back out of the built image's real,
/// committed layer content directly (*not* via `ociman run` against
/// the built image, which would always synthesize its own fresh
/// `/etc/hosts` for the running container regardless of what the
/// image itself contains — seeing the entry in the file the `RUN`
/// step itself wrote is what actually proves `--add-host` reached the
/// build).
#[test]
fn build_add_host_flag_is_visible_during_run_steps() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/build-add-host-base:latest",
        &busybox,
        &["sh", "cat"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/build-add-host-base:latest\n\
         RUN cat /etc/hosts > /hosts-snapshot.txt\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/build-add-host-result:latest",
            "--add-host",
            "foo.example;bar.example:10.0.0.5",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let normalized = oci_spec_types::Reference::parse("ociman-test/build-add-host-result:latest")
        .unwrap()
        .to_string();
    let record = store.resolve_image(&normalized).unwrap().unwrap();
    let manifest = store.image_manifest(&record).unwrap();
    let digest = &manifest.layers.last().unwrap().digest;
    let blob = store.open_blob(digest).unwrap();
    let decoder = flate2::read::GzDecoder::new(blob);
    let mut archive = tar::Archive::new(decoder);
    let mut snapshot = String::new();
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        if entry.path().unwrap().to_string_lossy() == "hosts-snapshot.txt" {
            std::io::Read::read_to_string(&mut entry, &mut snapshot).unwrap();
        }
    }
    assert!(
        snapshot.contains("10.0.0.5\tfoo.example bar.example"),
        "snapshot: {snapshot:?}"
    );
}

/// The synthesized `/etc/hosts` (localhost, plus any `--add-host`
/// entries) a build container's own `RUN` steps see is never actually
/// committed into any real layer of the built image — matching real
/// buildah's own transient, bind-mounted (never committed) build-time
/// `/etc/hosts` exactly, though by an entirely different mechanism of
/// this project's own (see `docs/design/0148`).
#[test]
fn build_never_commits_a_synthesized_etc_hosts_into_any_layer() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/build-no-host-leak-base:latest",
        &busybox,
        &["sh", "cat"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/build-no-host-leak-base:latest\n\
         RUN cat /etc/hosts\n\
         RUN echo hi > /marker.txt\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/build-no-host-leak-result:latest",
            "--add-host",
            "foo.example:10.0.0.5",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let paths = all_layer_tar_paths(&store, "ociman-test/build-no-host-leak-result:latest");
    assert!(
        paths.iter().any(|p| p == "marker.txt"),
        "sanity check that layer inspection itself works: {paths:?}"
    );
    assert!(
        !paths.iter().any(|p| p.contains("hosts")),
        "no layer should ever contain a synthesized /etc/hosts: {paths:?}"
    );
    assert!(
        !paths.iter().any(|p| p == "etc" || p == "etc/"),
        "no layer should even contain the /etc directory this project creates just to hold \
         the (never-committed) synthesized hosts file: {paths:?}"
    );
}

/// A small, real shell-compatible script that proves whether *it*
/// (rather than real `/bin/sh`) actually ran: writes a fixed marker
/// file first, then hands off to real `/bin/sh -c` with the same
/// command text a real shell-form `RUN` step always passes as its own
/// second argument, so the command's own real effect still happens
/// exactly like it would have under the real default shell.
const CUSTOM_SHELL_SCRIPT: &str =
    "#!/bin/sh\necho marker > /shell-was-used\nexec /bin/sh -c \"$2\"\n";

/// `SHELL` genuinely changes what a later shell-form `RUN` in the
/// *same* stage actually gets invoked with -- checked directly against
/// a real `podman build` during this feature's own design (`docs/
/// design/0175`): a `SHELL ["/some/script", "-c"]` instruction really
/// does make a later `RUN`'s shell-form command run through that
/// script instead of `/bin/sh`. Verified two ways here: the custom
/// script's own marker file exists (proving it, not real `/bin/sh`,
/// was the process actually launched), and the `RUN` step's own
/// intended real effect (writing `/output.txt`) still happened
/// correctly (proving the script's own `exec /bin/sh -c "$2"`
/// hand-off preserved the command text exactly).
#[test]
fn shell_instruction_changes_what_a_later_run_step_actually_gets_invoked_with() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/shell-base:latest",
        &busybox,
        &["sh", "cat"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    std::fs::write(context_dir.path().join("myshell"), CUSTOM_SHELL_SCRIPT).unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/shell-base:latest\n\
         COPY --chmod=0755 myshell /myshell\n\
         SHELL [\"/myshell\", \"-c\"]\n\
         RUN echo hi > /output.txt\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/shell-result:latest",
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
            "ociman-test/shell-result:latest",
            "--",
            "/bin/sh",
            "-c",
            "cat /shell-was-used /output.txt",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "marker\nhi\n",
        "the custom SHELL should have actually run (marker) and the RUN step's own real \
         command should still have taken effect (hi)"
    );
}

/// A `SHELL` instruction has **no** effect on `CMD`/`ENTRYPOINT`'s own
/// shell-form wrapping -- both always keep the fixed `/bin/sh -c`
/// default regardless, matching real `podman build` exactly (checked
/// directly during this feature's own design: real Docker's own
/// documentation claims `SHELL` affects `RUN`/`CMD`/`ENTRYPOINT`
/// alike, but a real `podman build`/buildah only ever actually applies
/// it to `RUN`; podman is this project's own primary reference
/// implementation throughout, so that's the behavior matched here —
/// see `docs/design/0175`).
#[test]
fn shell_instruction_never_affects_cmds_own_shell_form_wrapping() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/shell-cmd-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    std::fs::write(context_dir.path().join("myshell"), CUSTOM_SHELL_SCRIPT).unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/shell-cmd-base:latest\n\
         COPY --chmod=0755 myshell /myshell\n\
         SHELL [\"/myshell\", \"-c\"]\n\
         CMD echo hi\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/shell-cmd-result:latest",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let normalized = oci_spec_types::Reference::parse("ociman-test/shell-cmd-result:latest")
        .unwrap()
        .to_string();
    let record = store.resolve_image(&normalized).unwrap().unwrap();
    let config = store.image_config(&record).unwrap();
    assert_eq!(
        config.config.unwrap().cmd,
        Some(vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "echo hi".to_string()
        ]),
        "CMD's own shell-form wrapping should always use the fixed default, never the active \
         SHELL"
    );
}

/// `SHELL` is scoped to the stage it appears in: a later, separate
/// stage (its own fresh `FROM`) starts with the real default shell
/// again, completely unaffected by an earlier stage's own `SHELL` --
/// checked directly against a real `podman build` (see `docs/design/
/// 0175`). Stage two's own custom shell script is never even copied
/// into its own rootfs, so if `SHELL` incorrectly persisted across
/// stages, this build would fail outright (the script wouldn't exist
/// to exec at all) rather than merely behaving subtly wrong --
/// confirmed here by checking for a real, successful build first, then
/// directly confirming the marker file the custom script alone ever
/// writes is genuinely absent from the final image.
#[test]
fn shell_instruction_resets_at_the_start_of_each_new_stage() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/shell-stage-base:latest",
        &busybox,
        &["sh", "cat"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    std::fs::write(context_dir.path().join("myshell"), CUSTOM_SHELL_SCRIPT).unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/shell-stage-base:latest AS stage1\n\
         COPY --chmod=0755 myshell /myshell\n\
         SHELL [\"/myshell\", \"-c\"]\n\
         RUN echo from-stage1 > /stage1-marker.txt\n\
         \n\
         FROM ociman-test/shell-stage-base:latest AS stage2\n\
         COPY --from=stage1 /stage1-marker.txt /copied-marker.txt\n\
         RUN echo from-stage2 > /stage2-marker.txt\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/shell-stage-result:latest",
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
            "ociman-test/shell-stage-result:latest",
            "--",
            "/bin/sh",
            "-c",
            "cat /copied-marker.txt /stage2-marker.txt; \
             test -e /shell-was-used && echo LEAKED || echo clean",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "from-stage1\nfrom-stage2\nclean\n",
        "stage2's own RUN must have used the real default shell, not stage1's custom one \
         leaking across the stage boundary"
    );
}

/// `--squash` folds every layer the target stage itself adds into
/// exactly one new layer on top of the base -- matching real `podman
/// build --squash` exactly (checked directly during this feature's
/// own design, see `docs/design/0177`): the manifest ends up with
/// exactly one more layer than the base image had, every real change
/// (two separate `RUN`s plus a `LABEL`) survives in the flattened
/// result, and the full per-instruction history is still there
/// afterward (unlike `ociman commit --squash`, which collapses history
/// down to one entry) -- only the very last entry keeps a non-empty
/// layer; every earlier one this same build added shows up as an
/// `empty_layer` history-only entry, its own real content already
/// folded into that final combined layer instead.
#[test]
fn build_squash_folds_every_added_layer_into_one_on_top_of_the_base() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/squash-build-base:latest",
        &busybox,
        &["sh", "cat"],
        ContainerConfig::default(),
    );
    let base_record = store
        .resolve_image("docker.io/ociman-test/squash-build-base:latest")
        .unwrap()
        .unwrap();
    let base_manifest = store.image_manifest(&base_record).unwrap();
    let base_history_len = store.image_config(&base_record).unwrap().history.len();

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/squash-build-base:latest\n\
         RUN echo one > /one.txt\n\
         RUN echo two > /two.txt\n\
         LABEL foo=bar\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            "--squash",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/squash-build-result:latest",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let record = store
        .resolve_image("docker.io/ociman-test/squash-build-result:latest")
        .unwrap()
        .unwrap();
    let manifest = store.image_manifest(&record).unwrap();
    assert_eq!(
        manifest.layers.len(),
        base_manifest.layers.len() + 1,
        "squash should add exactly one new layer, regardless of how many RUN/COPY/ADD steps \
         this build itself had: {manifest:?}"
    );

    let config = store.image_config(&record).unwrap();
    assert_eq!(
        config.rootfs.diff_ids.len(),
        manifest.layers.len(),
        "diff_ids must stay in lockstep with the manifest's own layer list"
    );
    // Every history entry this build added is still there (three new
    // ones: RUN one, RUN two, LABEL) -- squash never removes history,
    // only re-attributes which of them carries a real layer.
    assert_eq!(config.history.len(), base_history_len + 3);
    let this_build_history = &config.history[base_history_len..];
    assert!(
        this_build_history[..2].iter().all(|h| h.empty_layer),
        "every entry but the last should show up as history-only now that its own real \
         content has been folded into the one combined layer: {this_build_history:?}"
    );
    assert!(
        !this_build_history[2].empty_layer,
        "only the very last entry should carry the one new combined layer's own real weight: \
         {this_build_history:?}"
    );

    // Real content check: both RUN steps' own files really did survive
    // being folded together into one layer.
    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "--rm",
            "ociman-test/squash-build-result:latest",
            "--",
            "/bin/sh",
            "-c",
            "cat /one.txt /two.txt",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "one\ntwo\n");
}

/// `--squash` only ever affects the target stage itself -- an earlier
/// stage a later one reads from via `COPY --from=` still builds
/// completely normally, with its own real per-instruction layers,
/// exactly as if `--squash` had never been passed at all (checked
/// directly against a real `podman build --squash` on an identically-
/// shaped multi-stage Containerfile).
#[test]
fn build_squash_only_affects_the_target_stage_not_an_earlier_one() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/squash-multi-base:latest",
        &busybox,
        &["sh", "cat"],
        ContainerConfig::default(),
    );
    let base_record = store
        .resolve_image("docker.io/ociman-test/squash-multi-base:latest")
        .unwrap()
        .unwrap();
    let base_manifest = store.image_manifest(&base_record).unwrap();

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/squash-multi-base:latest AS builder\n\
         RUN echo builder-content > /builder.txt\n\
         \n\
         FROM ociman-test/squash-multi-base:latest\n\
         COPY --from=builder /builder.txt /builder.txt\n\
         RUN echo final-content > /final.txt\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            "--squash",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/squash-multi-result:latest",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let record = store
        .resolve_image("docker.io/ociman-test/squash-multi-result:latest")
        .unwrap()
        .unwrap();
    let manifest = store.image_manifest(&record).unwrap();
    assert_eq!(
        manifest.layers.len(),
        base_manifest.layers.len() + 1,
        "the final (target) stage's own COPY --from= plus RUN should still fold into just one \
         new layer: {manifest:?}"
    );

    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "--rm",
            "ociman-test/squash-multi-result:latest",
            "--",
            "/bin/sh",
            "-c",
            "cat /builder.txt /final.txt",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "builder-content\nfinal-content\n"
    );
}

/// `--squash` on a stage whose own instructions never touch the
/// filesystem at all (just a `LABEL`) still adds one new, real (if
/// empty) layer -- matching `commit_layer`'s own already-established
/// "an empty diff still commits a real layer" convention, and checked
/// directly against a real `podman build --squash` doing the exact
/// same thing.
#[test]
fn build_squash_on_a_metadata_only_stage_still_adds_one_real_empty_layer() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/squash-noop-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let base_record = store
        .resolve_image("docker.io/ociman-test/squash-noop-base:latest")
        .unwrap()
        .unwrap();
    let base_manifest = store.image_manifest(&base_record).unwrap();

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/squash-noop-base:latest\nLABEL foo=bar\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            "--squash",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/squash-noop-result:latest",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let record = store
        .resolve_image("docker.io/ociman-test/squash-noop-result:latest")
        .unwrap()
        .unwrap();
    let manifest = store.image_manifest(&record).unwrap();
    assert_eq!(manifest.layers.len(), base_manifest.layers.len() + 1);
}

/// `--squash` on a Containerfile with *no* instructions beyond the
/// `FROM` itself is a true no-op -- matching real `podman build
/// --squash`'s own observed behavior exactly: no new layer, no new
/// history entry, nothing to squash at all since this build added
/// nothing.
#[test]
fn build_squash_with_no_instructions_at_all_is_a_true_no_op() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/squash-barefrom-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let base_record = store
        .resolve_image("docker.io/ociman-test/squash-barefrom-base:latest")
        .unwrap()
        .unwrap();
    let base_manifest = store.image_manifest(&base_record).unwrap();
    let base_config = store.image_config(&base_record).unwrap();

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/squash-barefrom-base:latest\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            "--squash",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/squash-barefrom-result:latest",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let record = store
        .resolve_image("docker.io/ociman-test/squash-barefrom-result:latest")
        .unwrap()
        .unwrap();
    let manifest = store.image_manifest(&record).unwrap();
    let config = store.image_config(&record).unwrap();
    assert_eq!(manifest.layers, base_manifest.layers);
    assert_eq!(config.history.len(), base_config.history.len());
}

/// `--squash` disables the build cache for the whole build -- matching
/// real `podman build --squash`'s own identical, checked-directly
/// behavior: re-running an otherwise-identical `--squash` build a
/// second time still re-executes every `RUN`, never reusing a cached
/// layer, since a squashed build's own per-instruction layers are
/// never stored as independently reusable layers to begin with.
/// Verified here by a real, observable side effect rather than
/// inspecting internal cache state directly: a `RUN` that appends to a
/// counter file produces a *different* result the second time only if
/// it genuinely re-executed.
#[test]
fn build_squash_disables_the_build_cache() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/squash-cache-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/squash-cache-base:latest\n\
         RUN cat /proc/sys/kernel/random/uuid > /marker.txt\n",
    );

    let first = ociman(
        storage_dir.path(),
        &[
            "build",
            "--squash",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/squash-cache-result:latest",
        ],
    );
    assert!(
        first.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first_digest = String::from_utf8_lossy(&first.stdout)
        .lines()
        .next()
        .unwrap()
        .to_string();

    std::thread::sleep(std::time::Duration::from_millis(20));
    let second = ociman(
        storage_dir.path(),
        &[
            "build",
            "--squash",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/squash-cache-result2:latest",
        ],
    );
    assert!(
        second.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    let second_digest = String::from_utf8_lossy(&second.stdout)
        .lines()
        .next()
        .unwrap()
        .to_string();

    assert_ne!(
        first_digest, second_digest,
        "a second --squash build should have genuinely re-executed RUN (producing a different \
         nanosecond-timestamped marker file), not reused a cached layer from the first"
    );
}

/// `--squash-all` folds the base image's own layers/history in too,
/// not just the target stage's own newly-added ones -- matching real
/// `podman build --squash-all` exactly (checked directly during this
/// feature's own design, see `docs/design/0184`): the manifest ends up
/// with exactly *one* layer total, never referencing the base at all
/// (unlike `--squash`'s own base_manifest.layers.len() + 1), and the
/// base's own inherited history is discarded entirely too -- only this
/// build's own instructions show up in `ociman history` afterward.
#[test]
fn build_squash_all_folds_the_base_image_in_too_leaving_exactly_one_layer() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/squash-all-build-base:latest",
        &busybox,
        &["sh", "cat"],
        ContainerConfig::default(),
    );
    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/squash-all-build-base:latest\n\
         RUN echo one > /one.txt\n\
         RUN echo two > /two.txt\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            "--squash-all",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/squash-all-build-result:latest",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let record = store
        .resolve_image("docker.io/ociman-test/squash-all-build-result:latest")
        .unwrap()
        .unwrap();
    let manifest = store.image_manifest(&record).unwrap();
    assert_eq!(
        manifest.layers.len(),
        1,
        "--squash-all should fold the base in too, leaving exactly one layer total \
         (regardless of how many layers the base itself had): {manifest:?}"
    );

    let config = store.image_config(&record).unwrap();
    assert_eq!(config.rootfs.diff_ids.len(), 1);
    assert_eq!(
        config.history.len(),
        2,
        "the base's own inherited history should be discarded entirely, leaving only this \
         build's own two RUN instructions: {:?}",
        config.history
    );
    assert!(config.history[0].empty_layer);
    assert!(!config.history[1].empty_layer);

    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "--rm",
            "ociman-test/squash-all-build-result:latest",
            "--",
            "/bin/sh",
            "-c",
            "cat /one.txt /two.txt",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "one\ntwo\n");
}

/// `--squash-all` on a Containerfile with no instructions beyond
/// `FROM` still produces exactly one new, freshly-recompressed layer
/// (never referencing the base's own original layer at all) plus
/// exactly one synthetic history entry -- unlike `--squash`, which
/// treats that same shape as a true no-op. Checked directly against a
/// real `podman build --squash-all` doing the exact same thing.
#[test]
fn build_squash_all_with_no_instructions_still_produces_one_fresh_layer() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/squash-all-barefrom-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let base_record = store
        .resolve_image("docker.io/ociman-test/squash-all-barefrom-base:latest")
        .unwrap()
        .unwrap();
    let base_manifest = store.image_manifest(&base_record).unwrap();

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/squash-all-barefrom-base:latest\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            "--squash-all",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/squash-all-barefrom-result:latest",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let record = store
        .resolve_image("docker.io/ociman-test/squash-all-barefrom-result:latest")
        .unwrap()
        .unwrap();
    let manifest = store.image_manifest(&record).unwrap();
    assert_eq!(manifest.layers.len(), 1);
    assert_ne!(
        manifest.layers[0].digest, base_manifest.layers[0].digest,
        "--squash-all should always produce a real, freshly-recompressed layer, never reuse \
         the base's own original one verbatim, even for a true no-op instruction-wise -- unlike \
         --squash, which special-cases this shape as a byte-identical no-op instead"
    );

    let config = store.image_config(&record).unwrap();
    assert_eq!(
        config.history.len(),
        1,
        "a synthetic single history entry, since there are no real instructions to repurpose \
         one from: {:?}",
        config.history
    );
    assert!(!config.history[0].empty_layer);
}

/// `--squash-all` only ever applies to the target stage -- an earlier
/// stage feeding it via `COPY --from=` still builds completely
/// normally, exactly like `--squash` (checked directly against a real
/// `podman build --squash-all` on the identically-shaped multi-stage
/// Containerfile).
#[test]
fn build_squash_all_only_affects_the_target_stage() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/squash-all-multi-base:latest",
        &busybox,
        &["sh", "cat"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/squash-all-multi-base:latest AS builder\n\
         RUN echo builder-content > /builder.txt\n\
         \n\
         FROM ociman-test/squash-all-multi-base:latest\n\
         COPY --from=builder /builder.txt /builder.txt\n\
         RUN echo final-content > /final.txt\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            "--squash-all",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/squash-all-multi-result:latest",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let record = store
        .resolve_image("docker.io/ociman-test/squash-all-multi-result:latest")
        .unwrap()
        .unwrap();
    let manifest = store.image_manifest(&record).unwrap();
    assert_eq!(manifest.layers.len(), 1, "{manifest:?}");

    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "--rm",
            "ociman-test/squash-all-multi-result:latest",
            "--",
            "/bin/sh",
            "-c",
            "cat /builder.txt /final.txt",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "builder-content\nfinal-content\n"
    );
}

/// `--squash` and `--squash-all` together is a clear, immediate error
/// -- matching real `podman build`'s own identical refusal (checked
/// directly: `Error: cannot specify --squash with --layers and
/// --squash-all with --squash`).
#[test]
fn build_squash_and_squash_all_together_is_a_clear_error() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/squash-both-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/squash-both-base:latest\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            "--squash",
            "--squash-all",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/squash-both-result:latest",
        ],
    );
    assert!(!build.status.success());
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(stderr.contains("cannot be used together"), "{stderr}");
}

/// A `RUN` step's own stdin (0187) must always be a fresh, empty
/// `/dev/null`, never a silent pass-through of whatever real stdin the
/// `ociman build` invocation itself happened to have -- matching real
/// `docker build`/`podman build` exactly (checked directly: piping
/// real input into a real `podman build` whose one `RUN` step tries to
/// read it back never sees it, even though the `podman build` process
/// itself did).
///
/// A real, previously-unnoticed bug this test would have caught:
/// before this fix, every `RUN` step silently inherited whatever fd 0
/// `ociman build` itself had, forwarding real piped input completely
/// unconditionally, with no way to turn it off -- the same underlying
/// root cause `run_without_interactive_never_forwards_real_stdin`
/// (`tests/tests/ociman_run.rs`) independently caught for `ociman run`.
#[test]
fn build_run_step_never_sees_real_host_stdin() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/build-stdin-base:latest",
        &busybox,
        &["sh", "cat"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/build-stdin-base:latest\n\
         RUN if read -t 5 line; then echo GOT:$line >/marker; else echo NOINPUT >/marker; fi\n",
    );

    let mut child = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args([
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/build-stdin-result:latest",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn ociman build");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"hello-from-host-stdin\n")
        .unwrap();
    let build = child.wait_with_output().unwrap();
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
            "ociman-test/build-stdin-result:latest",
            "/bin/cat",
            "/marker",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "NOINPUT",
        "a RUN step should never see real host stdin, piped into the build invocation or not"
    );
}

/// The `GOARCH`-style name for the architecture these tests are
/// actually running on — matches `oci_spec_types::image::Platform::
/// host`'s own internal `host_arch()` naming exactly, so these tests
/// stay portable regardless of which real host architecture runs
/// them (this project's own CI matrix covers both x86_64 and
/// aarch64).
fn host_goarch() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "amd64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        panic!("unsupported test host architecture");
    }
}

/// `ociman build --platform <host's own real platform>` (0193): builds
/// completely normally — the common case a real Containerfile pinning
/// its own platform explicitly (even when it happens to already match)
/// must not break.
#[test]
fn build_platform_matching_the_real_host_succeeds() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/platform-match-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/platform-match-base:latest\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            "--platform",
            &format!("linux/{}", host_goarch()),
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/platform-match-result:latest",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );
}

/// `ociman build --platform` naming a *different* architecture is a
/// real, previously-unnoticed bug this closes: before this flag (and
/// the check backing it) existed, a `FROM --platform=` value was
/// parsed but never read anywhere at all, so a Containerfile
/// requesting a non-host platform silently got the host platform
/// instead — this project has no real cross-architecture emulation of
/// any kind, so a mismatch is now a clear, immediate error instead.
#[test]
fn build_platform_mismatch_is_a_clear_error() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/platform-mismatch-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/platform-mismatch-base:latest\n",
    );

    // Deliberately the *other* real architecture this project supports
    // at all, never the host's own -- guaranteed to be a real mismatch
    // regardless of which host runs this test.
    let other_arch = if host_goarch() == "amd64" {
        "arm64"
    } else {
        "amd64"
    };
    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            "--platform",
            &format!("linux/{other_arch}"),
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/platform-mismatch-result:latest",
        ],
    );
    assert!(!build.status.success());
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(
        stderr.contains(&format!("linux/{other_arch}")) && stderr.contains("does not match"),
        "{stderr}"
    );
}

/// A per-stage `FROM --platform=` always wins over the whole build's
/// own `--platform` flag — matching real BuildKit's own identical
/// precedence exactly (checked directly against
/// `~/git/moby/vendor/.../dockerfile2llb/convert.go`): even a
/// `--platform` that *does* match the host is overridden by a
/// mismatched per-stage value, which must still be the one clear error
/// that actually surfaces.
#[test]
fn build_from_platform_overrides_a_matching_global_platform_flag() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/platform-precedence-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let other_arch = if host_goarch() == "amd64" {
        "arm64"
    } else {
        "amd64"
    };
    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        &format!(
            "FROM --platform=linux/{other_arch} ociman-test/platform-precedence-base:latest\n"
        ),
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            "--platform",
            &format!("linux/{}", host_goarch()),
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/platform-precedence-result:latest",
        ],
    );
    assert!(!build.status.success());
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(
        stderr.contains(&format!("linux/{other_arch}")) && stderr.contains("does not match"),
        "the per-stage FROM --platform= should override a matching global --platform flag, \
         still surfacing as the one clear error: {stderr}"
    );
}

/// `ociman build --unsetenv <NAME>` (0194): removes an environment
/// variable from the *final* built image, regardless of whether it
/// came from the base image's own config or a real `ENV` instruction
/// in this Containerfile — matching real `docker build --unsetenv`/
/// `podman build --unsetenv` exactly (checked directly).
#[test]
fn build_unsetenv_removes_a_declared_env_var_from_the_final_image() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/unsetenv-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            env: vec!["FOO=bar".to_string()],
            ..Default::default()
        },
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/unsetenv-base:latest\nENV BAZ=qux\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            "--unsetenv",
            "FOO",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/unsetenv-result:latest",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let inspect = ociman(
        storage_dir.path(),
        &["inspect", "--json", "ociman-test/unsetenv-result:latest"],
    );
    assert!(inspect.status.success());
    let view: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    let env = view["config"]["Env"].as_array().unwrap();
    assert!(
        env.iter().all(|v| v.as_str() != Some("FOO=bar")),
        "--unsetenv FOO should remove it from the base image's own inherited env: {env:?}"
    );
    assert!(
        env.iter().any(|v| v.as_str() == Some("BAZ=qux")),
        "a real ENV instruction not named by --unsetenv should be untouched: {env:?}"
    );
}

/// `--unsetenv` still removes a variable even when a *later* `ENV`
/// instruction re-declares it — applied once, after every real
/// instruction has already run, matching real `podman build
/// --unsetenv` exactly (checked directly).
#[test]
fn build_unsetenv_removes_a_variable_even_if_a_later_env_redeclares_it() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/unsetenv-redeclare-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/unsetenv-redeclare-base:latest\nENV FOO=bar\nENV FOO=overridden\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            "--unsetenv",
            "FOO",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/unsetenv-redeclare-result:latest",
        ],
    );
    assert!(build.status.success());

    let inspect = ociman(
        storage_dir.path(),
        &[
            "inspect",
            "--json",
            "ociman-test/unsetenv-redeclare-result:latest",
        ],
    );
    assert!(inspect.status.success());
    let view: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    // Legitimately absent (not an empty array) once `--unsetenv`
    // removes the *only* variable this image's own config ever
    // declared -- `serde`'s own `skip_serializing_if` on an empty
    // `Vec` (`ContainerConfig::env`), not a bug.
    let empty = Vec::new();
    let env = view["config"]["Env"].as_array().unwrap_or(&empty);
    assert!(
        env.iter().all(|v| !v.as_str().unwrap().starts_with("FOO=")),
        "--unsetenv should still win even over a later ENV re-declaring the same name: {env:?}"
    );
}

/// `--unsetenv` never adds its own `ociman history` entry — matching
/// real `podman build --unsetenv`'s own identical behavior exactly
/// (checked directly): unlike `--label`, which shows up as its own
/// extra `LABEL` step, an otherwise-identical build with and without
/// `--unsetenv` produces the exact same history.
#[test]
fn build_unsetenv_adds_no_history_entry_of_its_own() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/unsetenv-history-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/unsetenv-history-base:latest\nENV FOO=bar\n",
    );

    let without = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/unsetenv-history-without:latest",
        ],
    );
    assert!(without.status.success());
    let with = ociman(
        storage_dir.path(),
        &[
            "build",
            "--unsetenv",
            "FOO",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/unsetenv-history-with:latest",
        ],
    );
    assert!(with.status.success());

    let history_without = ociman(
        storage_dir.path(),
        &[
            "history",
            "--json",
            "ociman-test/unsetenv-history-without:latest",
        ],
    );
    let history_with = ociman(
        storage_dir.path(),
        &[
            "history",
            "--json",
            "ociman-test/unsetenv-history-with:latest",
        ],
    );
    assert!(history_without.status.success());
    assert!(history_with.status.success());
    assert_eq!(
        history_without.stdout, history_with.stdout,
        "--unsetenv should never add a history entry of its own"
    );
}

/// A real, previously-unnoticed discrepancy found by hand while first
/// verifying `--unsetenv` end to end (0194): unsetting an image's only
/// declared environment variable used to leave a stray `TERM=xterm`
/// behind (a leftover from `Spec::example()`'s own placeholder
/// `process.env`, never actually cleared), instead of matching real
/// `podman run`'s own identical fallback (a real `PATH`, checked
/// directly, but never `TERM`).
#[test]
fn build_unsetenv_down_to_zero_leaves_only_the_real_podman_style_path_fallback() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/unsetenv-to-zero-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            env: vec![
                "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string(),
            ],
            ..Default::default()
        },
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/unsetenv-to-zero-base:latest\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            "--unsetenv",
            "PATH",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/unsetenv-to-zero-result:latest",
        ],
    );
    assert!(build.status.success());

    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "--rm",
            "ociman-test/unsetenv-to-zero-result:latest",
            "/bin/sh",
            "-c",
            "env",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    // `SHLVL`/`PWD` are the shell's own, legitimate additions (`ash`
    // sets both on every invocation) -- unrelated to this project's
    // own env-fallback logic, so only check for the two things that
    // actually matter here: the real `PATH` fallback present, and no
    // stray `TERM` (this bug's own exact symptom) anywhere.
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"),
        "an image left with zero declared env vars should fall back to a real PATH, matching \
         real podman exactly: {stdout:?}"
    );
    assert!(
        !stdout.contains("TERM="),
        "a stray TERM should never leak in, unlike before this fix: {stdout:?}"
    );
}

/// `ociman build --unsetlabel <KEY>` (0195): removes a label the
/// *base image itself* declared — matching real `docker build
/// --unsetlabel`/`podman build --unsetlabel` exactly (checked
/// directly).
#[test]
fn build_unsetlabel_removes_a_label_inherited_from_the_base_image() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/unsetlabel-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            labels: std::collections::BTreeMap::from([(
                "inherited".to_string(),
                "frombase".to_string(),
            )]),
            ..Default::default()
        },
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/unsetlabel-base:latest\nLABEL owndeclared=fromhere\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            "--unsetlabel",
            "inherited",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/unsetlabel-result:latest",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let inspect = ociman(
        storage_dir.path(),
        &["inspect", "--json", "ociman-test/unsetlabel-result:latest"],
    );
    assert!(inspect.status.success());
    let view: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert!(
        view["config"]["Labels"].get("inherited").is_none(),
        "--unsetlabel should remove a label the base image itself declared: {view:?}"
    );
    assert_eq!(
        view["config"]["Labels"]["owndeclared"], "fromhere",
        "a label only ever declared by this Containerfile's own LABEL should be untouched: \
         {view:?}"
    );
}

/// A real, checked-directly subtlety that makes `--unsetlabel`
/// deliberately *not* shaped like `--unsetenv`: naming a key that's
/// only ever set by a `LABEL` instruction in *this* Containerfile
/// (never present in the base image's own config at all) leaves it
/// completely untouched.
#[test]
fn build_unsetlabel_never_touches_a_label_only_declared_by_this_containerfile() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/unsetlabel-ownonly-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/unsetlabel-ownonly-base:latest\nLABEL owndeclared=fromhere\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            "--unsetlabel",
            "owndeclared",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/unsetlabel-ownonly-result:latest",
        ],
    );
    assert!(build.status.success());

    let inspect = ociman(
        storage_dir.path(),
        &[
            "inspect",
            "--json",
            "ociman-test/unsetlabel-ownonly-result:latest",
        ],
    );
    assert!(inspect.status.success());
    let view: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert_eq!(
        view["config"]["Labels"]["owndeclared"], "fromhere",
        "--unsetlabel naming a key only ever declared by this Containerfile's own LABEL \
         (never present in the base at all) must leave it completely untouched: {view:?}"
    );
}

/// A base-inherited key that a *later* `LABEL` in this same
/// Containerfile also redeclares is still removed by `--unsetlabel`
/// naming it — the redeclaration does not save it, matching real
/// `podman build --unsetlabel`'s own checked-directly behavior
/// exactly.
#[test]
fn build_unsetlabel_still_removes_an_inherited_key_even_when_redeclared() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/unsetlabel-redeclare-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            labels: std::collections::BTreeMap::from([(
                "inherited".to_string(),
                "frombase".to_string(),
            )]),
            ..Default::default()
        },
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/unsetlabel-redeclare-base:latest\nLABEL inherited=overridden\n",
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            "--unsetlabel",
            "inherited",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/unsetlabel-redeclare-result:latest",
        ],
    );
    assert!(build.status.success());

    let inspect = ociman(
        storage_dir.path(),
        &[
            "inspect",
            "--json",
            "ociman-test/unsetlabel-redeclare-result:latest",
        ],
    );
    assert!(inspect.status.success());
    let view: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert!(
        view["config"]["Labels"].get("inherited").is_none(),
        "--unsetlabel should still remove an inherited key even though a later LABEL in this \
         same Containerfile redeclares it: {view:?}"
    );
}

/// `--unsetlabel` adds no `ociman history` entry of its own, matching
/// real `podman build --unsetlabel`'s own identical behavior exactly.
#[test]
fn build_unsetlabel_adds_no_history_entry_of_its_own() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/unsetlabel-history-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            labels: std::collections::BTreeMap::from([("foo".to_string(), "bar".to_string())]),
            ..Default::default()
        },
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/unsetlabel-history-base:latest\n",
    );

    let without = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/unsetlabel-history-without:latest",
        ],
    );
    assert!(without.status.success());
    let with = ociman(
        storage_dir.path(),
        &[
            "build",
            "--unsetlabel",
            "foo",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/unsetlabel-history-with:latest",
        ],
    );
    assert!(with.status.success());

    let history_without = ociman(
        storage_dir.path(),
        &[
            "history",
            "--json",
            "ociman-test/unsetlabel-history-without:latest",
        ],
    );
    let history_with = ociman(
        storage_dir.path(),
        &[
            "history",
            "--json",
            "ociman-test/unsetlabel-history-with:latest",
        ],
    );
    assert!(history_without.status.success());
    assert!(history_with.status.success());
    assert_eq!(
        history_without.stdout, history_with.stdout,
        "--unsetlabel should never add a history entry of its own"
    );
}

/// `ociman build -q`/`--quiet` (0196): matching real `podman build -q`
/// exactly (checked directly against a real installed `podman build
/// -q`) -- the *only* thing it still prints is the final image
/// digest; a `RUN` step's own live stdout, which an ordinary
/// (non-quiet) build passes straight through, is completely
/// suppressed instead.
#[test]
fn build_quiet_suppresses_run_step_output_and_prints_only_the_digest() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/quiet-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/quiet-base:latest\nRUN echo run-output-marker\n",
    );

    // Without `-q`: the RUN step's own live output and the "tagged:
    // ..." line are both present, matching this project's own
    // already-established (pre-0196) default behavior.
    let loud = ociman(
        storage_dir.path(),
        &[
            "build",
            "--no-cache",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/quiet-loud:latest",
        ],
    );
    assert!(
        loud.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&loud.stderr)
    );
    let loud_stdout = String::from_utf8_lossy(&loud.stdout);
    assert!(
        loud_stdout.contains("run-output-marker"),
        "without -q, a RUN step's own live output should still be visible: {loud_stdout:?}"
    );
    assert!(
        loud_stdout.contains("tagged:"),
        "without -q, a tagged build should still print \"tagged: ...\": {loud_stdout:?}"
    );

    // With `-q`: neither the RUN step's own output nor the "tagged:
    // ..." line appears -- stdout is *only* the final digest.
    let quiet = ociman(
        storage_dir.path(),
        &[
            "build",
            "--no-cache",
            "-q",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/quiet-result:latest",
        ],
    );
    assert!(
        quiet.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&quiet.stderr)
    );
    let quiet_stdout = String::from_utf8_lossy(&quiet.stdout);
    assert!(
        !quiet_stdout.contains("run-output-marker"),
        "-q should suppress a RUN step's own live output entirely: {quiet_stdout:?}"
    );
    assert!(
        !quiet_stdout.contains("tagged:"),
        "-q should suppress the \"tagged: ...\" line too, matching a real `podman build -q`'s \
         own checked-directly output (just the one digest line, nothing else): {quiet_stdout:?}"
    );
    let digest = quiet_stdout.trim();
    assert!(
        digest.starts_with("sha256:") && !digest.contains('\n'),
        "-q's own entire stdout should be exactly one digest line: {quiet_stdout:?}"
    );

    // The image was still actually built and tagged despite the
    // suppressed messages -- `-q` only silences *output*, never
    // changes what actually gets built.
    let inspect = ociman(
        storage_dir.path(),
        &["inspect", "--json", "ociman-test/quiet-result:latest"],
    );
    assert!(inspect.status.success());
}

/// `-q` also suppresses the unused-`--build-arg` warning -- checked
/// directly against a real `podman build -q --build-arg UNUSED=x`,
/// which prints nothing at all except the final digest, not even
/// this normally-always-shown warning.
#[test]
fn build_quiet_suppresses_unused_build_arg_warning() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/quiet-unused-arg-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/quiet-unused-arg-base:latest\n",
    );

    let loud = ociman(
        storage_dir.path(),
        &[
            "build",
            "--build-arg",
            "UNUSED=xyz",
            context_dir.path().to_str().unwrap(),
        ],
    );
    assert!(loud.status.success());
    let loud_stderr = String::from_utf8_lossy(&loud.stderr);
    assert!(
        loud_stderr.contains("[Warning]"),
        "without -q, an unused --build-arg should still warn: {loud_stderr:?}"
    );

    let quiet = ociman(
        storage_dir.path(),
        &[
            "build",
            "-q",
            "--build-arg",
            "UNUSED=xyz",
            context_dir.path().to_str().unwrap(),
        ],
    );
    assert!(quiet.status.success());
    let quiet_stderr = String::from_utf8_lossy(&quiet.stderr);
    assert!(
        !quiet_stderr.contains("[Warning]"),
        "-q should suppress the unused --build-arg warning too: {quiet_stderr:?}"
    );
}
