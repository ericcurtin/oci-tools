//! `ocibox export --bin`/`--app` integration tests (`docs/design/
//! 0252`, `0322`, `0327`): exercises the actual built `ocibox` binary
//! writing (and removing) a real wrapper script/`.desktop` launcher
//! (and, since `0327`, a real copied icon file) that routes an
//! exported binary/application's own invocations through `ocibox
//! enter` — checked directly against real `distrobox export`'s own
//! actual shell implementation (`~/git/distrobox/internal/inside-
//! distrobox/assets/distrobox-export`), including how the explicit
//! `--box` flag here diverges from real `distrobox export`'s own
//! "detect which box I'm running in" model.

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

/// Same as [`ocibox`], but with `$HOME` overridden to `home` — every
/// icon-export test below needs this (icon destinations are always
/// computed from `$HOME`, matching real distrobox exactly, regardless
/// of `--export-path`) so it never actually touches the real test
/// runner's own home directory.
fn ocibox_with_home(storage_root: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin_path("ocibox"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
        .env_remove("OCI_TOOLS_LOG")
        .env("HOME", home)
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
/// box's) — matching real `distrobox export --app`'s own core
/// `Exec=`-rewriting mechanism, checked directly against
/// `~/git/distrobox`'s own real shell implementation. This particular
/// fixture's own `Icon=myapp-icon` (a bare name with no matching real
/// icon file anywhere in the box's own rootfs) is left completely
/// untouched here for a genuine reason (see `0327`): nothing was
/// actually found to copy at all, not because icon handling itself is
/// unimplemented — `export_app_copies_a_bare_named_icon_to_the_real_
/// host_icon_directory` below covers the real, positive case.
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

/// Writes a real file into a box's own rootfs at `relative` (parent
/// directories created as needed) — the icon-export tests' own
/// equivalent of [`write_desktop_file`], for real icon files that
/// need to exist under specific search directories.
fn write_box_file(
    storage_dir: &tempfile::TempDir,
    box_name: &str,
    relative: &str,
    contents: &[u8],
) {
    let path = storage_dir
        .path()
        .join("boxes")
        .join(box_name)
        .join("rootfs")
        .join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

/// A bare `Icon=` name found under a themed `usr/share/icons/.../
/// apps/` subdirectory (0327) is copied to the exact same relative
/// path under `$HOME/.local/share/icons/...`, and the `.desktop`
/// file's own `Icon=` line is left completely untouched (a bare name
/// resolves via the icon theme's own normal lookup once the file
/// exists there) — matching real `distrobox-export`'s own identical
/// path-mapping rule, checked directly.
#[test]
fn export_app_copies_a_themed_icon_and_leaves_a_bare_icon_name_untouched() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    make_box(&storage_dir, "testbox");
    write_desktop_file(&storage_dir, "testbox", SAMPLE_DESKTOP_FILE);
    write_box_file(
        &storage_dir,
        "testbox",
        "usr/share/icons/hicolor/48x48/apps/myapp-icon.png",
        b"fake png bytes",
    );
    let export_dir = tempfile::tempdir().unwrap();
    let home_dir = tempfile::tempdir().unwrap();

    let export = ocibox_with_home(
        storage_dir.path(),
        home_dir.path(),
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

    let copied_icon = home_dir
        .path()
        .join(".local/share/icons/hicolor/48x48/apps/myapp-icon.png");
    assert!(
        copied_icon.is_file(),
        "icon should have been copied to {copied_icon:?}"
    );
    assert_eq!(std::fs::read(&copied_icon).unwrap(), b"fake png bytes");

    let contents =
        std::fs::read_to_string(export_dir.path().join("testbox-myapp.desktop")).unwrap();
    assert!(
        contents.contains("Icon=myapp-icon"),
        "a bare icon name must stay untouched: {contents:?}"
    );
}

/// A bare `Icon=` name found under `usr/share/pixmaps/` (the other
/// real, checked-directly canonical search directory) is copied to
/// `$HOME/.local/share/icons/...` — not `.../pixmaps/...` — matching
/// real distrobox's own identical `pixmaps`->`icons` rename (`.local/
/// share/pixmaps` isn't a real XDG icon-theme search location at all).
#[test]
fn export_app_copies_a_pixmaps_icon_renaming_the_directory_to_icons() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    make_box(&storage_dir, "testbox");
    write_desktop_file(&storage_dir, "testbox", SAMPLE_DESKTOP_FILE);
    write_box_file(
        &storage_dir,
        "testbox",
        "usr/share/pixmaps/myapp-icon.xpm",
        b"fake xpm bytes",
    );
    let export_dir = tempfile::tempdir().unwrap();
    let home_dir = tempfile::tempdir().unwrap();

    let export = ocibox_with_home(
        storage_dir.path(),
        home_dir.path(),
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

    let copied_icon = home_dir.path().join(".local/share/icons/myapp-icon.xpm");
    assert!(
        copied_icon.is_file(),
        "icon should have been copied (with pixmaps renamed to icons) to {copied_icon:?}"
    );
    assert!(
        !home_dir.path().join(".local/share/pixmaps").exists(),
        "a `.local/share/pixmaps` directory should never be created at all"
    );
}

/// An `Icon=` value that's already an absolute path pointing *outside*
/// any canonical icon directory (a real, if rare, vendor-specific
/// location) falls back to a flat `$HOME/.local/share/icons/
/// <basename>` destination, and — unlike the bare-name cases above —
/// the `.desktop` file's own `Icon=` line genuinely must be rewritten
/// to that new absolute host path, since the original path only ever
/// existed inside the box's own rootfs, never on the real host.
#[test]
fn export_app_rewrites_icon_for_a_non_canonical_absolute_path() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    make_box(&storage_dir, "testbox");
    write_desktop_file(
        &storage_dir,
        "testbox",
        "[Desktop Entry]\nType=Application\nName=My App\n\
         Exec=/usr/bin/myapp --flag\nIcon=/opt/myapp/icon.png\n",
    );
    write_box_file(
        &storage_dir,
        "testbox",
        "opt/myapp/icon.png",
        b"vendor icon",
    );
    let export_dir = tempfile::tempdir().unwrap();
    let home_dir = tempfile::tempdir().unwrap();

    let export = ocibox_with_home(
        storage_dir.path(),
        home_dir.path(),
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

    let copied_icon = home_dir.path().join(".local/share/icons/icon.png");
    assert!(copied_icon.is_file(), "expected {copied_icon:?} to exist");
    assert_eq!(std::fs::read(&copied_icon).unwrap(), b"vendor icon");

    let contents =
        std::fs::read_to_string(export_dir.path().join("testbox-myapp.desktop")).unwrap();
    let expected_icon_line = format!("Icon={}", copied_icon.display());
    assert!(
        contents.contains(&expected_icon_line),
        "Icon= must be rewritten to the new absolute host path: {contents:?}"
    );
    assert!(
        !contents.contains("Icon=/opt/myapp/icon.png"),
        "the original, host-unreachable path must not survive: {contents:?}"
    );
}

/// An `Icon=` value that's already an absolute path under the
/// canonical `/usr/share/` prefix gets that prefix rewritten to
/// `$HOME/.local/share/` in the exported `.desktop` file itself,
/// matching real distrobox's own identical (if narrow — see
/// `rewrite_desktop_file`'s own doc comment) `sed` rule.
#[test]
fn export_app_rewrites_icon_for_a_canonical_absolute_usr_share_path() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    make_box(&storage_dir, "testbox");
    write_desktop_file(
        &storage_dir,
        "testbox",
        "[Desktop Entry]\nType=Application\nName=My App\n\
         Exec=/usr/bin/myapp --flag\nIcon=/usr/share/pixmaps/myapp.png\n",
    );
    write_box_file(
        &storage_dir,
        "testbox",
        "usr/share/pixmaps/myapp.png",
        b"canonical hard path icon",
    );
    let export_dir = tempfile::tempdir().unwrap();
    let home_dir = tempfile::tempdir().unwrap();

    let export = ocibox_with_home(
        storage_dir.path(),
        home_dir.path(),
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

    let copied_icon = home_dir.path().join(".local/share/icons/myapp.png");
    assert!(copied_icon.is_file(), "expected {copied_icon:?} to exist");

    let contents =
        std::fs::read_to_string(export_dir.path().join("testbox-myapp.desktop")).unwrap();
    let expected_icon_line = format!(
        "Icon={}/.local/share/icons/myapp.png",
        home_dir.path().display()
    );
    assert!(
        contents.contains(&expected_icon_line),
        "Icon=/usr/share/... must be rewritten to $HOME/.local/share/...: {contents:?}"
    );
}

/// `export --app --delete` also removes the real, previously-copied
/// icon file, tolerant of it already being gone -- matching real
/// distrobox's own identical unconditional-but-tolerant icon removal.
#[test]
fn export_app_delete_also_removes_the_copied_icon() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    make_box(&storage_dir, "testbox");
    write_desktop_file(&storage_dir, "testbox", SAMPLE_DESKTOP_FILE);
    write_box_file(
        &storage_dir,
        "testbox",
        "usr/share/icons/hicolor/48x48/apps/myapp-icon.png",
        b"fake png bytes",
    );
    let export_dir = tempfile::tempdir().unwrap();
    let home_dir = tempfile::tempdir().unwrap();

    let export = ocibox_with_home(
        storage_dir.path(),
        home_dir.path(),
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
    let copied_icon = home_dir
        .path()
        .join(".local/share/icons/hicolor/48x48/apps/myapp-icon.png");
    assert!(copied_icon.is_file());

    let delete = ocibox_with_home(
        storage_dir.path(),
        home_dir.path(),
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
        !copied_icon.exists(),
        "the copied icon should really be gone now"
    );
}

/// `--export-label` (0328) not given at all defaults to appending
/// `" (on <box_name>)"` to the exported `.desktop` file's own `Name=`
/// line -- matching real `distrobox export`'s own identical, checked-
/// directly default exactly.
#[test]
fn export_app_default_label_appends_on_box_name() {
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

    let contents =
        std::fs::read_to_string(export_dir.path().join("testbox-myapp.desktop")).unwrap();
    assert!(
        contents.lines().any(|l| l == "Name=My App (on testbox)"),
        "{contents:?}"
    );
}

/// `--export-label none` disables the label entirely -- `Name=` stays
/// exactly as it already was, no default `(on <box_name>)` appended.
#[test]
fn export_app_export_label_none_disables_the_label() {
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
            "--export-label",
            "none",
        ],
    );
    assert!(
        export.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&export.stderr)
    );

    let contents =
        std::fs::read_to_string(export_dir.path().join("testbox-myapp.desktop")).unwrap();
    assert!(contents.lines().any(|l| l == "Name=My App"), "{contents:?}");
}

/// A real, explicit `--export-label` value is appended verbatim (with
/// a leading space), overriding the default `(on <box_name>)` label
/// entirely.
#[test]
fn export_app_custom_export_label_is_appended_verbatim() {
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
            "--export-label",
            "[work]",
        ],
    );
    assert!(
        export.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&export.stderr)
    );

    let contents =
        std::fs::read_to_string(export_dir.path().join("testbox-myapp.desktop")).unwrap();
    assert!(
        contents.lines().any(|l| l == "Name=My App [work]"),
        "{contents:?}"
    );
}

/// The label is only ever appended to a line that genuinely *starts*
/// with `Name` (covering both the bare `Name=` key and a localized
/// `Name[xx]=` one) -- a deliberate, documented narrowing of real
/// distrobox's own cruder, unanchored `sed "s|Name.*|&${label}|g"`,
/// which would also (mis)match a `GenericName=`/`Comment=` line merely
/// *containing* the substring "Name" anywhere in its own value.
#[test]
fn export_app_label_only_touches_lines_starting_with_name() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    make_box(&storage_dir, "testbox");
    write_desktop_file(
        &storage_dir,
        "testbox",
        "[Desktop Entry]\nType=Application\nName=My App\nGenericName=Editor\n\
         Comment=Has a Name mentioned here\nExec=/usr/bin/myapp --flag\n",
    );
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

    let contents =
        std::fs::read_to_string(export_dir.path().join("testbox-myapp.desktop")).unwrap();
    assert!(
        contents.lines().any(|l| l == "Name=My App (on testbox)"),
        "{contents:?}"
    );
    assert!(
        contents.lines().any(|l| l == "GenericName=Editor"),
        "GenericName= must be left untouched: {contents:?}"
    );
    assert!(
        contents
            .lines()
            .any(|l| l == "Comment=Has a Name mentioned here"),
        "a Comment= merely containing the substring \"Name\" must be left untouched: {contents:?}"
    );
}

/// `--list-apps` (0329) shows every real, previously-exported
/// application from `--box`, with its own default label stripped back
/// off for a clean display -- matching real `distrobox export
/// --list-apps`'s own identical cosmetic convention.
#[test]
fn export_list_apps_shows_a_previously_exported_app_with_a_clean_name() {
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

    let list = ocibox(
        storage_dir.path(),
        &[
            "export",
            "--box",
            "testbox",
            "--list-apps",
            "--export-path",
            export_dir.path().to_str().unwrap(),
        ],
    );
    assert!(
        list.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&list.stderr)
    );
    let stdout = String::from_utf8_lossy(&list.stdout).into_owned();
    let dest = export_dir.path().join("testbox-myapp.desktop");
    assert!(
        stdout.contains("My App") && stdout.contains(dest.to_str().unwrap()),
        "{stdout:?}"
    );
    assert!(
        !stdout.contains("(on testbox)"),
        "the default label should be stripped back off for display: {stdout:?}"
    );
}

/// `--list-apps` on a box with nothing exported at all is a real,
/// silent, empty success -- not an error -- matching real distrobox's
/// own identical tolerance for a search directory with no matches (or
/// not existing at all yet).
#[test]
fn export_list_apps_on_a_box_with_nothing_exported_is_empty() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    let export_dir = tempfile::tempdir().unwrap();

    let list = ocibox(
        storage_dir.path(),
        &[
            "export",
            "--box",
            "testbox",
            "--list-apps",
            "--export-path",
            export_dir.path().to_str().unwrap(),
        ],
    );
    assert!(
        list.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&list.stderr)
    );
    assert!(list.stdout.is_empty());
}

/// `--list-binaries` (0329) shows every real, previously-exported
/// binary from `--box`, named after the exported file's own basename
/// (this project's own established `--bin` destination-naming
/// convention already makes that exactly the original binary's own
/// name).
#[test]
fn export_list_binaries_shows_a_previously_exported_binary() {
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

    let list = ocibox(
        storage_dir.path(),
        &[
            "export",
            "--box",
            "testbox",
            "--list-binaries",
            "--export-path",
            export_dir.path().to_str().unwrap(),
        ],
    );
    assert!(
        list.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&list.stderr)
    );
    let stdout = String::from_utf8_lossy(&list.stdout).into_owned();
    let dest = export_dir.path().join("echo");
    assert!(
        stdout.contains("echo") && stdout.contains(dest.to_str().unwrap()),
        "{stdout:?}"
    );
}

/// `--list-binaries`/`--list-apps` only ever shows exports belonging
/// to the given `--box`, never a different one's -- even when both
/// share the exact same `--export-path`.
#[test]
fn export_list_binaries_only_shows_exports_from_the_given_box() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    make_box(&storage_dir, "boxone");
    make_box(&storage_dir, "boxtwo");
    let export_dir = tempfile::tempdir().unwrap();

    // Different basenames (`echo`/`sh`) so the two boxes' own exports
    // don't collide at the same destination filename (`--bin` exports
    // are never box-name-prefixed, matching real distrobox exactly).
    for (box_name, bin) in [("boxone", "/bin/echo"), ("boxtwo", "/bin/sh")] {
        let export = ocibox(
            storage_dir.path(),
            &[
                "export",
                "--box",
                box_name,
                "--bin",
                bin,
                "--export-path",
                export_dir.path().to_str().unwrap(),
            ],
        );
        assert!(export.status.success());
    }

    let list = ocibox(
        storage_dir.path(),
        &[
            "export",
            "--box",
            "boxone",
            "--list-binaries",
            "--export-path",
            export_dir.path().to_str().unwrap(),
        ],
    );
    assert!(list.status.success());
    let stdout = String::from_utf8_lossy(&list.stdout).into_owned();
    assert!(stdout.contains("echo"), "{stdout:?}");
    assert_eq!(
        stdout.lines().count(),
        1,
        "only boxone's own single exported binary should be listed, not boxtwo's too: {stdout:?}"
    );
}

/// `--list-apps`/`--list-binaries` cannot be combined with each other,
/// or with `--app`/`--bin`/`--delete`/`--export-label`.
#[test]
fn export_list_apps_and_list_binaries_are_mutually_exclusive_with_other_actions() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let both_lists = ocibox(
        storage_dir.path(),
        &[
            "export",
            "--box",
            "testbox",
            "--list-apps",
            "--list-binaries",
        ],
    );
    assert!(!both_lists.status.success());
    assert!(
        String::from_utf8_lossy(&both_lists.stderr).contains("choose only one"),
        "{}",
        String::from_utf8_lossy(&both_lists.stderr)
    );

    let list_and_app = ocibox(
        storage_dir.path(),
        &["export", "--box", "testbox", "--list-apps", "--app", "foo"],
    );
    assert!(!list_and_app.status.success());
    assert!(
        String::from_utf8_lossy(&list_and_app.stderr).contains("cannot be combined"),
        "{}",
        String::from_utf8_lossy(&list_and_app.stderr)
    );
}

/// `export --bin --extra-flags` (0330) inserts the given flags right
/// after the exported binary's own path in the generated wrapper
/// script, before its own forwarded `"$@"` -- matching real
/// `distrobox export --bin --extra-flags` exactly.
#[test]
fn export_bin_extra_flags_are_inserted_before_the_forwarded_args() {
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
            "--extra-flags",
            "-n",
        ],
    );
    assert!(
        export.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&export.stderr)
    );

    let contents = std::fs::read_to_string(export_dir.path().join("echo")).unwrap();
    assert!(
        contents.contains("exec ocibox enter testbox -- '/bin/echo' -n \"$@\""),
        "{contents:?}"
    );
}

/// `export --app --extra-flags` (0330) is inserted right before a
/// real desktop-entry field code (`%f`/`%u`/...) in the `Exec=` line
/// -- matching real `distrobox export --app --extra-flags`'s own
/// identical, narrower `sed`-based rule exactly.
#[test]
fn export_app_extra_flags_are_inserted_before_a_field_code() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    make_box(&storage_dir, "testbox");
    write_desktop_file(
        &storage_dir,
        "testbox",
        "[Desktop Entry]\nType=Application\nName=My App\nExec=/usr/bin/myapp %f\n",
    );
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
            "--extra-flags",
            "--no-remote",
        ],
    );
    assert!(
        export.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&export.stderr)
    );

    let contents =
        std::fs::read_to_string(export_dir.path().join("testbox-myapp.desktop")).unwrap();
    assert!(
        contents
            .lines()
            .any(|l| l == "Exec=ocibox enter testbox -- /usr/bin/myapp --no-remote %f"),
        "{contents:?}"
    );
}

/// `export --app --extra-flags` has no effect at all when the
/// `Exec=` line has no field code to insert before -- a real, crude
/// limitation of real distrobox's own `sed`-based implementation this
/// project deliberately matches rather than "fixes" into always
/// appending unconditionally.
#[test]
fn export_app_extra_flags_have_no_effect_without_a_field_code() {
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
            "--extra-flags",
            "--no-remote",
        ],
    );
    assert!(
        export.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&export.stderr)
    );

    let contents =
        std::fs::read_to_string(export_dir.path().join("testbox-myapp.desktop")).unwrap();
    assert!(
        contents
            .lines()
            .any(|l| l == "Exec=ocibox enter testbox -- /usr/bin/myapp --flag"),
        "no field code in Exec= means --extra-flags has no effect: {contents:?}"
    );
    assert!(!contents.contains("--no-remote"), "{contents:?}");
}
