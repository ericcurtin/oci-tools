//! `ociman import` integration tests: creating a brand-new, single-
//! layer image straight from a plain tar, matching real `docker
//! import`/`podman import` (see `docs/design/0169`). Real, live
//! `podman import`/`docker import`/`podman export` interop (importing
//! a tar this project's own `ociman export` produced with a real
//! `podman import`, and importing a real `podman export`'s own tar
//! with `ociman import`, both round-tripping and actually running)
//! was additionally verified by hand during this feature's own
//! development, since a real `podman`/`docker` binary is not a
//! dependency this automated suite can assume is present everywhere
//! it runs -- see `docs/design/0169` for that record.

use std::path::Path;
use std::process::Command;

use oci_tools_tests::bin_path;

fn ociman(storage_root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
        .env_remove("OCI_TOOLS_LOG")
        .args(args)
        .output()
        .expect("failed to spawn ociman")
}

fn make_plain_tar(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, content) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, name, *content).unwrap();
    }
    builder.into_inner().unwrap()
}

/// A plain tar with a real `bin/busybox` binary (plus one symlinked
/// applet per name in `applets`) alongside `files` -- unlike
/// [`make_plain_tar`] alone, an image imported from this one can
/// actually `ociman run` a real command (`cat`, `sh`, ...) inside
/// itself, matching the same shape `oci_tools_tests::seed_image`
/// builds for every other test file's own already-stored images.
fn make_busybox_tar(busybox: &Path, applets: &[&str], files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    let busybox_bytes = std::fs::read(busybox).unwrap();
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(busybox_bytes.len() as u64);
    header.set_mode(0o755);
    builder
        .append_data(&mut header, "bin/busybox", busybox_bytes.as_slice())
        .unwrap();
    for applet in applets {
        let mut link_header = tar::Header::new_gnu();
        link_header.set_entry_type(tar::EntryType::Symlink);
        link_header.set_mode(0o777);
        link_header.set_size(0);
        builder
            .append_link(&mut link_header, format!("bin/{applet}"), "busybox")
            .unwrap();
    }
    for (name, content) in files {
        let mut file_header = tar::Header::new_gnu();
        file_header.set_size(content.len() as u64);
        file_header.set_mode(0o644);
        file_header.set_cksum();
        builder
            .append_data(&mut file_header, name, *content)
            .unwrap();
    }
    builder.into_inner().unwrap()
}

#[test]
fn import_of_a_missing_input_file_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let import = ociman(
        storage_dir.path(),
        &["import", "/nonexistent/path/to/nothing.tar"],
    );
    assert!(!import.status.success());
    assert!(
        String::from_utf8_lossy(&import.stderr).contains("opening"),
        "{}",
        String::from_utf8_lossy(&import.stderr)
    );
}

#[test]
fn import_creates_a_single_layer_image_and_tags_it() {
    let Some(busybox) = oci_tools_tests::busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let tar_bytes = make_busybox_tar(&busybox, &["cat"], &[("hello.txt", b"hello from import\n")]);
    let tar_path = storage_dir.path().join("in.tar");
    std::fs::write(&tar_path, &tar_bytes).unwrap();

    let import = ociman(
        storage_dir.path(),
        &[
            "import",
            "-m",
            "a real test import",
            tar_path.to_str().unwrap(),
            "example.com/import-test:v1",
        ],
    );
    assert!(
        import.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&import.stderr)
    );
    let digest = String::from_utf8_lossy(&import.stdout).trim().to_string();
    assert!(digest.starts_with("sha256:"), "{digest:?}");

    let inspect = ociman(
        storage_dir.path(),
        &["inspect", "example.com/import-test:v1", "--json"],
    );
    assert!(inspect.status.success());
    let parsed: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert_eq!(parsed["rootfs"]["diff_ids"].as_array().unwrap().len(), 1);
    assert_eq!(
        parsed["history"][0]["comment"], "a real test import",
        "{parsed}"
    );

    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "--rm",
            "example.com/import-test:v1",
            "cat",
            "hello.txt",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "hello from import\n");
}

#[test]
fn import_with_no_reference_creates_an_untagged_image() {
    let storage_dir = tempfile::tempdir().unwrap();
    let tar_bytes = make_plain_tar(&[("a.txt", b"a\n")]);
    let tar_path = storage_dir.path().join("in.tar");
    std::fs::write(&tar_path, &tar_bytes).unwrap();

    let import = ociman(storage_dir.path(), &["import", tar_path.to_str().unwrap()]);
    assert!(
        import.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&import.stderr)
    );

    let images = ociman(storage_dir.path(), &["images"]);
    assert!(
        !String::from_utf8_lossy(&images.stdout).contains("example.com"),
        "an untagged import must not create any tag pointer: {}",
        String::from_utf8_lossy(&images.stdout)
    );
}

#[test]
fn import_reads_from_standard_input_when_path_is_a_dash() {
    let Some(busybox) = oci_tools_tests::busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let tar_bytes = make_busybox_tar(&busybox, &["cat"], &[("stdin.txt", b"from stdin\n")]);

    let mut child = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args(["import", "-", "example.com/import-stdin:v1"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write as _;
    child.stdin.take().unwrap().write_all(&tar_bytes).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "--rm",
            "example.com/import-stdin:v1",
            "cat",
            "stdin.txt",
        ],
    );
    assert!(run.status.success());
    assert_eq!(String::from_utf8_lossy(&run.stdout), "from stdin\n");
}

#[test]
fn import_applies_change_instructions() {
    let Some(busybox) = oci_tools_tests::busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let tar_bytes = make_busybox_tar(&busybox, &["cat"], &[("f.txt", b"f\n")]);
    let tar_path = storage_dir.path().join("in.tar");
    std::fs::write(&tar_path, &tar_bytes).unwrap();

    let import = ociman(
        storage_dir.path(),
        &[
            "import",
            "-c",
            "ENV FOO=bar",
            "-c",
            "CMD [\"cat\", \"f.txt\"]",
            tar_path.to_str().unwrap(),
            "example.com/import-change:v1",
        ],
    );
    assert!(
        import.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&import.stderr)
    );

    let run = ociman(
        storage_dir.path(),
        &["run", "--rm", "example.com/import-change:v1"],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "f\n");
}

#[test]
fn import_rejects_a_build_only_change_instruction() {
    let storage_dir = tempfile::tempdir().unwrap();
    let tar_bytes = make_plain_tar(&[("f.txt", b"f\n")]);
    let tar_path = storage_dir.path().join("in.tar");
    std::fs::write(&tar_path, &tar_bytes).unwrap();

    let import = ociman(
        storage_dir.path(),
        &[
            "import",
            "-c",
            "RUN echo not allowed",
            tar_path.to_str().unwrap(),
        ],
    );
    assert!(!import.status.success());
}

/// The full real round trip through both real CLI commands: export a
/// real, seeded, already-stopped container, import the archive back
/// as a brand-new image, and confirm the imported image is fully
/// usable (runs, has the right content).
#[test]
fn export_then_import_round_trips_through_the_real_cli() {
    let Some(busybox) = oci_tools_tests::busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        storage_dir.path().join(".rootless-overlay-supported"),
        "false",
    )
    .unwrap();
    let store = oci_store::Store::open(storage_dir.path()).unwrap();
    oci_tools_tests::seed_image(
        &store,
        "ociman-test/export-import-source:latest",
        &busybox,
        &["sh", "cat"],
        oci_spec_types::image::ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "echo round trip me > /rt.txt".to_string(),
            ]),
            ..Default::default()
        },
    );
    let run = ociman(
        storage_dir.path(),
        &["run", "ociman-test/export-import-source:latest"],
    );
    assert!(run.status.success());
    let ps = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    let id = String::from_utf8_lossy(&ps.stdout).trim().to_string();

    let archive_path = storage_dir.path().join("exported.tar");
    let export = ociman(
        storage_dir.path(),
        &["export", "-o", archive_path.to_str().unwrap(), &id],
    );
    assert!(export.status.success());

    let import = ociman(
        storage_dir.path(),
        &[
            "import",
            archive_path.to_str().unwrap(),
            "example.com/round-tripped:v1",
        ],
    );
    assert!(
        import.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&import.stderr)
    );

    let run2 = ociman(
        storage_dir.path(),
        &[
            "run",
            "--rm",
            "example.com/round-tripped:v1",
            "cat",
            "rt.txt",
        ],
    );
    assert!(
        run2.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run2.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run2.stdout), "round trip me\n");
}
