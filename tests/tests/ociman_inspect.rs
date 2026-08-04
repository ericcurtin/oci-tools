//! `ociman inspect` integration tests: real docker/podman's own
//! default resolution order — a container (by id or `--name`) is
//! tried first, falling back to an image if no such container exists
//! (checked directly against `~/git/podman/cmd/podman/inspect/
//! inspect.go`'s own `inspectAll`, see `docs/design/0094`). Same fully
//! offline seeded-image approach `ociman_run.rs` established.

use std::path::Path;
use std::process::Command;

use oci_spec_types::image::{ContainerConfig, HealthcheckConfig};
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

/// `ociman inspect --type image` (0409), matching real `podman
/// inspect --type image` exactly -- resolves *only* an image, never
/// falling back to (or even considering) a container of the exact
/// same name, unlike the default `--type all` behavior.
#[test]
fn inspect_type_image_never_resolves_a_container_of_the_same_name() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/inspect-type:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let create = ociman(
        storage_dir.path(),
        &[
            "create",
            "--name",
            "inspect-type-shared-name",
            "ociman-test/inspect-type:latest",
            "true",
        ],
    );
    assert!(create.status.success(), "{create:?}");

    // The default (`--type all`) resolves the container first.
    let default_inspect = ociman(storage_dir.path(), &["inspect", "inspect-type-shared-name"]);
    assert!(default_inspect.status.success());
    let default_view: serde_json::Value = serde_json::from_slice(&default_inspect.stdout).unwrap();
    assert!(default_view.get("status").is_some(), "{default_view:?}");

    // `--type image` on that exact same name must fail -- it's a
    // real container name, never an image reference at all, and
    // `--type image` never considers a container regardless.
    let image_inspect = ociman(
        storage_dir.path(),
        &["inspect", "--type", "image", "inspect-type-shared-name"],
    );
    assert!(!image_inspect.status.success());
    assert!(
        String::from_utf8_lossy(&image_inspect.stderr).contains("no such image"),
        "{image_inspect:?}"
    );

    // `--type image` on the real image reference still works.
    let image_inspect_ok = ociman(
        storage_dir.path(),
        &[
            "inspect",
            "--type",
            "image",
            "ociman-test/inspect-type:latest",
        ],
    );
    assert!(
        image_inspect_ok.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&image_inspect_ok.stderr)
    );
    let image_view: serde_json::Value = serde_json::from_slice(&image_inspect_ok.stdout).unwrap();
    assert!(image_view.get("architecture").is_some(), "{image_view:?}");
}

/// `ociman inspect --type container` (0409), matching real `podman
/// inspect --type container` exactly -- resolves *only* a container,
/// never falling back to an image even when the given reference
/// would otherwise resolve to a real one.
#[test]
fn inspect_type_container_never_resolves_an_image() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/inspect-type-container-only:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    // No container ever created -- only the image exists.
    let container_inspect = ociman(
        storage_dir.path(),
        &[
            "inspect",
            "--type",
            "container",
            "ociman-test/inspect-type-container-only:latest",
        ],
    );
    assert!(!container_inspect.status.success());
    assert!(
        String::from_utf8_lossy(&container_inspect.stderr).contains("no such container"),
        "{container_inspect:?}"
    );
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

/// `--label-file` (0402) reads `KEY=value`/bare-`KEY` entries from a
/// real file, matching real `podman create --label-file` exactly —
/// blank lines and `#`-comment lines (even with leading whitespace)
/// are skipped, the same shape `--env-file` already established;
/// merges with (never replaces) the image's own inherited labels the
/// same way `--label` itself does.
#[test]
fn create_label_file_reads_entries_from_a_real_file_and_merges_with_the_image() {
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
        "ociman-test/label-file:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            labels,
            ..Default::default()
        },
    );

    let label_file_dir = tempfile::tempdir().unwrap();
    let label_file_path = label_file_dir.path().join("labels.list");
    std::fs::write(
        &label_file_path,
        "\n  # a comment, with leading whitespace\nfrom.file=yes\nbarekey\n",
    )
    .unwrap();

    let create = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args(["create", "--name", "label-file-ctr", "--label-file"])
        .arg(&label_file_path)
        .args(["ociman-test/label-file:latest", "true"])
        .output()
        .expect("failed to spawn ociman create");
    assert!(
        create.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&create.stderr)
    );

    let inspect = ociman(storage_dir.path(), &["inspect", "label-file-ctr"]);
    assert!(inspect.status.success(), "{inspect:?}");
    let view: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert_eq!(
        view["labels"],
        serde_json::json!({
            "image.label": "fromimage",
            "from.file": "yes",
            "barekey": "",
        }),
        "{view:?}"
    );
}

/// `--label` always wins over `--label-file` for a shared key,
/// regardless of which one appears first on the command line —
/// matching real `podman`'s own identical `--env`/`--env-file`
/// precedence, the same fixed (not flag-order-dependent) rule this
/// project's own `combined_env` construction already established.
#[test]
fn create_label_flag_always_wins_over_label_file_regardless_of_order() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/label-file-precedence:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let label_file_dir = tempfile::tempdir().unwrap();
    let label_file_path = label_file_dir.path().join("labels.list");
    std::fs::write(&label_file_path, "shared=from-file\n").unwrap();

    // `--label` given *before* `--label-file` on the command line --
    // still wins, since precedence is fixed, not flag-order-dependent.
    let create = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args([
            "create",
            "--name",
            "label-file-precedence-ctr",
            "--label",
            "shared=from-flag",
            "--label-file",
        ])
        .arg(&label_file_path)
        .args(["ociman-test/label-file-precedence:latest", "true"])
        .output()
        .expect("failed to spawn ociman create");
    assert!(
        create.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&create.stderr)
    );

    let inspect = ociman(
        storage_dir.path(),
        &["inspect", "label-file-precedence-ctr"],
    );
    assert!(inspect.status.success(), "{inspect:?}");
    let view: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert_eq!(
        view["labels"],
        serde_json::json!({"shared": "from-flag"}),
        "--label should always win over --label-file, even given first on the command line"
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

/// `ociman inspect`'s own `started_at`/`finished_at` fields
/// (`docs/design/0370`): both absent for a container that has never
/// actually started at all yet (`ociman create`, no `start`).
#[test]
fn inspect_started_at_and_finished_at_are_absent_for_a_never_started_container() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/inspect-started-never:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let create = ociman(
        storage_dir.path(),
        &["create", "ociman-test/inspect-started-never:latest", "true"],
    );
    assert!(create.status.success(), "{create:?}");
    let id = String::from_utf8_lossy(&create.stdout).trim().to_string();

    let view: serde_json::Value =
        serde_json::from_slice(&ociman(storage_dir.path(), &["inspect", &id, "--json"]).stdout)
            .unwrap();
    assert!(view.get("started_at").is_none(), "{view:?}");
    assert!(view.get("finished_at").is_none(), "{view:?}");
}

/// Both fields are set once a container has actually run to
/// completion, `finished_at` never earlier than `started_at` (RFC3339
/// strings sort lexically the same as chronologically).
#[test]
fn inspect_started_at_and_finished_at_are_set_after_a_container_runs_to_completion() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/inspect-started-ran:latest",
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );
    let run = ociman(
        storage_dir.path(),
        &["run", "ociman-test/inspect-started-ran:latest", "true"],
    );
    assert!(run.status.success(), "{run:?}");
    let id = only_container_id(storage_dir.path());
    assert!(!id.is_empty());

    let view: serde_json::Value =
        serde_json::from_slice(&ociman(storage_dir.path(), &["inspect", &id, "--json"]).stdout)
            .unwrap();
    let started_at = view["started_at"]
        .as_str()
        .expect("started_at should be set");
    let finished_at = view["finished_at"]
        .as_str()
        .expect("finished_at should be set");
    assert!(
        finished_at >= started_at,
        "finished_at {finished_at:?} should be at or after started_at {started_at:?}"
    );
}

/// `ociman restart` overwrites `started_at` to the new start's own
/// time, matching real podman's own identical `StartedTime`
/// unconditional-overwrite behavior (checked directly, `~/git/podman/
/// libpod/runtime_ctr.go`) -- not just set once at the very first
/// start.
#[test]
fn inspect_restart_overwrites_started_at() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/inspect-restart-started:latest",
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );
    let run = ociman(
        storage_dir.path(),
        &["run", "ociman-test/inspect-restart-started:latest", "true"],
    );
    assert!(run.status.success(), "{run:?}");
    let id = only_container_id(storage_dir.path());
    assert!(!id.is_empty());

    let first_view: serde_json::Value =
        serde_json::from_slice(&ociman(storage_dir.path(), &["inspect", &id, "--json"]).stdout)
            .unwrap();
    let first_started_at = first_view["started_at"]
        .as_str()
        .expect("started_at should be set")
        .to_string();

    // `format_rfc3339_utc`'s own second-level precision (matching
    // real runc's `state.json`, see its own doc comment) needs a
    // real, full second of separation to reliably observe a
    // different value here.
    std::thread::sleep(std::time::Duration::from_millis(1100));

    let restart = ociman(storage_dir.path(), &["restart", &id]);
    assert!(restart.status.success(), "{restart:?}");

    let second_view: serde_json::Value =
        serde_json::from_slice(&ociman(storage_dir.path(), &["inspect", &id, "--json"]).stdout)
            .unwrap();
    let second_started_at = second_view["started_at"]
        .as_str()
        .expect("started_at should still be set")
        .to_string();
    assert!(
        second_started_at > first_started_at,
        "restart should overwrite started_at with a later value: {first_started_at:?} -> \
         {second_started_at:?}"
    );
}

/// `ociman inspect`'s own new `healthcheck` field (0442): a `run`/
/// `create --health-cmd` override takes precedence over the resolved
/// image's own declared `HEALTHCHECK` -- matching [`resolve_
/// effective_healthcheck`]'s own exact precedence, the same one
/// `ociman healthcheck run`/`ociman update --health-cmd` already
/// share.
#[test]
fn inspect_healthcheck_shows_a_health_cmd_override_taking_precedence_over_the_image() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/inspect-health-override:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            healthcheck: Some(HealthcheckConfig {
                test: vec![
                    "CMD".to_string(),
                    "test".to_string(),
                    "-f".to_string(),
                    "/image-healthy".to_string(),
                ],
                ..Default::default()
            }),
            ..Default::default()
        },
    );

    let create = ociman(
        storage_dir.path(),
        &[
            "create",
            "--health-cmd",
            "test -f /cli-healthy",
            "ociman-test/inspect-health-override:latest",
            "true",
        ],
    );
    assert!(create.status.success(), "{create:?}");
    let id = String::from_utf8_lossy(&create.stdout).trim().to_string();

    let inspect = ociman(storage_dir.path(), &["inspect", &id, "--json"]);
    assert!(inspect.status.success(), "{inspect:?}");
    let view: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert_eq!(
        view["healthcheck"]["Test"],
        serde_json::json!(["CMD-SHELL", "test -f /cli-healthy"]),
        "{view:?}"
    );
}

/// With no `--health-cmd`/`--no-healthcheck` override at all, `ociman
/// inspect`'s own `healthcheck` field falls back to the resolved
/// image's own declared `HEALTHCHECK`.
#[test]
fn inspect_healthcheck_falls_back_to_the_images_own_declared_one() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/inspect-health-image:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            healthcheck: Some(HealthcheckConfig {
                test: vec![
                    "CMD".to_string(),
                    "test".to_string(),
                    "-f".to_string(),
                    "/image-healthy".to_string(),
                ],
                ..Default::default()
            }),
            ..Default::default()
        },
    );

    let create = ociman(
        storage_dir.path(),
        &["create", "ociman-test/inspect-health-image:latest", "true"],
    );
    assert!(create.status.success(), "{create:?}");
    let id = String::from_utf8_lossy(&create.stdout).trim().to_string();

    let inspect = ociman(storage_dir.path(), &["inspect", &id, "--json"]);
    assert!(inspect.status.success(), "{inspect:?}");
    let view: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert_eq!(
        view["healthcheck"]["Test"],
        serde_json::json!(["CMD", "test", "-f", "/image-healthy"]),
        "{view:?}"
    );
}

/// A container with genuinely no healthcheck at all (neither the
/// image nor an explicit override declares one) omits the
/// `healthcheck` field entirely, not a `null`.
#[test]
fn inspect_healthcheck_field_is_absent_with_no_healthcheck_at_all() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/inspect-health-none:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let create = ociman(
        storage_dir.path(),
        &["create", "ociman-test/inspect-health-none:latest", "true"],
    );
    assert!(create.status.success(), "{create:?}");
    let id = String::from_utf8_lossy(&create.stdout).trim().to_string();

    let inspect = ociman(storage_dir.path(), &["inspect", &id, "--json"]);
    assert!(inspect.status.success(), "{inspect:?}");
    let view: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert!(view.get("healthcheck").is_none(), "{view:?}");
}

/// `ociman inspect`'s own `healthcheck` field reflects a *later*
/// `ociman update --health-cmd` change too, not just what the
/// container was originally created with -- proving this is a real,
/// live-resolved view (`ContainerInspectView::from_state`'s own
/// `resolve_effective_healthcheck` call), not a value snapshotted
/// once at creation time.
#[test]
fn inspect_healthcheck_reflects_a_later_update_health_cmd_change() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/inspect-health-update:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let create = ociman(
        storage_dir.path(),
        &["create", "ociman-test/inspect-health-update:latest", "true"],
    );
    assert!(create.status.success(), "{create:?}");
    let id = String::from_utf8_lossy(&create.stdout).trim().to_string();

    let before = ociman(storage_dir.path(), &["inspect", &id, "--json"]);
    let before_view: serde_json::Value = serde_json::from_slice(&before.stdout).unwrap();
    assert!(before_view.get("healthcheck").is_none(), "{before_view:?}");

    let update = ociman(
        storage_dir.path(),
        &["update", "--health-cmd", "test -f /updated-healthy", &id],
    );
    assert!(update.status.success(), "{update:?}");

    let after = ociman(storage_dir.path(), &["inspect", &id, "--json"]);
    let after_view: serde_json::Value = serde_json::from_slice(&after.stdout).unwrap();
    assert_eq!(
        after_view["healthcheck"]["Test"],
        serde_json::json!(["CMD-SHELL", "test -f /updated-healthy"]),
        "{after_view:?}"
    );
}

/// `ociman inspect --latest`/`-l` (matching real `podman inspect
/// --latest` exactly) inspects the single, real most-recently-
/// *created* container -- an earlier container's own, genuinely
/// different name must never be reported.
#[test]
fn inspect_latest_shows_the_most_recently_created_container() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/inspect-latest:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let older = ociman(
        storage_dir.path(),
        &[
            "create",
            "--name",
            "inspect-latest-older",
            "ociman-test/inspect-latest:latest",
            "true",
        ],
    );
    assert!(older.status.success(), "{older:?}");

    // A real, distinguishable creation-time gap.
    std::thread::sleep(std::time::Duration::from_secs(2));

    let newer = ociman(
        storage_dir.path(),
        &[
            "create",
            "--name",
            "inspect-latest-newer",
            "ociman-test/inspect-latest:latest",
            "true",
        ],
    );
    assert!(newer.status.success(), "{newer:?}");
    let newer_id = String::from_utf8_lossy(&newer.stdout).trim().to_string();

    let inspect = ociman(storage_dir.path(), &["inspect", "--latest"]);
    assert!(
        inspect.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&inspect.stderr)
    );
    let view: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert_eq!(view["name"], "inspect-latest-newer", "{view:?}");
    assert_eq!(view["id"], newer_id, "{view:?}");
}

/// `--latest` and an explicit reference together is a real, immediate
/// error, matching real podman's own exact wording -- a real,
/// deliberate divergence from `ociman diff --latest` (`0448`), which
/// has no such check at all.
#[test]
fn inspect_latest_combined_with_an_explicit_reference_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(storage_dir.path(), &["inspect", "--latest", "some-id"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("--latest and arguments cannot be used together"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `--latest --type image` is a real, immediate error, matching real
/// podman's own exact wording -- an image has no "most recently
/// created" resolution concept `--latest` could ever mean here.
#[test]
fn inspect_latest_combined_with_type_image_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(
        storage_dir.path(),
        &["inspect", "--latest", "--type", "image"],
    );
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("latest is not supported for type"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Neither `--latest` nor an explicit reference at all is a real,
/// immediate error.
#[test]
fn inspect_with_no_reference_and_no_latest_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(storage_dir.path(), &["inspect"]);
    assert!(!out.status.success());
}

/// `inspect --latest` on a genuinely empty store is a real, clear
/// error, matching real `podman inspect --latest`'s own
/// `ErrNoSuchCtr`.
#[test]
fn inspect_latest_on_an_empty_store_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    let out = ociman(storage_dir.path(), &["inspect", "--latest"]);
    assert!(!out.status.success());
}
