//! `ocibox export --bin`/`--app` integration tests (`docs/design/
//! 0252`, `0322`): exercises the actual built `ocibox` binary writing
//! (and removing) a real wrapper script/`.desktop` launcher that
//! routes an exported binary/application's own invocations through
//! `ocibox enter` — checked directly against real `distrobox
//! export`'s own actual shell implementation (`~/git/distrobox/
//! internal/inside-distrobox/assets/distrobox-export`), deliberately
//! without `--app`'s own icon-copying half (see `Command::Export`'s
//! own doc comment for exactly why not, and how the explicit `--box`
//! flag here diverges from real `distrobox export`'s own "detect
//! which box I'm running in" model).

use std::path::Path;
use std::process::Command;

use oci_spec_types::image::ContainerConfig;
use oci_store::Store;

use oci_tools_tests::{bin_path, busybox_path, seed_image};

fn ocibox(storage_root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin_path("ocibox"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
        .env_remove("OCI_TOOLS_LOG")
        .args(args)
        .output()
        .expect("failed to spawn ocibox")
}

/// Seeds a real busybox-based image and `create`s a box from it.
fn make_box(storage_dir: &tempfile::TempDir, name: &str) {
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ocibox-test/export-base:latest",
        &busybox_path().expect("busybox not found on $PATH"),
        &["sh", "echo"],
        ContainerConfig::default(),
    );
    let create = ocibox(
        storage_dir.path(),
        &[
            "create",
            "--image",
            "ocibox-test/export-base:latest",
            "--name",
            name,
        ],
    );
    assert!(
        create.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&create.stderr)
    );
}

#[test]
fn export_writes_a_real_executable_wrapper_that_actually_runs_the_binary() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    make_box(&storage_dir, "testbox");
    let export_dir = tempfile::tempdir().unwrap();

    let export = ocibox(
        storage_dir.path(),
        &[
            "export",
            "--box",
            "testbox",
            "--bin",
            "/bin/echo",
            "--export-path",
            export_dir.path().to_str().unwrap(),
        ],
    );
    assert!(
        export.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&export.stderr)
    );
    assert!(
        String::from_utf8_lossy(&export.stdout).contains("exported successfully"),
        "{}",
        String::from_utf8_lossy(&export.stdout)
    );

    let wrapper = export_dir.path().join("echo");
    assert!(wrapper.is_file(), "wrapper should exist at {wrapper:?}");
    let contents = std::fs::read_to_string(&wrapper).unwrap();
    assert!(contents.contains("ocibox_binary"), "{contents:?}");
    assert!(contents.contains("testbox"), "{contents:?}");
    assert!(contents.contains("/bin/echo"), "{contents:?}");

    // Real, executable, and actually runs the exported binary inside
    // the box via a real `ocibox enter` -- not just a plausible-
    // looking file. `ocibox` itself must resolve on $PATH here since
    // the wrapper's own `exec ocibox enter ...` line calls it by bare
    // name (matching real `distrobox-export`'s own identical
    // `${DISTROBOX_PATH:-"distrobox"}` convention); this test's own
    // build directory is prepended to $PATH for exactly that reason.
    let bin_dir = bin_path("ocibox").parent().unwrap().to_path_buf();
    let path_var = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let run = Command::new(&wrapper)
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .env("PATH", path_var)
        .arg("hello-from-wrapper")
        .output()
        .expect("failed to run the exported wrapper");
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "hello-from-wrapper"
    );
}

#[test]
fn export_of_a_missing_binary_is_a_clear_error() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    make_box(&storage_dir, "testbox");
    let export_dir = tempfile::tempdir().unwrap();

    let out = ocibox(
        storage_dir.path(),
        &[
            "export",
            "--box",
            "testbox",
            "--bin",
            "/bin/does-not-exist",
            "--export-path",
            export_dir.path().to_str().unwrap(),
        ],
    );
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("cannot find"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !export_dir.path().join("does-not-exist").exists(),
        "a failed export must leave no wrapper behind"
    );
}

#[test]
fn export_of_an_unknown_box_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    let export_dir = tempfile::tempdir().unwrap();

    let out = ocibox(
        storage_dir.path(),
        &[
            "export",
            "--box",
            "no-such-box",
            "--bin",
            "/bin/echo",
            "--export-path",
            export_dir.path().to_str().unwrap(),
        ],
    );
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no such box"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn export_delete_removes_the_wrapper_and_refuses_a_foreign_file() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    make_box(&storage_dir, "testbox");
    let export_dir = tempfile::tempdir().unwrap();

    let export = ocibox(
        storage_dir.path(),
        &[
            "export",
            "--box",
            "testbox",
            "--bin",
            "/bin/echo",
            "--export-path",
            export_dir.path().to_str().unwrap(),
        ],
    );
    assert!(export.status.success());
    let wrapper = export_dir.path().join("echo");
    assert!(wrapper.is_file());

    // A file that was never `ocibox export`ed (no marker comment) is
    // refused, matching real `distrobox export --delete`'s own
    // identical safety check -- confirmed the foreign file survives
    // completely untouched.
    let foreign = export_dir.path().join("foreign");
    std::fs::write(&foreign, "#!/bin/sh\necho not an export\n").unwrap();
    let refuse = ocibox(
        storage_dir.path(),
        &[
            "export",
            "--box",
            "testbox",
            "--bin",
            "/bin/foreign",
            "--export-path",
            export_dir.path().to_str().unwrap(),
            "--delete",
        ],
    );
    assert!(!refuse.status.success());
    assert!(
        String::from_utf8_lossy(&refuse.stderr).contains("not an ocibox-exported binary"),
        "{}",
        String::from_utf8_lossy(&refuse.stderr)
    );
    assert!(foreign.is_file(), "the foreign file must survive untouched");

    // The real, genuinely-exported wrapper deletes cleanly.
    let delete = ocibox(
        storage_dir.path(),
        &[
            "export",
            "--box",
            "testbox",
            "--bin",
            "/bin/echo",
            "--export-path",
            export_dir.path().to_str().unwrap(),
            "--delete",
        ],
    );
    assert!(
        delete.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&delete.stderr)
    );
    assert!(
        String::from_utf8_lossy(&delete.stdout).contains("removed successfully"),
        "{}",
        String::from_utf8_lossy(&delete.stdout)
    );
    assert!(!wrapper.exists(), "the wrapper should really be gone now");
}

#[test]
fn export_rejects_a_non_absolute_bin_path() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    make_box(&storage_dir, "testbox");
    let export_dir = tempfile::tempdir().unwrap();

    let out = ocibox(
        storage_dir.path(),
        &[
            "export",
            "--box",
            "testbox",
            "--bin",
            "relative/echo",
            "--export-path",
            export_dir.path().to_str().unwrap(),
        ],
    );
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("absolute path"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Writes a real `.desktop` file into a box's own rootfs, matching
/// what a real installed application would leave behind.
fn write_desktop_file(storage_dir: &tempfile::TempDir, box_name: &str, contents: &str) {
    let dir = storage_dir
        .path()
        .join("boxes")
        .join(box_name)
        .join("rootfs/usr/share/applications");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("myapp.desktop"), contents).unwrap();
}

const SAMPLE_DESKTOP_FILE: &str = "[Desktop Entry]\nType=Application\nName=My App\n\
Exec=/usr/bin/myapp --flag\nTryExec=/usr/bin/myapp\nIcon=myapp-icon\n";

/// `export --app` (0322) writes a rewritten `.desktop` launcher whose
/// own `Exec=` routes through `ocibox enter`, strips `TryExec=`
/// entirely (it would check for the host's own binary, not the
/// box's), and leaves `Icon=` completely untouched (icon handling is
/// a real, separate, deferred increment — see `Command::Export`'s own
/// doc comment) — matching real `distrobox export --app`'s own core
/// `Exec=`-rewriting mechanism, checked directly against
/// `~/git/distrobox`'s own real shell implementation.
#[test]
fn export_app_writes_a_rewritten_desktop_file() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    make_box(&storage_dir, "testbox");
    write_desktop_file(&storage_dir, "testbox", SAMPLE_DESKTOP_FILE);
    let export_dir = tempfile::tempdir().unwrap();

    let export = ocibox(
        storage_dir.path(),
        &[
            "export",
            "--box",
            "testbox",
            "--app",
            "My App",
            "--export-path",
            export_dir.path().to_str().unwrap(),
        ],
    );
    assert!(
        export.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&export.stderr)
    );
    assert!(
        String::from_utf8_lossy(&export.stdout).contains("exported successfully"),
        "{}",
        String::from_utf8_lossy(&export.stdout)
    );

    let dest = export_dir.path().join("testbox-myapp.desktop");
    assert!(dest.is_file(), "launcher should exist at {dest:?}");
    let contents = std::fs::read_to_string(&dest).unwrap();
    assert!(contents.contains("ocibox_app_export"), "{contents:?}");
    assert!(
        contents.contains("Exec=ocibox enter testbox -- /usr/bin/myapp --flag"),
        "{contents:?}"
    );
    assert!(
        !contents.contains("TryExec="),
        "TryExec should be stripped entirely: {contents:?}"
    );
    assert!(
        contents.contains("Icon=myapp-icon"),
        "Icon= should be left completely untouched: {contents:?}"
    );
    assert!(contents.contains("Name=My App"), "{contents:?}");
}

/// `export --app` also accepts an absolute path (inside the box's own
/// rootfs) to a `.desktop` file directly, matching real `distrobox
/// export --app`'s own identical either/or interpretation.
#[test]
fn export_app_accepts_an_explicit_desktop_file_path() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    make_box(&storage_dir, "testbox");
    write_desktop_file(&storage_dir, "testbox", SAMPLE_DESKTOP_FILE);
    let export_dir = tempfile::tempdir().unwrap();

    let export = ocibox(
        storage_dir.path(),
        &[
            "export",
            "--box",
            "testbox",
            "--app",
            "/usr/share/applications/myapp.desktop",
            "--export-path",
            export_dir.path().to_str().unwrap(),
        ],
    );
    assert!(
        export.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&export.stderr)
    );
    assert!(export_dir.path().join("testbox-myapp.desktop").is_file());
}

/// `export --app --delete` removes a previously-exported launcher,
/// refusing a foreign file with no marker comment -- the same
/// marker/safety-check convention `--bin --delete` already
/// established.
#[test]
fn export_app_delete_removes_the_launcher_and_refuses_a_foreign_file() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    make_box(&storage_dir, "testbox");
    write_desktop_file(&storage_dir, "testbox", SAMPLE_DESKTOP_FILE);
    let export_dir = tempfile::tempdir().unwrap();

    let export = ocibox(
        storage_dir.path(),
        &[
            "export",
            "--box",
            "testbox",
            "--app",
            "My App",
            "--export-path",
            export_dir.path().to_str().unwrap(),
        ],
    );
    assert!(export.status.success());
    let dest = export_dir.path().join("testbox-myapp.desktop");
    assert!(dest.is_file());

    // A foreign file at the exact same destination our own search
    // would resolve to (no marker comment) is refused, matching real
    // `distrobox export --delete`'s own identical safety check.
    std::fs::write(&dest, "[Desktop Entry]\nName=Not exported\n").unwrap();
    let refuse = ocibox(
        storage_dir.path(),
        &[
            "export",
            "--box",
            "testbox",
            "--app",
            "My App",
            "--export-path",
            export_dir.path().to_str().unwrap(),
            "--delete",
        ],
    );
    assert!(!refuse.status.success());
    assert!(
        String::from_utf8_lossy(&refuse.stderr).contains("not an ocibox-exported application"),
        "{}",
        String::from_utf8_lossy(&refuse.stderr)
    );
    assert!(dest.is_file(), "the foreign file must survive untouched");

    // Re-export for real, then delete it for real.
    let reexport = ocibox(
        storage_dir.path(),
        &[
            "export",
            "--box",
            "testbox",
            "--app",
            "My App",
            "--export-path",
            export_dir.path().to_str().unwrap(),
        ],
    );
    assert!(reexport.status.success());

    let delete = ocibox(
        storage_dir.path(),
        &[
            "export",
            "--box",
            "testbox",
            "--app",
            "My App",
            "--export-path",
            export_dir.path().to_str().unwrap(),
            "--delete",
        ],
    );
    assert!(
        delete.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&delete.stderr)
    );
    assert!(
        String::from_utf8_lossy(&delete.stdout).contains("removed successfully"),
        "{}",
        String::from_utf8_lossy(&delete.stdout)
    );
    assert!(!dest.exists(), "the launcher should really be gone now");
}

/// `export --app` of a name with no matching `.desktop` file is a
/// clear error, matching real `distrobox export --app`'s own
/// identical "cannot find any desktop files" rejection.
#[test]
fn export_app_of_an_unknown_app_is_a_clear_error() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    make_box(&storage_dir, "testbox");
    let export_dir = tempfile::tempdir().unwrap();

    let out = ocibox(
        storage_dir.path(),
        &[
            "export",
            "--box",
            "testbox",
            "--app",
            "NoSuchApp",
            "--export-path",
            export_dir.path().to_str().unwrap(),
        ],
    );
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("cannot find any desktop files"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `export` requires exactly one of `--app`/`--bin`, matching real
/// `distrobox export`'s own identical "choose only one action" rule.
#[test]
fn export_requires_exactly_one_of_app_or_bin() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    let export_dir = tempfile::tempdir().unwrap();

    let neither = ocibox(
        storage_dir.path(),
        &[
            "export",
            "--box",
            "testbox",
            "--export-path",
            export_dir.path().to_str().unwrap(),
        ],
    );
    assert!(!neither.status.success());
    assert!(
        String::from_utf8_lossy(&neither.stderr).contains("either --app or --bin is required"),
        "{}",
        String::from_utf8_lossy(&neither.stderr)
    );

    let both = ocibox(
        storage_dir.path(),
        &[
            "export",
            "--box",
            "testbox",
            "--app",
            "someapp",
            "--bin",
            "/bin/echo",
            "--export-path",
            export_dir.path().to_str().unwrap(),
        ],
    );
    assert!(!both.status.success());
    assert!(
        String::from_utf8_lossy(&both.stderr).contains("choose only one"),
        "{}",
        String::from_utf8_lossy(&both.stderr)
    );
}
