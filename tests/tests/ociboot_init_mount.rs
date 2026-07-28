//! `ociboot-init mount` integration tests (`docs/design/0246`): builds
//! a real deployment erofs via the actual `ociboot build-image` binary
//! (the same seeded-store fixture `ociboot_build_image.rs` uses), then
//! drives the initramfs helper's own real mount operation — the
//! unprivileged validation/verity failures unconditionally, and the
//! real loop-attach + erofs mount under passwordless `sudo` when this
//! host offers it (the same opportunistic-privilege pattern the
//! workspace's loop-device/fs-verity tests already use).

use std::path::Path;
use std::process::Command;

use oci_spec_types::image::ContainerConfig;
use oci_store::Store;
use oci_tools_tests::{bin_path, busybox_path, seed_image};
// (seed_image_with_files referenced via the crate path where used.)

fn ociboot(storage_root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin_path("ociboot"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
        .env_remove("OCI_TOOLS_LOG")
        .args(args)
        .output()
        .expect("failed to spawn ociboot")
}

fn ociboot_init(args: &[&str]) -> std::process::Output {
    Command::new(bin_path("ociboot-init"))
        .args(args)
        .output()
        .expect("failed to spawn ociboot-init")
}

fn mkfs_erofs_available() -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| dir.join("mkfs.erofs").is_file())
    })
}

/// Passwordless sudo, the workspace's established opportunistic gate
/// for the few tests that need real privileges.
fn sudo_available() -> bool {
    Command::new("sudo")
        .args(["-n", "true"])
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Builds a real deployment image into `dir`, returning its path.
/// `seal`: also fs-verity-seal it (`ociboot build-image --seal`),
/// returning the printed digest — `None` if sealing isn't supported
/// there (the caller decides whether that's a skip).
fn build_deployment(dir: &Path, seal: bool) -> (std::path::PathBuf, Option<String>) {
    let busybox = busybox_path().expect("caller checked busybox");
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociboot-test/init-mount:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let output_path = dir.join("deployment.erofs");
    let mut args = vec![
        "build-image",
        "ociboot-test/init-mount:latest",
        "--output",
        output_path.to_str().unwrap(),
    ];
    if seal {
        args.push("--seal");
    }
    let build = ociboot(storage_dir.path(), &args);
    if !build.status.success() {
        panic!(
            "build-image failed: {}",
            String::from_utf8_lossy(&build.stderr)
        );
    }
    // The fs-verity digest, when sealing was requested and actually
    // used fs-verity (a dm-verity-fallback line means the filesystem
    // didn't support it — treated as "not fs-verity sealed" here).
    let digest = seal.then(|| {
        let stdout = String::from_utf8_lossy(&build.stdout);
        stdout
            .lines()
            .find_map(|l| l.strip_prefix("fs-verity digest: ").map(str::to_string))
    });
    (output_path, digest.flatten())
}

/// The unprivileged failure surface: karg validation, a missing
/// image, and a verity expectation an unsealed image can't meet —
/// all before anything privileged would ever run.
#[test]
fn mount_validates_the_kernel_command_line_before_touching_anything() {
    if !mkfs_erofs_available() {
        eprintln!("skipping: mkfs.erofs not installed");
        return;
    }
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (image, _) = build_deployment(dir.path(), false);
    let image_dir = image.parent().unwrap().to_str().unwrap().to_string();
    let target = dir.path().join("sysroot");
    let write_cmdline = |contents: &str| {
        let path = dir.path().join("cmdline");
        std::fs::write(&path, contents).unwrap();
        path.to_str().unwrap().to_string()
    };

    // No ociboot.image= at all.
    let cmdline = write_cmdline("quiet root=/dev/vda2");
    let out = ociboot_init(&[
        "mount",
        "--cmdline",
        &cmdline,
        "--image-dir",
        &image_dir,
        "--target",
        target.to_str().unwrap(),
    ]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("ociboot.image= not present"),
        "{out:?}"
    );

    // A path-shaped image name must be rejected (traversal guard).
    let cmdline = write_cmdline("ociboot.image=../../etc/shadow");
    let out = ociboot_init(&[
        "mount",
        "--cmdline",
        &cmdline,
        "--image-dir",
        &image_dir,
        "--target",
        target.to_str().unwrap(),
    ]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("plain file name"),
        "{out:?}"
    );

    // A verity expectation against an unsealed image is a hard error.
    let cmdline = write_cmdline(&format!(
        "ociboot.image=deployment.erofs ociboot.verity={}",
        "ab".repeat(32)
    ));
    let out = ociboot_init(&[
        "mount",
        "--cmdline",
        &cmdline,
        "--image-dir",
        &image_dir,
        "--target",
        target.to_str().unwrap(),
    ]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not fs-verity sealed") || stderr.contains("measuring"),
        "{stderr:?}"
    );

    // A missing deployment image is a clear error naming the path.
    let cmdline = write_cmdline("ociboot.image=nope.erofs");
    let out = ociboot_init(&[
        "mount",
        "--cmdline",
        &cmdline,
        "--image-dir",
        &image_dir,
        "--target",
        target.to_str().unwrap(),
    ]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("does not exist"),
        "{out:?}"
    );

    // Usage errors exit 2 with help, distinct from boot failures.
    let out = ociboot_init(&["mount", "--target", target.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
}

/// The real, privileged happy path: loop-attach + erofs mount, the
/// mounted tree readable, then unmounted and the loop device
/// released. Skips cleanly without passwordless sudo (or a kernel
/// without erofs).
#[test]
fn mount_loop_mounts_a_real_deployment_read_only() {
    if !mkfs_erofs_available() {
        eprintln!("skipping: mkfs.erofs not installed");
        return;
    }
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    if !sudo_available() {
        eprintln!("skipping: no passwordless sudo");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (image, _) = build_deployment(dir.path(), false);
    let image_dir = image.parent().unwrap().to_str().unwrap().to_string();
    let target = dir.path().join("sysroot");
    let cmdline_path = dir.path().join("cmdline");
    std::fs::write(&cmdline_path, "quiet ociboot.image=deployment.erofs rw").unwrap();

    let out = Command::new("sudo")
        .args([
            "-n",
            bin_path("ociboot-init").to_str().unwrap(),
            "mount",
            "--cmdline",
            cmdline_path.to_str().unwrap(),
            "--image-dir",
            &image_dir,
            "--target",
            target.to_str().unwrap(),
        ])
        .output()
        .expect("failed to spawn sudo ociboot-init");
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() && stderr.contains("mounting") {
        // A kernel without erofs support is a real environment
        // limitation, not a bug in this code.
        eprintln!("skipping: erofs mount unsupported here: {stderr}");
        return;
    }
    assert!(out.status.success(), "{stderr}");
    // The unverified-mount warning is real and expected (no
    // ociboot.verity= on this cmdline).
    assert!(stderr.contains("mounting"), "warning expected: {stderr}");

    // The mounted tree is the deployment's own rootfs.
    assert!(
        target.join("bin/busybox").is_file(),
        "the mounted erofs should contain the seeded rootfs"
    );
    // Read-only for real: a write attempt fails.
    assert!(
        std::fs::write(target.join("scribble"), b"x").is_err(),
        "an erofs mount must reject writes"
    );

    // Cleanup: unmount; the loop device (attached without autoclear)
    // is found by backing file and detached -- test-side `losetup`
    // use is the same pattern the loop-device unit tests employ.
    let umount = Command::new("sudo")
        .args(["-n", "umount", target.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(umount.status.success(), "{umount:?}");
    let list = Command::new("losetup")
        .args(["-j", image.to_str().unwrap()])
        .output()
        .unwrap();
    let listing = String::from_utf8_lossy(&list.stdout);
    if let Some(device) = listing.split(':').next().filter(|s| !s.is_empty()) {
        let detach = Command::new("sudo")
            .args(["-n", "losetup", "-d", device])
            .output()
            .unwrap();
        assert!(detach.status.success(), "{detach:?}");
    }
}

/// The writable view (`docs/design/0250`): with `--state-dir`, `/etc`
/// becomes a real overlay whose upper half lives on the state
/// directory (edits persist there, host-visible), `/var` binds the
/// state directory's own `var`, `/run`+`/tmp` are fresh tmpfs — all
/// on top of the still-sealed read-only image. An image missing the
/// required directories is a clear error, and the failure path tears
/// the whole target down again (no mount, no loop device left).
#[test]
fn mount_with_state_dir_assembles_the_writable_view() {
    if !mkfs_erofs_available() {
        eprintln!("skipping: mkfs.erofs not installed");
        return;
    }
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    if !sudo_available() {
        eprintln!("skipping: no passwordless sudo");
        return;
    }

    // An OS-shaped image: /etc with real content, /var, /run, /tmp.
    let dir = tempfile::tempdir().unwrap();
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    oci_tools_tests::seed_image_with_files(
        &store,
        "ociboot-test/os-shaped:latest",
        &busybox,
        &["sh"],
        &[
            ("etc/os-release", b"NAME=oci-tools-test\n"),
            ("var/.keep", b""),
            ("run/.keep", b""),
            ("tmp/.keep", b""),
        ],
        ContainerConfig::default(),
    );
    let image = dir.path().join("deployment.erofs");
    let build = ociboot(
        storage_dir.path(),
        &[
            "build-image",
            "ociboot-test/os-shaped:latest",
            "--output",
            image.to_str().unwrap(),
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let target = dir.path().join("sysroot");
    let state = dir.path().join("state");
    let cmdline_path = dir.path().join("cmdline");
    std::fs::write(&cmdline_path, "ociboot.image=deployment.erofs").unwrap();

    let out = Command::new("sudo")
        .args([
            "-n",
            bin_path("ociboot-init").to_str().unwrap(),
            "mount",
            "--cmdline",
            cmdline_path.to_str().unwrap(),
            "--image-dir",
            dir.path().to_str().unwrap(),
            "--target",
            target.to_str().unwrap(),
            "--state-dir",
            state.to_str().unwrap(),
        ])
        .output()
        .expect("failed to spawn sudo ociboot-init");
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() && (stderr.contains("mounting") || stderr.contains("overlay")) {
        eprintln!("skipping: erofs/overlay mount unsupported here: {stderr}");
        return;
    }
    assert!(out.status.success(), "{stderr}");

    // The overlay's lower half: the image's own /etc content reads
    // back through the mounted view.
    assert_eq!(
        std::fs::read_to_string(target.join("etc/os-release")).unwrap(),
        "NAME=oci-tools-test\n"
    );
    // An /etc write lands in the state directory's upper half --
    // host-visible, which is exactly how it persists across boots.
    let write = Command::new("sudo")
        .args(["-n", "sh", "-c"])
        .arg(format!(
            "echo persisted > {}/etc/new.conf && echo vardata > {}/var/data.txt && \
             echo scratch > {}/tmp/scratch",
            target.display(),
            target.display(),
            target.display()
        ))
        .output()
        .unwrap();
    assert!(write.status.success(), "{write:?}");
    assert_eq!(
        std::fs::read_to_string(state.join("etc/upper/new.conf")).unwrap(),
        "persisted\n",
        "/etc writes must land on the state directory"
    );
    assert_eq!(
        std::fs::read_to_string(state.join("var/data.txt")).unwrap(),
        "vardata\n",
        "/var writes must land on the state directory"
    );
    assert!(
        !state.join("tmp").exists() && !dir.path().join("tmp/scratch").exists(),
        "/tmp is a fresh tmpfs, never persisted anywhere"
    );
    // The base image itself is still genuinely read-only.
    let scribble = Command::new("sudo")
        .args(["-n", "sh", "-c"])
        .arg(format!("echo x > {}/scribble", target.display()))
        .output()
        .unwrap();
    assert!(
        !scribble.status.success(),
        "the erofs base must reject writes even as root"
    );

    // Cleanup: recursive unmount takes the overlay/bind/tmpfs and the
    // base in one sweep, then release the loop device.
    let umount = Command::new("sudo")
        .args(["-n", "umount", "-R", target.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(umount.status.success(), "{umount:?}");
    let list = Command::new("losetup")
        .args(["-j", image.to_str().unwrap()])
        .output()
        .unwrap();
    let listing = String::from_utf8_lossy(&list.stdout);
    if let Some(device) = listing.split(':').next().filter(|s| !s.is_empty()) {
        let detach = Command::new("sudo")
            .args(["-n", "losetup", "-d", device])
            .output()
            .unwrap();
        assert!(detach.status.success(), "{detach:?}");
    }

    // The failure path: a minimal image (no /etc at all) with
    // --state-dir errors clearly and leaves nothing behind.
    let (minimal, _) = build_deployment(dir.path(), false);
    std::fs::rename(&minimal, dir.path().join("minimal.erofs")).unwrap();
    std::fs::write(&cmdline_path, "ociboot.image=minimal.erofs").unwrap();
    let out = Command::new("sudo")
        .args([
            "-n",
            bin_path("ociboot-init").to_str().unwrap(),
            "mount",
            "--cmdline",
            cmdline_path.to_str().unwrap(),
            "--image-dir",
            dir.path().to_str().unwrap(),
            "--target",
            target.to_str().unwrap(),
            "--state-dir",
            state.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("has no /etc directory"),
        "{out:?}"
    );
    // Nothing left mounted, no loop device leaked.
    let mounts = std::fs::read_to_string("/proc/mounts").unwrap();
    assert!(
        !mounts.contains(target.to_str().unwrap()),
        "the failure path must unmount the target again"
    );
    // `ociboot-init`'s own teardown already waits out the real,
    // environment-dependent delay `LOOP_CLR_FD` can have before the
    // device is genuinely gone (see `oci_mount::loop_device::detach`'s
    // own doc comment: another opener -- `systemd-udevd` transiently
    // probing a just-changed device, observed directly -- can defer
    // the actual clear past the ioctl call that scheduled it), so by
    // the time the subprocess above has exited this should already be
    // true; poll briefly anyway rather than assuming a zero-delay
    // `losetup -j` observes the exact same instant the kernel does.
    let minimal_path = dir.path().join("minimal.erofs");
    let released = (0..40).any(|i| {
        if i > 0 {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let list = Command::new("losetup")
            .args(["-j", minimal_path.to_str().unwrap()])
            .output()
            .unwrap();
        list.stdout.is_empty()
    });
    assert!(
        released,
        "the failure path must release the loop device (still listed after waiting)"
    );
}
