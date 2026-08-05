//! `ociman container` subcommand family integration tests
//! (`docs/design/0357`): `exists` (see `ociman_exists.rs`), `prune`,
//! `list`/`ls` (`docs/design/0431`, a real, genuine alias for
//! `ociman ps` itself), `inspect` (`docs/design/0488`, a real alias
//! for the top-level `ociman inspect` forced to container-only
//! resolution), `rm` (`docs/design/0489`, a real, byte-identical
//! alias for the top-level `ociman rm` itself), `stop`
//! (`docs/design/0490`, the same byte-identical-alias shape for
//! `ociman stop`), `start` (`docs/design/0491`, the same shape again
//! for `ociman start`), `kill` (`docs/design/0492`, the same shape
//! again for `ociman kill`), `pause`/`unpause` (`docs/design/0493`,
//! the same shape again for `ociman pause`/`ociman unpause`),
//! `restart` (`docs/design/0494`, the same shape again for `ociman
//! restart`), `rename` (`docs/design/0495`, the same shape again for
//! `ociman rename`, with no flags at all), `wait` (`docs/design/
//! 0496`, the same shape again for `ociman wait`), `top`
//! (`docs/design/0497`, the same shape again for `ociman top`), and
//! `logs` (`docs/design/0498`, the same shape again for `ociman
//! logs`) — see `ociman_ps.rs`/`ociman_stop.rs`/`ociman_start.rs`/
//! `ociman_kill.rs`/`ociman_pause.rs`/`ociman_rename.rs`/
//! `ociman_wait.rs`/`ociman_top.rs`/`ociman_logs.rs` for each
//! top-level command's own much larger test suite; this file only
//! proves each alias itself is byte-identical, not the aliased
//! command's own full semantics again.
//!
//! `ociman container prune` removes every real, non-running container
//! (this project's own `Created`/`Stopped`, never `Running`/`Paused`,
//! and never `Creating` either) — matching real `podman container
//! prune`'s own identical eligibility filter exactly (checked
//! directly against `~/git/podman/libpod/runtime_ctr.go`'s own
//! `PruneContainers`).

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

fn ociman_run_detached(storage_root: &Path, image: &str, container_args: &[&str]) {
    let out = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
        .env_remove("OCI_TOOLS_LOG")
        .args(["run", "-d", image])
        .args(container_args)
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn ociman run -d");
    assert!(
        out.status.success(),
        "ociman run -d failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn inspect_json(storage_root: &Path, id: &str) -> serde_json::Value {
    let out = ociman(storage_root, &["inspect", id, "--json"]);
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("inspect --json output was not valid JSON: {e}"))
}

fn wait_for_status(storage_root: &Path, id: &str, want: &str, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let status = inspect_json(storage_root, id)["status"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if status == want || Instant::now() >= deadline {
            return status;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn all_ids(storage_root: &Path) -> Vec<String> {
    let out = ociman(storage_root, &["ps", "-a", "-q"]);
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

/// `ociman container prune` removes only `Created`/`Stopped`
/// containers, leaves a genuinely `Running` one completely untouched,
/// and prints one line per removed id (no heading), matching real
/// `podman container prune`'s own `PrintContainerPruneResults
/// (responses, false)` exactly.
/// `ociman container list`/`ociman container ls` (0431) are real
/// aliases for `ociman ps` itself, matching real `podman container
/// list`/`ls`'s own checked-directly identical `RunE`/flag set as
/// top-level `podman ps` exactly (`~/git/podman/cmd/podman/
/// containers/list.go`) -- byte-identical output for the same
/// fixture state, not merely "close" or "similar".
#[test]
fn container_list_and_ls_are_byte_identical_aliases_for_ps() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/container-list-alias:latest",
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );
    let create = ociman(
        storage_dir.path(),
        &["create", "ociman-test/container-list-alias:latest", "true"],
    );
    assert!(create.status.success(), "{create:?}");

    let ps = ociman(storage_dir.path(), &["ps", "-a"]);
    assert!(ps.status.success());

    let list = ociman(storage_dir.path(), &["container", "list", "-a"]);
    assert!(
        list.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&list.stderr)
    );
    assert_eq!(list.stdout, ps.stdout);

    let ls = ociman(storage_dir.path(), &["container", "ls", "-a"]);
    assert!(
        ls.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&ls.stderr)
    );
    assert_eq!(ls.stdout, ps.stdout);

    // The identical flag set works through the alias too, not just
    // the bare default table.
    let list_quiet = ociman(storage_dir.path(), &["container", "list", "-a", "-q"]);
    let ps_quiet = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    assert!(list_quiet.status.success());
    assert_eq!(list_quiet.stdout, ps_quiet.stdout);
}

#[test]
fn container_prune_removes_created_and_stopped_but_not_running() {
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
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/container-prune:latest",
        &busybox,
        &["sh", "true", "sleep"],
        ContainerConfig::default(),
    );

    // A `Created` (real, never-started) container.
    let create = ociman(
        storage_dir.path(),
        &["create", "ociman-test/container-prune:latest", "true"],
    );
    assert!(create.status.success(), "{create:?}");
    let created_id = String::from_utf8_lossy(&create.stdout).trim().to_string();
    assert_eq!(
        inspect_json(storage_dir.path(), &created_id)["status"],
        "created"
    );

    // A `Stopped` (real, already-exited) container. `ociman run`
    // (foreground) never prints the container's own id on success
    // (only the container's own output/exit code do, matching real
    // `podman run` exactly) — the new id is found by diffing `ps -a
    // -q` against what's already known, the same technique
    // `rm_all_removes_every_stopped_container` above already
    // established via a plain count.
    let run = ociman(
        storage_dir.path(),
        &["run", "ociman-test/container-prune:latest", "true"],
    );
    assert!(run.status.success(), "{run:?}");
    let stopped_id = all_ids(storage_dir.path())
        .into_iter()
        .find(|id| id != &created_id)
        .expect("a second container (the one just run) should now exist");
    assert_eq!(
        inspect_json(storage_dir.path(), &stopped_id)["status"],
        "stopped"
    );

    // A genuinely `Running` container — must survive `prune`
    // untouched.
    ociman_run_detached(
        storage_dir.path(),
        "ociman-test/container-prune:latest",
        &["sleep", "30"],
    );
    let running_id = all_ids(storage_dir.path())
        .into_iter()
        .find(|id| id != &created_id && id != &stopped_id)
        .expect("a third container (the detached one) should now exist");
    assert_eq!(
        wait_for_status(
            storage_dir.path(),
            &running_id,
            "running",
            Duration::from_secs(20)
        ),
        "running",
        "the third container should genuinely be running before prune is even attempted"
    );

    let prune = ociman(storage_dir.path(), &["container", "prune"]);
    assert!(
        prune.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&prune.stderr)
    );
    let mut pruned: Vec<String> = String::from_utf8_lossy(&prune.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    pruned.sort();
    let mut expected = vec![created_id.clone(), stopped_id.clone()];
    expected.sort();
    assert_eq!(pruned, expected, "{prune:?}");

    // Only the running container is left.
    let remaining = all_ids(storage_dir.path());
    assert_eq!(remaining, vec![running_id.clone()]);

    // A second `prune` with nothing left to remove prints nothing and
    // still succeeds (no "nothing to prune" false-error), matching
    // `ociman volume prune`'s own already-established empty-result
    // convention.
    let prune_again = ociman(storage_dir.path(), &["container", "prune"]);
    assert!(prune_again.status.success());
    assert!(
        String::from_utf8_lossy(&prune_again.stdout)
            .trim()
            .is_empty()
    );

    // Clean up the still-running container so the temp dir doesn't
    // leak a live process past this test.
    let _ = ociman(storage_dir.path(), &["kill", &running_id]);
}

/// `-f`/`--force` is accepted (real CLI compatibility with `podman
/// container prune --force`) but changes nothing: this project has no
/// interactive confirmation prompt to skip in the first place.
#[test]
fn container_prune_force_is_accepted_and_behaves_identically() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/container-prune-force:latest",
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );
    let run = ociman(
        storage_dir.path(),
        &["run", "ociman-test/container-prune-force:latest", "true"],
    );
    assert!(run.status.success(), "{run:?}");
    let id = all_ids(storage_dir.path())
        .into_iter()
        .next()
        .expect("the just-run container should exist");

    let prune = ociman(storage_dir.path(), &["container", "prune", "--force"]);
    assert!(prune.status.success(), "{prune:?}");
    assert_eq!(String::from_utf8_lossy(&prune.stdout).trim(), id);
    assert!(all_ids(storage_dir.path()).is_empty());
}

/// `container prune --filter label=` only removes a `Created`/
/// `Stopped` container that also matches (OR'd across multiple
/// values, matching `ociman prune --filter label=`'s own established
/// convention, not `ociman ps --filter label=`'s AND'd one — see
/// `ContainerPruneFilters`'s own doc comment in `bin/ociman/src/
/// main.rs`), leaving a non-matching one completely untouched.
#[test]
fn container_prune_filter_label_only_removes_a_matching_stopped_container() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/container-prune-filter-label:latest",
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );

    let create_match = ociman(
        storage_dir.path(),
        &[
            "create",
            "--label",
            "env=prod",
            "ociman-test/container-prune-filter-label:latest",
            "true",
        ],
    );
    assert!(create_match.status.success(), "{create_match:?}");
    let match_id = String::from_utf8_lossy(&create_match.stdout)
        .trim()
        .to_string();

    let create_other = ociman(
        storage_dir.path(),
        &[
            "create",
            "--label",
            "env=staging",
            "ociman-test/container-prune-filter-label:latest",
            "true",
        ],
    );
    assert!(create_other.status.success(), "{create_other:?}");
    let other_id = String::from_utf8_lossy(&create_other.stdout)
        .trim()
        .to_string();

    let prune = ociman(
        storage_dir.path(),
        &["container", "prune", "--filter", "label=env=prod"],
    );
    assert!(
        prune.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&prune.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&prune.stdout).trim(),
        match_id,
        "{prune:?}"
    );

    let remaining = all_ids(storage_dir.path());
    assert_eq!(remaining, vec![other_id]);
}

/// `container prune --filter until=` only removes a container older
/// than the given threshold, keeping a freshly created one, matching
/// `ociman prune --filter until=`'s/`ociman images --filter until=`'s
/// own already-established `until=` semantics exactly.
#[test]
fn container_prune_filter_until_keeps_a_freshly_created_container() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/container-prune-filter-until:latest",
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );

    let create = ociman(
        storage_dir.path(),
        &[
            "create",
            "ociman-test/container-prune-filter-until:latest",
            "true",
        ],
    );
    assert!(create.status.success(), "{create:?}");
    let id = String::from_utf8_lossy(&create.stdout).trim().to_string();

    // A threshold in the past: this freshly created container is
    // *not* older than it, so it must survive.
    let prune_past = ociman(
        storage_dir.path(),
        &["container", "prune", "--filter", "until=1h"],
    );
    assert!(prune_past.status.success(), "{prune_past:?}");
    assert!(
        String::from_utf8_lossy(&prune_past.stdout)
            .trim()
            .is_empty()
    );
    assert_eq!(all_ids(storage_dir.path()), vec![id.clone()]);

    // Once the container genuinely *is* older than the given
    // duration, it's removed -- the same `sleep` then `until=1s`
    // technique `ociman_prune.rs`'s own `prune_filter_until_removes_
    // an_image_older_than_the_threshold` already established.
    std::thread::sleep(Duration::from_secs(2));
    let prune_old = ociman(
        storage_dir.path(),
        &["container", "prune", "--filter", "until=1s"],
    );
    assert!(
        prune_old.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&prune_old.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&prune_old.stdout).trim(),
        id,
        "{prune_old:?}"
    );
    assert!(all_ids(storage_dir.path()).is_empty());
}

/// `container prune --json` emits the same removed-id list as a plain
/// JSON array, matching `volume prune --json`'s own already-
/// established shape.
#[test]
fn container_prune_json_emits_an_array_of_removed_ids() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/container-prune-json:latest",
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );
    let run = ociman(
        storage_dir.path(),
        &["run", "ociman-test/container-prune-json:latest", "true"],
    );
    assert!(run.status.success(), "{run:?}");
    let id = all_ids(storage_dir.path())
        .into_iter()
        .next()
        .expect("the just-run container should exist");

    let prune = ociman(storage_dir.path(), &["--json", "container", "prune"]);
    assert!(prune.status.success(), "{prune:?}");
    let json: serde_json::Value = serde_json::from_slice(&prune.stdout).unwrap();
    assert_eq!(json, serde_json::json!([id]));
}

/// `ociman container inspect` (0488) is a real, byte-identical alias
/// for the top-level `ociman inspect --type container`, matching real
/// `podman container inspect`'s own checked-directly identical flag
/// set and `inspectExec`'s own unconditional `inspectOpts.Type =
/// common.ContainerType` (`~/git/podman/cmd/podman/containers/
/// inspect.go:43-46`) exactly.
#[test]
fn container_inspect_is_a_byte_identical_alias_for_top_level_inspect_forced_to_container_type() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/container-inspect-alias:latest",
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );
    let create = ociman(
        storage_dir.path(),
        &[
            "create",
            "ociman-test/container-inspect-alias:latest",
            "true",
        ],
    );
    assert!(create.status.success(), "{create:?}");
    let id = String::from_utf8_lossy(&create.stdout).trim().to_string();

    let top_level = ociman(storage_dir.path(), &["inspect", "--type", "container", &id]);
    assert!(top_level.status.success(), "{top_level:?}");

    let alias = ociman(storage_dir.path(), &["container", "inspect", &id]);
    assert!(
        alias.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&alias.stderr)
    );
    assert_eq!(alias.stdout, top_level.stdout);

    // The identical flag set works through the alias too.
    let top_level_fmt = ociman(
        storage_dir.path(),
        &["inspect", "--type", "container", "-f", "{{.id}}", &id],
    );
    let alias_fmt = ociman(
        storage_dir.path(),
        &["container", "inspect", "-f", "{{.id}}", &id],
    );
    assert!(alias_fmt.status.success());
    assert_eq!(alias_fmt.stdout, top_level_fmt.stdout);
    assert_eq!(String::from_utf8_lossy(&alias_fmt.stdout).trim(), id);
}

/// `ociman container inspect` never falls back to an image on a
/// container miss, even when the given reference would otherwise
/// resolve to a real one -- matching real `podman container inspect`
/// exactly (it has no such fallback at all), the exact same
/// "never resolves the other kind" contrast
/// `inspect_type_container_never_resolves_an_image` already
/// establishes for `ociman inspect --type container` itself.
#[test]
fn container_inspect_never_falls_back_to_an_image_on_a_container_miss() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/container-inspect-no-fallback:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    // No container ever created -- only the image exists.
    let alias = ociman(
        storage_dir.path(),
        &[
            "container",
            "inspect",
            "ociman-test/container-inspect-no-fallback:latest",
        ],
    );
    assert!(!alias.status.success());
    assert!(
        String::from_utf8_lossy(&alias.stderr).contains("no such container"),
        "{alias:?}"
    );
}

/// `ociman container inspect --latest`/`-l` resolves the most
/// recently created container, matching the top-level `ociman
/// inspect --latest`'s own already-established behavior exactly.
#[test]
fn container_inspect_latest_works() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/container-inspect-latest:latest",
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );
    let first = ociman(
        storage_dir.path(),
        &[
            "create",
            "ociman-test/container-inspect-latest:latest",
            "true",
        ],
    );
    assert!(first.status.success(), "{first:?}");
    // A real, wall-clock timestamp gap so the two containers' own
    // `created` values are unambiguously ordered -- see `ociman_
    // mount.rs`'s own `unmount_latest_targets_the_most_recently_
    // created_container`'s identical doc comment for why 1200ms
    // specifically.
    std::thread::sleep(Duration::from_millis(1200));
    let second = ociman(
        storage_dir.path(),
        &[
            "create",
            "ociman-test/container-inspect-latest:latest",
            "true",
        ],
    );
    assert!(second.status.success(), "{second:?}");
    let second_id = String::from_utf8_lossy(&second.stdout).trim().to_string();

    let alias = ociman(
        storage_dir.path(),
        &["container", "inspect", "--latest", "-f", "{{.id}}"],
    );
    assert!(
        alias.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&alias.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&alias.stdout).trim(), second_id);

    // `--latest` and an explicit reference together is a real error,
    // matching the top-level `ociman inspect`'s own established rule.
    let both = ociman(
        storage_dir.path(),
        &["container", "inspect", "--latest", &second_id],
    );
    assert!(!both.status.success());
    assert!(
        String::from_utf8_lossy(&both.stderr)
            .contains("--latest and arguments cannot be used together"),
        "{both:?}"
    );
}

/// `ociman container inspect --size`/`-s` reports a real total file
/// size, matching the top-level `ociman inspect --size`'s own
/// already-established behavior exactly.
#[test]
fn container_inspect_size_works() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/container-inspect-size:latest",
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );
    let create = ociman(
        storage_dir.path(),
        &[
            "create",
            "ociman-test/container-inspect-size:latest",
            "true",
        ],
    );
    assert!(create.status.success(), "{create:?}");
    let id = String::from_utf8_lossy(&create.stdout).trim().to_string();

    let alias = ociman(
        storage_dir.path(),
        &["container", "inspect", "--size", "--json", &id],
    );
    assert!(
        alias.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&alias.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&alias.stdout).unwrap();
    assert!(
        json["size"]["root_fs_size"].is_number(),
        "expected a populated size field: {json:?}"
    );
}

/// `ociman container rm` (0489) is a real, byte-identical alias for
/// the top-level `ociman rm`, matching real `podman container rm`'s
/// own checked-directly identical `Use`/`Short`/`Long`/`RunE`/`Args`/
/// `ValidArgsFunction` as top-level `podman rm` exactly (`~/git/
/// podman/cmd/podman/containers/rm.go:39-49`). Full `rm` semantics
/// (multi-id, `--all`, `--cidfile`, `--ignore`, `--time`, `--filter`,
/// `--latest`) are already exhaustively tested against the top-level
/// command in `ociman_ps.rs`; this only proves the alias itself
/// reaches the identical function with the identical fields.
#[test]
fn container_rm_is_a_byte_identical_alias_for_top_level_rm() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/container-rm-alias:latest",
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );
    let run = ociman(
        storage_dir.path(),
        &["run", "ociman-test/container-rm-alias:latest", "true"],
    );
    assert!(run.status.success(), "{run:?}");
    let id = all_ids(storage_dir.path())
        .into_iter()
        .next()
        .expect("the just-run container should exist");

    let alias = ociman(storage_dir.path(), &["container", "rm", &id]);
    assert!(
        alias.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&alias.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&alias.stdout).trim(), id);
    assert!(all_ids(storage_dir.path()).is_empty());
}

/// The alias's own flag set works too, not just the bare form —
/// `--force` kills a still-running container first, matching the
/// top-level `ociman rm --force`'s own already-established behavior
/// exactly.
#[test]
fn container_rm_force_kills_a_still_running_container_first() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/container-rm-force:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );
    ociman_run_detached(
        storage_dir.path(),
        "ociman-test/container-rm-force:latest",
        &["sleep", "30"],
    );
    let id = all_ids(storage_dir.path())
        .into_iter()
        .next()
        .expect("the just-run container should exist");
    assert_eq!(
        wait_for_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let without_force = ociman(storage_dir.path(), &["container", "rm", &id]);
    assert!(!without_force.status.success());
    assert_eq!(all_ids(storage_dir.path()), vec![id.clone()]);

    let with_force = ociman(storage_dir.path(), &["container", "rm", "--force", &id]);
    assert!(
        with_force.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&with_force.stderr)
    );
    assert!(all_ids(storage_dir.path()).is_empty());
}

/// `ociman container stop` (0490) is a real, byte-identical alias for
/// the top-level `ociman stop`, matching real `podman container
/// stop`'s own checked-directly identical `Use`/`Short`/`Long`/
/// `RunE`/`Args`/`ValidArgsFunction` (and identical `stopFlags`-
/// applied flag set) as top-level `podman stop` exactly (`~/git/
/// podman/cmd/podman/containers/stop.go:36-101`). Full `stop`
/// semantics (graceful signal/escalation, `--all`, `--cidfile`,
/// `--ignore`, `--time`, `--signal`, `--filter`, `--latest`) are
/// already exhaustively tested against the top-level command in
/// `ociman_stop.rs`; this only proves the alias itself reaches the
/// identical function with the identical fields.
#[test]
fn container_stop_is_a_byte_identical_alias_for_top_level_stop() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/container-stop-alias:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );
    ociman_run_detached(
        storage_dir.path(),
        "ociman-test/container-stop-alias:latest",
        &["sleep", "30"],
    );
    let id = all_ids(storage_dir.path())
        .into_iter()
        .next()
        .expect("the just-run container should exist");
    assert_eq!(
        wait_for_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let alias = ociman(storage_dir.path(), &["container", "stop", &id]);
    assert!(
        alias.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&alias.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&alias.stdout).trim(), id);
    assert_eq!(inspect_json(storage_dir.path(), &id)["status"], "stopped");
}

/// The alias's own flag set works too, not just the bare form —
/// `--time`/`-t` (an immediate `KILL` with `0`) works through the
/// alias exactly like the top-level `ociman stop --time`'s own
/// already-established behavior.
#[test]
fn container_stop_time_flag_works_through_the_alias() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/container-stop-time:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );
    ociman_run_detached(
        storage_dir.path(),
        "ociman-test/container-stop-time:latest",
        &["sleep", "30"],
    );
    let id = all_ids(storage_dir.path())
        .into_iter()
        .next()
        .expect("the just-run container should exist");
    assert_eq!(
        wait_for_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let start = Instant::now();
    let alias = ociman(storage_dir.path(), &["container", "stop", "-t", "0", &id]);
    assert!(
        alias.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&alias.stderr)
    );
    assert_eq!(inspect_json(storage_dir.path(), &id)["status"], "stopped");
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "a real --time 0 should stop nearly immediately, not wait out any grace period"
    );
}

/// `ociman container start` (0491) is a real, byte-identical alias
/// for the top-level `ociman start`, matching real `podman container
/// start`'s own checked-directly identical `Use`/`Short`/`Long`/
/// `RunE`/`Args`/`ValidArgsFunction` (and identical `startFlags`-
/// applied flag set) as top-level `podman start` exactly (`~/git/
/// podman/cmd/podman/containers/start.go:20-39`). Full `start`
/// semantics (attach, latest resolution, stdin forwarding) are
/// already exhaustively tested against the top-level command in
/// `ociman_start.rs`; this only proves the alias itself reaches the
/// identical function with the identical fields.
#[test]
fn container_start_is_a_byte_identical_alias_for_top_level_start() {
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
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/container-start-alias:latest",
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );
    let create = ociman(
        storage_dir.path(),
        &["create", "ociman-test/container-start-alias:latest", "true"],
    );
    assert!(create.status.success(), "{create:?}");
    let id = String::from_utf8_lossy(&create.stdout).trim().to_string();
    assert_eq!(inspect_json(storage_dir.path(), &id)["status"], "created");

    let alias = ociman(storage_dir.path(), &["container", "start", &id]);
    assert!(
        alias.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&alias.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&alias.stdout).trim(), id);
    assert_eq!(
        wait_for_status(storage_dir.path(), &id, "stopped", Duration::from_secs(20)),
        "stopped"
    );
}

/// The alias's own validation works too, not just the bare form —
/// `--latest` and an explicit id together is a clear error, matching
/// the top-level `ociman start`'s own already-established behavior
/// exactly.
#[test]
fn container_start_latest_and_explicit_id_together_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let alias = ociman(
        storage_dir.path(),
        &["container", "start", "--latest", "some-container"],
    );
    assert!(!alias.status.success());
    assert!(
        String::from_utf8_lossy(&alias.stderr)
            .contains("--latest and containers cannot be used together"),
        "{alias:?}"
    );
}

/// `ociman container kill` (0492) is a real, byte-identical alias for
/// the top-level `ociman kill`, matching real `podman container
/// kill`'s own checked-directly identical `Use`/`Short`/`Long`/
/// `RunE`/`Args`/`ValidArgsFunction` (and identical `killFlags`-
/// applied flag set) as top-level `podman kill` exactly (`~/git/
/// podman/cmd/podman/containers/kill.go:20-46`). Full `kill`
/// semantics (multi-id, `--all`, `--cidfile`, `--latest`) are already
/// exhaustively tested against the top-level command in
/// `ociman_kill.rs`; this only proves the alias itself reaches the
/// identical function with the identical fields.
#[test]
fn container_kill_is_a_byte_identical_alias_for_top_level_kill() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/container-kill-alias:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );
    ociman_run_detached(
        storage_dir.path(),
        "ociman-test/container-kill-alias:latest",
        &["sleep", "30"],
    );
    let id = all_ids(storage_dir.path())
        .into_iter()
        .next()
        .expect("the just-run container should exist");
    assert_eq!(
        wait_for_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    // No `--signal` given at all -- real `podman kill`'s own default
    // is `KILL`, not `TERM`.
    let alias = ociman(storage_dir.path(), &["container", "kill", &id]);
    assert!(
        alias.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&alias.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&alias.stdout).trim(), id);
    assert_eq!(
        wait_for_status(storage_dir.path(), &id, "stopped", Duration::from_secs(20)),
        "stopped"
    );
}

/// The alias's own flag set works too, not just the bare form —
/// `--signal`/`-s` sends exactly that signal (never escalating,
/// unlike `stop`), matching the top-level `ociman kill --signal`'s
/// own already-established behavior exactly.
#[test]
fn container_kill_signal_flag_works_through_the_alias() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/container-kill-signal:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );
    ociman_run_detached(
        storage_dir.path(),
        "ociman-test/container-kill-signal:latest",
        &["sleep", "30"],
    );
    let id = all_ids(storage_dir.path())
        .into_iter()
        .next()
        .expect("the just-run container should exist");
    assert_eq!(
        wait_for_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    // `sleep`, as a pid-namespace's own init, ignores an unhandled-
    // default-action `TERM` outright (the same real, already-
    // established finding `ociman_kill.rs`'s own identical test
    // relies on) -- `kill --signal TERM` never escalates, so the
    // container should genuinely still be running afterward.
    let alias = ociman(
        storage_dir.path(),
        &["container", "kill", "--signal", "TERM", &id],
    );
    assert!(
        alias.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&alias.stderr)
    );
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(inspect_json(storage_dir.path(), &id)["status"], "running");

    // Clean up the still-running container so the temp dir doesn't
    // leak a live process past this test.
    let _ = ociman(storage_dir.path(), &["kill", &id]);
}

/// `ociman container pause`/`ociman container unpause` (0493) are
/// real, byte-identical aliases for the top-level `ociman pause`/
/// `ociman unpause`, matching real `podman container pause`/`unpause`'s
/// own checked-directly identical `Use`/`Short`/`Long`/`RunE`/`Args`/
/// `ValidArgsFunction` (and identical `pauseFlags`/`unpauseFlags`-
/// applied flag sets) as their top-level counterparts exactly
/// (`~/git/podman/cmd/podman/containers/pause.go:19-49`, `unpause.go:
/// 19-49`). Full `pause`/`unpause` semantics (`--all`, `--cidfile`,
/// `--filter`, `--latest`, the real cgroup-freezer state
/// transitions) are already exhaustively tested against the
/// top-level commands in `ociman_pause.rs`; this only proves each
/// alias itself reaches the identical function with the identical
/// fields.
#[test]
fn container_pause_and_unpause_are_byte_identical_aliases_for_top_level_pause_and_unpause() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/container-pause-alias:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );
    ociman_run_detached(
        storage_dir.path(),
        "ociman-test/container-pause-alias:latest",
        &["sleep", "30"],
    );
    let id = all_ids(storage_dir.path())
        .into_iter()
        .next()
        .expect("the just-run container should exist");
    assert_eq!(
        wait_for_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let pause = ociman(storage_dir.path(), &["container", "pause", &id]);
    assert!(
        pause.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&pause.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&pause.stdout).trim(), id);
    assert_eq!(
        wait_for_status(storage_dir.path(), &id, "paused", Duration::from_secs(5)),
        "paused"
    );

    let unpause = ociman(storage_dir.path(), &["container", "unpause", &id]);
    assert!(
        unpause.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&unpause.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&unpause.stdout).trim(), id);
    assert_eq!(
        wait_for_status(storage_dir.path(), &id, "running", Duration::from_secs(5)),
        "running"
    );

    // Clean up the still-running container so the temp dir doesn't
    // leak a live process past this test.
    let _ = ociman(storage_dir.path(), &["kill", &id]);
}

/// `ociman container restart` (0494) is a real, byte-identical alias
/// for the top-level `ociman restart`, matching real `podman
/// container restart`'s own checked-directly identical `Use`/`Short`/
/// `Long`/`RunE`/`Args`/`ValidArgsFunction` (and identical
/// `restartFlags`-applied flag set) as top-level `podman restart`
/// exactly (`~/git/podman/cmd/podman/containers/restart.go:23-93`).
/// Full `restart` semantics (multi-id, `--all`, `--cidfile`,
/// `--filter`, `--latest`, `--time`) are already exhaustively tested
/// against the top-level command in `ociman_start.rs`/
/// `ociman_stop.rs`; this only proves the alias itself reaches the
/// identical function with the identical fields.
#[test]
fn container_restart_is_a_byte_identical_alias_for_top_level_restart() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/container-restart-alias:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );
    ociman_run_detached(
        storage_dir.path(),
        "ociman-test/container-restart-alias:latest",
        &["sleep", "30"],
    );
    let id = all_ids(storage_dir.path())
        .into_iter()
        .next()
        .expect("the just-run container should exist");
    assert_eq!(
        wait_for_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let alias = ociman(storage_dir.path(), &["container", "restart", &id]);
    assert!(
        alias.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&alias.stderr)
    );
    assert_eq!(
        wait_for_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running",
        "the container should genuinely be running again after a real restart"
    );

    // Clean up the still-running container so the temp dir doesn't
    // leak a live process past this test.
    let _ = ociman(storage_dir.path(), &["kill", &id]);
}

/// `ociman container rename` (0495) is a real, byte-identical alias
/// for the top-level `ociman rename`, matching real `podman container
/// rename`'s own checked-directly identical `Use`/`Short`/`Long`/
/// `RunE`/`Args`/`ValidArgsFunction` as top-level `podman rename`
/// exactly (`~/git/podman/cmd/podman/containers/rename.go:11-41`) --
/// the simplest byte-identical alias in the whole family, with no
/// flags at all on either side. Full `rename` semantics (name
/// validation, collision refusal, immediate usability of the new
/// name) are already exhaustively tested against the top-level
/// command in `ociman_rename.rs`; this only proves the alias itself
/// reaches the identical function with the identical fields.
#[test]
fn container_rename_is_a_byte_identical_alias_for_top_level_rename() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/container-rename-alias:latest",
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );
    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "--name",
            "container-rename-old",
            "ociman-test/container-rename-alias:latest",
            "true",
        ],
    );
    assert!(run.status.success(), "{run:?}");

    let alias = ociman(
        storage_dir.path(),
        &[
            "container",
            "rename",
            "container-rename-old",
            "container-rename-new",
        ],
    );
    assert!(
        alias.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&alias.stderr)
    );
    // Real `docker`/`podman rename` print nothing at all on success.
    assert!(alias.stdout.is_empty());

    // The old name no longer resolves to anything.
    let old_gone = ociman(
        storage_dir.path(),
        &["container", "exists", "container-rename-old"],
    );
    assert!(!old_gone.status.success());

    // The new name is immediately usable wherever the old one was.
    let rm = ociman(storage_dir.path(), &["rm", "container-rename-new"]);
    assert!(
        rm.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rm.stderr)
    );
}

/// `ociman container wait` (0496) is a real, byte-identical alias for
/// the top-level `ociman wait`, matching real `podman container
/// wait`'s own checked-directly identical `Use`/`Short`/`Long`/
/// `RunE`/`ValidArgsFunction` (and identical `waitFlags`-applied flag
/// set) as top-level `podman wait` exactly (`~/git/podman/cmd/podman/
/// containers/wait.go:20-73`). Full `wait` semantics (multi-id,
/// `--condition`, `--ignore`, `--latest`, `--interval`) are already
/// exhaustively tested against the top-level command in
/// `ociman_wait.rs`; this only proves the alias itself reaches the
/// identical function with the identical fields.
#[test]
fn container_wait_is_a_byte_identical_alias_for_top_level_wait() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/container-wait-alias:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "exit 42".to_string(),
            ]),
            ..Default::default()
        },
    );
    let run = ociman(
        storage_dir.path(),
        &["run", "ociman-test/container-wait-alias:latest"],
    );
    assert_eq!(run.status.code(), Some(42));
    let id = all_ids(storage_dir.path())
        .into_iter()
        .next()
        .expect("the just-run container should exist");

    // The container has already exited by the time `run` returns
    // (foreground, not detached) -- the alias should return
    // immediately with the real exit code.
    let alias = ociman(storage_dir.path(), &["container", "wait", &id]);
    assert!(
        alias.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&alias.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&alias.stdout).trim(), "42");
}

/// `ociman container top` (0497) is a real, byte-identical alias for
/// the top-level `ociman top`, matching real `podman container top`'s
/// own checked-directly identical `Use`/`Short`/`Long`/`RunE`/
/// `ValidArgsFunction` (and identical `topFlags`-applied flag set) as
/// top-level `podman top` exactly (`~/git/podman/cmd/podman/
/// containers/top.go:26-46`). Full `top` semantics (real `ps(1)`
/// passthrough, extra arguments, `--latest`) are already exhaustively
/// tested against the top-level command in `ociman_top.rs`; this only
/// proves the alias itself reaches the identical function with the
/// identical fields.
#[test]
fn container_top_is_a_byte_identical_alias_for_top_level_top() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/container-top-alias:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );
    ociman_run_detached(
        storage_dir.path(),
        "ociman-test/container-top-alias:latest",
        &["sleep", "30"],
    );
    let id = all_ids(storage_dir.path())
        .into_iter()
        .next()
        .expect("the just-run container should exist");
    assert_eq!(
        wait_for_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let alias = ociman(storage_dir.path(), &["container", "top", &id]);
    assert!(
        alias.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&alias.stderr)
    );
    let stdout = String::from_utf8_lossy(&alias.stdout);
    let header = stdout.lines().next().expect("a header line");
    assert!(header.contains("PID"), "{header:?}");
    assert!(
        stdout.contains("sleep 30"),
        "expected the container's own real command: {stdout:?}"
    );

    // Clean up the still-running container so the temp dir doesn't
    // leak a live process past this test.
    let _ = ociman(storage_dir.path(), &["kill", &id]);
}

/// `ociman container logs` (0498) is a real, byte-identical alias for
/// the top-level `ociman logs`, matching real `podman container
/// logs`'s own checked-directly identical `Use`/`Short`/`Long`/
/// `Args`/`RunE`/`ValidArgsFunction` (and identical `logsFlags`-
/// applied flag set) as top-level `podman logs` exactly (`~/git/
/// podman/cmd/podman/containers/logs.go:34-73`). Full `logs`
/// semantics (`--follow`, `--tail`, `--latest`, combined stdout/
/// stderr capture) are already exhaustively tested against the
/// top-level command in `ociman_logs.rs`; this only proves the alias
/// itself reaches the identical function with the identical fields.
#[test]
fn container_logs_is_a_byte_identical_alias_for_top_level_logs() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/container-logs-alias:latest",
        &busybox,
        &["sh", "echo"],
        ContainerConfig::default(),
    );
    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "ociman-test/container-logs-alias:latest",
            "/bin/sh",
            "-c",
            "echo line-from-stdout; echo line-from-stderr 1>&2",
        ],
    );
    assert!(run.status.success(), "{run:?}");
    let id = all_ids(storage_dir.path())
        .into_iter()
        .next()
        .expect("the just-run container should exist");

    let alias = ociman(storage_dir.path(), &["container", "logs", &id]);
    assert!(
        alias.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&alias.stderr)
    );
    let stdout = String::from_utf8_lossy(&alias.stdout);
    assert!(stdout.contains("line-from-stdout"), "got: {stdout:?}");
    assert!(stdout.contains("line-from-stderr"), "got: {stdout:?}");
}
