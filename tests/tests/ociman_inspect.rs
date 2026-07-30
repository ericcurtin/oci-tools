//! `ociman inspect` integration tests: real docker/podman's own
//! default resolution order — a container (by id or `--name`) is
//! tried first, falling back to an image if no such container exists
//! (checked directly against `~/git/podman/cmd/podman/inspect/
//! inspect.go`'s own `inspectAll`, see `docs/design/0094`). Same fully
//! offline seeded-image approach `ociman_run.rs` established.

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

fn only_container_id(storage_root: &Path) -> String {
    let out = ociman(storage_root, &["ps", "-a", "-q"]);
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn inspect_by_container_name_returns_the_real_container_state() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/inspect-basic:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "exit 5".to_string(),
            ]),
            ..Default::default()
        },
    );

    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "--name",
            "inspect-me",
            "ociman-test/inspect-basic:latest",
        ],
    );
    assert_eq!(run.status.code(), Some(5));

    let inspect = ociman(storage_dir.path(), &["inspect", "inspect-me"]);
    assert!(
        inspect.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&inspect.stderr)
    );
    let view: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert_eq!(view["name"], "inspect-me");
    assert_eq!(view["status"], "stopped");
    assert_eq!(view["pid"], 0);
    assert_eq!(view["exit_code"], 5);
    assert_eq!(
        view["image"], "docker.io/ociman-test/inspect-basic:latest",
        "{view:?}"
    );
    assert!(
        view["bundle"]
            .as_str()
            .unwrap()
            .contains(view["id"].as_str().unwrap()),
        "{view:?}"
    );
}

#[test]
fn inspect_by_container_id_returns_the_same_data_as_by_name() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/inspect-by-id:latest",
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
        &["run", "ociman-test/inspect-by-id:latest"],
    );
    assert!(run.status.success());
    let id = only_container_id(storage_dir.path());
    assert!(!id.is_empty());

    let inspect = ociman(storage_dir.path(), &["inspect", &id]);
    assert!(inspect.status.success());
    let view: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert_eq!(view["id"], id);
    // No `--name` given, so the field is omitted entirely (matches
    // `ContainerView`'s own established `skip_serializing_if` for the
    // same field).
    assert!(view.get("name").is_none(), "{view:?}");
}

#[test]
fn inspect_falls_back_to_an_image_when_no_such_container_exists() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/inspect-image-only:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    // No container ever created -- only the image exists.
    let inspect = ociman(
        storage_dir.path(),
        &["inspect", "ociman-test/inspect-image-only:latest"],
    );
    assert!(
        inspect.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&inspect.stderr)
    );
    let config: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    // A real `ImageConfig`, not a `ContainerInspectView` -- has
    // `architecture`/`os`, not `status`/`pid`.
    assert!(config.get("architecture").is_some(), "{config:?}");
    assert!(config.get("status").is_none(), "{config:?}");
}

#[test]
fn inspect_of_an_unknown_reference_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(
        storage_dir.path(),
        &["inspect", "nothing-matches-this-at-all"],
    );
    assert!(!out.status.success());
}

/// Real docker/podman rule, checked directly: `inspect` (and `rmi`)
/// resolve by image ID too, not just a tag reference -- the exact
/// short digest `ociman images`' own `DIGEST` column already prints.
#[test]
fn inspect_resolves_a_real_image_by_its_own_short_id() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/inspect-by-id:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let record = store
        .resolve_image("docker.io/ociman-test/inspect-by-id:latest")
        .unwrap()
        .unwrap();
    let full_hex = record.manifest_digest.hex().to_string();
    let short_id = &full_hex[..12];

    let inspect = ociman(storage_dir.path(), &["inspect", short_id]);
    assert!(
        inspect.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&inspect.stderr)
    );
    let config: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert!(config.get("architecture").is_some(), "{config:?}");

    // The full digest (with and without the `sha256:` prefix) works
    // too, matching real `docker inspect <full-id>`.
    let inspect_full = ociman(storage_dir.path(), &["inspect", &full_hex]);
    assert!(inspect_full.status.success());
    let inspect_prefixed = ociman(
        storage_dir.path(),
        &["inspect", &format!("sha256:{full_hex}")],
    );
    assert!(inspect_prefixed.status.success());
}

/// Two tags pointing at the exact same image (`ociman tag`) must never
/// make resolving that image by its own (now doubly-referenced) ID
/// ambiguous -- only two genuinely *different* images sharing a
/// digest prefix should be.
#[test]
fn inspect_by_id_is_not_ambiguous_when_multiple_tags_share_the_same_digest() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/inspect-by-id-aliased:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let tag = ociman(
        storage_dir.path(),
        &[
            "tag",
            "ociman-test/inspect-by-id-aliased:latest",
            "ociman-test/inspect-by-id-aliased:v2",
        ],
    );
    assert!(tag.status.success());

    let record = store
        .resolve_image("docker.io/ociman-test/inspect-by-id-aliased:latest")
        .unwrap()
        .unwrap();
    let short_id = &record.manifest_digest.hex()[..12];

    let inspect = ociman(storage_dir.path(), &["inspect", short_id]);
    assert!(
        inspect.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&inspect.stderr)
    );
}

/// `ociman run`/`create --label` (0274): a container with no explicit
/// `--label` still shows its base image's own real `LABEL`s via
/// `ociman inspect`'s own `labels` field, matching real `podman
/// create`/`podman inspect`'s checked-directly behavior exactly.
#[test]
fn inspect_shows_the_image_own_inherited_labels_with_no_explicit_label_flag() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    let mut labels = std::collections::BTreeMap::new();
    labels.insert("image.label".to_string(), "fromimage".to_string());
    seed_image(
        &store,
        "ociman-test/label-inherit:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            labels,
            ..Default::default()
        },
    );

    let create = ociman(
        storage_dir.path(),
        &[
            "create",
            "--name",
            "label-inherit-ctr",
            "ociman-test/label-inherit:latest",
            "true",
        ],
    );
    assert!(create.status.success(), "{create:?}");

    let inspect = ociman(storage_dir.path(), &["inspect", "label-inherit-ctr"]);
    assert!(
        inspect.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&inspect.stderr)
    );
    let view: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert_eq!(
        view["labels"],
        serde_json::json!({"image.label": "fromimage"}),
        "{view:?}"
    );
}

/// `--label KEY=VALUE`/bare `KEY` merges with (rather than replacing)
/// the image's own inherited labels, a same-key `--label` overriding
/// the image's own value — matching real `podman create --label`'s
/// own checked-directly behavior exactly.
#[test]
fn create_label_merges_with_and_overrides_the_image_own_inherited_labels() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    let mut labels = std::collections::BTreeMap::new();
    labels.insert("image.label".to_string(), "fromimage".to_string());
    labels.insert("shared.key".to_string(), "fromimage".to_string());
    seed_image(
        &store,
        "ociman-test/label-merge:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            labels,
            ..Default::default()
        },
    );

    let create = ociman(
        storage_dir.path(),
        &[
            "create",
            "--name",
            "label-merge-ctr",
            "--label",
            "own.label=fromcli",
            "--label",
            "barekey",
            "--label",
            "shared.key=fromcli",
            "ociman-test/label-merge:latest",
            "true",
        ],
    );
    assert!(create.status.success(), "{create:?}");

    let inspect = ociman(storage_dir.path(), &["inspect", "label-merge-ctr"]);
    assert!(inspect.status.success(), "{inspect:?}");
    let view: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert_eq!(
        view["labels"],
        serde_json::json!({
            "image.label": "fromimage",
            "shared.key": "fromcli",
            "own.label": "fromcli",
            "barekey": "",
        }),
        "{view:?}"
    );
}

/// `inspect --format` (0332) renders a single scalar field with no
/// surrounding JSON quoting -- matching real `podman inspect --format
/// '{{.Field}}'`'s own default scalar-rendering exactly, just against
/// this project's own (lowercase) field names.
#[test]
fn inspect_format_renders_a_single_scalar_field() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/inspect-format:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let create = ociman(
        storage_dir.path(),
        &[
            "create",
            "--name",
            "format-me",
            "ociman-test/inspect-format:latest",
            "true",
        ],
    );
    assert!(create.status.success(), "{create:?}");

    let format = ociman(
        storage_dir.path(),
        &["inspect", "format-me", "--format", "{{.status}}"],
    );
    assert!(
        format.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&format.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&format.stdout).trim(),
        "created",
        "a scalar string field must render with no surrounding JSON quotes"
    );
}

/// Multiple `{{.field}}` placeholders in one template, mixed with
/// literal text, all get substituted -- and a numeric field (`pid`)
/// renders as a plain number, matching Go's own default numeric
/// scalar rendering.
#[test]
fn inspect_format_supports_multiple_placeholders_and_literal_text() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/inspect-format-multi:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let create = ociman(
        storage_dir.path(),
        &[
            "create",
            "--name",
            "format-multi",
            "ociman-test/inspect-format-multi:latest",
            "true",
        ],
    );
    assert!(create.status.success(), "{create:?}");

    let format = ociman(
        storage_dir.path(),
        &[
            "inspect",
            "format-multi",
            "--format",
            "status={{.status}} pid={{.pid}}",
        ],
    );
    assert!(format.status.success());
    assert_eq!(
        String::from_utf8_lossy(&format.stdout).trim(),
        "status=created pid=0"
    );
}

/// A nested field (dot-path navigation through a JSON object) resolves
/// correctly, and an array field renders as its own compact JSON
/// representation -- matching real image-config inspection's own
/// nested `Config`/`RootFS` shape.
#[test]
fn inspect_format_navigates_a_nested_field_on_an_image() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/inspect-format-nested:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            cmd: Some(vec!["/bin/sh".to_string()]),
            ..Default::default()
        },
    );

    let format = ociman(
        storage_dir.path(),
        &[
            "inspect",
            "ociman-test/inspect-format-nested:latest",
            "--format",
            "{{.config.Cmd}}",
        ],
    );
    assert!(
        format.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&format.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&format.stdout).trim(),
        "[\"/bin/sh\"]"
    );
}

/// An unresolvable field path is a real, immediate error, matching
/// real Go templates' own "can't evaluate field" failure for a typo'd
/// field name rather than a silent empty string.
#[test]
fn inspect_format_of_an_unknown_field_is_a_clear_error() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/inspect-format-error:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let create = ociman(
        storage_dir.path(),
        &[
            "create",
            "--name",
            "format-error",
            "ociman-test/inspect-format-error:latest",
            "true",
        ],
    );
    assert!(create.status.success(), "{create:?}");

    let format = ociman(
        storage_dir.path(),
        &["inspect", "format-error", "--format", "{{.nosuchfield}}"],
    );
    assert!(!format.status.success());
    assert!(
        String::from_utf8_lossy(&format.stderr).contains("no field"),
        "{}",
        String::from_utf8_lossy(&format.stderr)
    );
}

/// `ociman inspect -s`/`--size` (0352), matching real `podman inspect
/// -s`/`--size` exactly (`~/git/podman/cmd/podman/inspect/
/// inspect.go`): a plain `inspect` shows no size information at all
/// (opt-in, matching real podman's own identical on-demand-only
/// computation); `--size` adds a real, nested `size` object with
/// `rw_size`/`root_fs_size`, `root_fs_size` always at least `rw_size`
/// (image size + rw size, `0342`'s own already-established formula).
#[test]
fn inspect_size_flag_reports_a_real_size_object_for_a_container() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/inspect-size:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let create = ociman(
        storage_dir.path(),
        &[
            "create",
            "--name",
            "inspect-size-test",
            "ociman-test/inspect-size:latest",
            "true",
        ],
    );
    assert!(create.status.success(), "{create:?}");

    let without_size = ociman(storage_dir.path(), &["inspect", "inspect-size-test"]);
    assert!(without_size.status.success());
    let view: serde_json::Value = serde_json::from_slice(&without_size.stdout).unwrap();
    assert!(
        view.get("size").is_none(),
        "a plain inspect must show no size information at all: {view:?}"
    );

    let with_size = ociman(storage_dir.path(), &["inspect", "-s", "inspect-size-test"]);
    assert!(
        with_size.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&with_size.stderr)
    );
    let view: serde_json::Value = serde_json::from_slice(&with_size.stdout).unwrap();
    let size = &view["size"];
    assert!(size["rw_size"].as_u64().is_some(), "{view:?}");
    assert!(
        size["root_fs_size"].as_u64().unwrap() >= size["rw_size"].as_u64().unwrap(),
        "root_fs_size (image + rw) must be at least as large as rw_size alone: {view:?}"
    );

    // `--size`/`--format` compose the same way `ps --size`'s own
    // `render_format_template` reuse already does (0342) -- no
    // special-casing needed at all.
    let format = ociman(
        storage_dir.path(),
        &[
            "inspect",
            "--size",
            "--format",
            "{{.size.rw_size}}",
            "inspect-size-test",
        ],
    );
    assert!(
        format.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&format.stderr)
    );
}

/// `--size` is a real, immediate error for an image, matching real
/// `podman inspect -s`'s own identical, checked-directly restriction
/// exactly (`"size is not supported for type"`).
#[test]
fn inspect_size_flag_is_a_clear_error_for_an_image() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/inspect-size-image:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let out = ociman(
        storage_dir.path(),
        &["inspect", "-s", "ociman-test/inspect-size-image:latest"],
    );
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("not supported"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `ociman inspect`'s own `mounts` field (`docs/design/0369`): a real
/// bind mount and a real named volume are both surfaced, the named
/// one carrying its own volume name; a container with no extra
/// mounts at all reports no `mounts` field whatsoever (not an empty
/// array), matching `ContainerView::size`'s own identical opt-in-field
/// convention.
#[test]
fn inspect_mounts_reports_bind_mounts_and_named_volumes_but_omits_the_field_when_empty() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/inspect-mounts:latest",
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );
    let host_dir = tempfile::tempdir().unwrap();

    let volume_create = ociman(
        storage_dir.path(),
        &["volume", "create", "inspect-mounts-vol"],
    );
    assert!(volume_create.status.success(), "{volume_create:?}");

    let create = ociman(
        storage_dir.path(),
        &[
            "create",
            "-v",
            &format!("{}:/data", host_dir.path().display()),
            "-v",
            "inspect-mounts-vol:/vol",
            "ociman-test/inspect-mounts:latest",
            "true",
        ],
    );
    assert!(create.status.success(), "{create:?}");
    let id = String::from_utf8_lossy(&create.stdout).trim().to_string();

    let inspect = ociman(storage_dir.path(), &["inspect", &id, "--json"]);
    assert!(inspect.status.success(), "{inspect:?}");
    let view: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    let mounts = view["mounts"]
        .as_array()
        .expect("mounts should be an array");
    assert_eq!(mounts.len(), 2, "{mounts:?}");

    let bind = mounts
        .iter()
        .find(|m| m["destination"] == "/data")
        .expect("the bind mount should be present");
    assert_eq!(
        bind["source"],
        serde_json::json!(host_dir.path().to_string_lossy())
    );
    assert!(bind.get("volume").is_none(), "{bind:?}");

    let volume = mounts
        .iter()
        .find(|m| m["destination"] == "/vol")
        .expect("the named-volume mount should be present");
    assert_eq!(volume["volume"], "inspect-mounts-vol");
    assert!(
        volume["source"]
            .as_str()
            .unwrap()
            .ends_with("/volumes/inspect-mounts-vol/_data"),
        "{volume:?}"
    );

    // A plain container with no extra mounts at all reports no
    // `mounts` field whatsoever, not an empty array.
    let plain_create = ociman(
        storage_dir.path(),
        &["create", "ociman-test/inspect-mounts:latest", "true"],
    );
    assert!(plain_create.status.success(), "{plain_create:?}");
    let plain_id = String::from_utf8_lossy(&plain_create.stdout)
        .trim()
        .to_string();
    let plain_inspect = ociman(storage_dir.path(), &["inspect", &plain_id, "--json"]);
    let plain_view: serde_json::Value = serde_json::from_slice(&plain_inspect.stdout).unwrap();
    assert!(plain_view.get("mounts").is_none(), "{plain_view:?}");
}
