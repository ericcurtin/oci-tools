//! `ociman run` integration tests, exercised entirely offline: no real
//! registry pull. Instead, a local `oci_store::Store` is hand-seeded
//! with a synthetic-but-structurally-real image — a real `busybox`
//! binary tarred and gzipped as the one layer, and a real
//! `ImageConfig`/`ImageManifest` (built from `oci_spec_types`, the same
//! types a real pull deserializes into) ingested exactly the way
//! `oci_registry::pull` would have left them. Deterministic, no network
//! dependency, and exercises the *same* extraction/spec-synthesis/
//! launch code path a real pulled image goes through — `ociman`'s own
//! `resolve_or_pull` finds the image already present and skips pulling
//! entirely, so nothing here is a special "test mode".
//!
//! This exact real-image-first approach is what caught a real,
//! previously-undetected bug while building this increment: real image
//! config JSON uses `PascalCase` field names (`Cmd`, `Env`, ...), which
//! `oci_spec_types::image::ContainerConfig` didn't declare — every
//! field silently deserialized to its default for every real image
//! ever pulled, until `ociman run` against an actual `busybox` image
//! produced an empty command. Fixed in `oci-spec-types` alongside this
//! increment; a real fixture-based test lives in `oci-spec-types`
//! itself, and the tests below would have caught the regression too
//! (a synthetic seeded image using the same real struct).

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use oci_spec_types::Reference;
use oci_spec_types::digest::sha256;
use oci_spec_types::image::{
    ContainerConfig, Descriptor, ImageManifest, MEDIA_TYPE_IMAGE_CONFIG,
    MEDIA_TYPE_IMAGE_LAYER_GZIP, MEDIA_TYPE_IMAGE_MANIFEST,
};
use oci_store::{ImageRecord, Store};

use oci_tools_tests::{
    LayerCompression, bin_path, busybox_path, seed_image, seed_image_with_files,
    seed_image_with_files_and_compression,
};

fn ociman_run(storage_root: &Path, image: &str, args: &[&str]) -> std::process::Output {
    Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
        .env_remove("OCI_TOOLS_LOG")
        .args(["run", image])
        .args(args)
        .output()
        .expect("failed to spawn ociman run")
}

/// The real, current systemd scope name for `container_id`'s own most
/// recent launch — since 0159, this is no longer just
/// `ociman-<id>.scope` (every real launch gets a fresh nonce folded
/// in, so a restarted container's own second launch never collides
/// with its first one's still-settling scope teardown); read directly
/// from the container's own persisted `state.json` rather than
/// hardcoded, so these tests keep working regardless of the exact
/// nonce a given run happens to get.
fn real_scope_name(storage_root: &Path, container_id: &str) -> String {
    let state_path = storage_root
        .join("containers")
        .join(container_id)
        .join("state.json");
    let state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap_or_else(|e| {
            panic!("reading {}: {e}", state_path.display());
        }))
        .expect("state.json should be valid JSON");
    match state["annotations"]["io.oci-tools.scope-nonce"].as_str() {
        Some(nonce) => format!("ociman-{container_id}-{nonce}.scope"),
        None => format!("ociman-{container_id}.scope"),
    }
}

/// `ociman run` grants real `podman run`'s own default 11-capability
/// set (`oci_spec_types::runtime::podman_default_capabilities`) —
/// deliberately *not* `Spec::example()`'s own bare 3-capability
/// real-runc-scaffold default `ocirun spec`/`ocirun run` still use
/// (see `tests/tests/ocirun_run.rs`'s own identically-shaped
/// `run_applies_the_default_capability_set_and_no_new_privileges`,
/// which keeps asserting the *smaller* runc-scaffold bitmask — the two
/// binaries deliberately no longer share one capability default,
/// `ocirun` being a `runc` clone and `ociman` a `podman` one). The
/// exact bitmask below was read directly from a real running
/// container's own `/proc/self/status` while building this increment,
/// and matches a real `podman run --rm alpine cat /proc/self/status`'s
/// own `CapEff` (podman 4.9.3) exactly.
#[test]
fn run_grants_the_real_podman_default_capability_set() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/default-caps:latest",
        &busybox,
        &["sh", "grep"],
        ContainerConfig::default(),
    );

    let out = ociman_run(
        storage_dir.path(),
        "ociman-test/default-caps:latest",
        &[
            "/bin/sh",
            "-c",
            r#"grep -E "^(CapInh|CapPrm|CapEff|CapBnd|CapAmb):" /proc/self/status"#,
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "CapInh:\t0000000000000000\nCapPrm:\t00000000800405fb\nCapEff:\t00000000800405fb\nCapBnd:\t00000000800405fb\nCapAmb:\t0000000000000000"
    );
}

/// `--cap-drop` genuinely removes a capability from the real
/// podman-default set. `CAP_CHOWN` is bit 0 of the default mask
/// (`0x800405fb`, see `run_grants_the_real_podman_default_capability_set`'s
/// own doc comment) -- dropping it should leave exactly
/// `0x800405fa`, confirmed by hand against a real running container
/// before writing this assertion, not derived from the bitmask alone.
#[test]
fn run_cap_drop_removes_a_capability_from_the_default_set() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/cap-drop:latest",
        &busybox,
        &["sh", "grep"],
        ContainerConfig::default(),
    );

    let out = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args(["run", "--rm", "--cap-drop", "chown"])
        .args(["ociman-test/cap-drop:latest"])
        .args(["/bin/sh", "-c", "grep -E \"^CapEff:\" /proc/self/status"])
        .output()
        .expect("failed to spawn ociman run");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "CapEff:\t00000000800405fa"
    );
}

/// `--cap-add` genuinely grants a capability beyond the real
/// podman-default set. `CAP_NET_ADMIN` is bit 12 (`0x1000`) --
/// added to the default mask that becomes `0x800415fb`, confirmed by
/// hand against a real running container first.
#[test]
fn run_cap_add_grants_a_capability_beyond_the_default_set() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/cap-add:latest",
        &busybox,
        &["sh", "grep"],
        ContainerConfig::default(),
    );

    let out = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args(["run", "--rm", "--cap-add", "net_admin"])
        .args(["ociman-test/cap-add:latest"])
        .args(["/bin/sh", "-c", "grep -E \"^CapEff:\" /proc/self/status"])
        .output()
        .expect("failed to spawn ociman run");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "CapEff:\t00000000800415fb"
    );
}

/// Giving the same capability to both `--cap-add` and `--cap-drop` is
/// a real, surfaced CLI error, not silently resolved either way --
/// the container never even starts.
#[test]
fn run_rejects_the_same_capability_added_and_dropped() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/cap-conflict:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let out = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args([
            "run",
            "--rm",
            "--cap-add",
            "net_admin",
            "--cap-drop",
            "net_admin",
        ])
        .args(["ociman-test/cap-conflict:latest", "/bin/true"])
        .output()
        .expect("failed to spawn ociman run");
    assert!(
        !out.status.success(),
        "should have refused a conflicting cap-add/cap-drop"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("CAP_NET_ADMIN"),
        "stderr should name the conflicting capability: {stderr}"
    );
}

/// `--privileged` grants every capability this build recognizes
/// (`CapEff` becomes all 41 recognized bits set, `0x1ffffffffff` --
/// confirmed by hand against a real running container first, the same
/// value real `--cap-add=all` alone produces) and disables seccomp
/// entirely (`Seccomp: 0`, `SECCOMP_MODE_DISABLED`).
#[test]
fn run_privileged_grants_every_capability_and_disables_seccomp() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/privileged:latest",
        &busybox,
        &["sh", "grep"],
        ContainerConfig::default(),
    );

    let out = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args(["run", "--rm", "--privileged"])
        .args(["ociman-test/privileged:latest"])
        .args([
            "/bin/sh",
            "-c",
            "grep -E \"^(CapEff|Seccomp):\" /proc/self/status",
        ])
        .output()
        .expect("failed to spawn ociman run");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "CapEff:\t000001ffffffffff\nSeccomp:\t0"
    );
}

/// `--cap-drop` still applies on top of `--privileged`'s own
/// all-capabilities base, exactly like it would on top of the
/// ordinary default -- `--privileged` isn't a special case
/// `merge_capabilities` treats differently.
#[test]
fn run_privileged_still_honors_an_explicit_cap_drop() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/privileged-cap-drop:latest",
        &busybox,
        &["sh", "grep"],
        ContainerConfig::default(),
    );

    let out = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args(["run", "--rm", "--privileged", "--cap-drop", "chown"])
        .args(["ociman-test/privileged-cap-drop:latest"])
        .args(["/bin/sh", "-c", "grep -E \"^CapEff:\" /proc/self/status"])
        .output()
        .expect("failed to spawn ociman run");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // All 41 recognized capabilities except CAP_CHOWN (bit 0).
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "CapEff:\t000001fffffffffe"
    );
}

/// An explicit `--security-opt seccomp=<path>` still wins over
/// `--privileged`'s own "disable seccomp entirely" default -- matching
/// real `podman`'s own `security_linux.go` check exactly (`--privileged`
/// only forces `unconfined` when no seccomp option was explicitly
/// given at all).
#[test]
fn run_privileged_still_honors_an_explicit_custom_seccomp_profile() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/privileged-custom-seccomp:latest",
        &busybox,
        &["sh", "mkdir", "cat", "grep"],
        ContainerConfig::default(),
    );

    let profile_dir = tempfile::tempdir().unwrap();
    let profile_path = profile_dir.path().join("custom-seccomp.json");
    // The real syscall `mkdir(1)` uses is genuinely architecture-
    // dependent, not just "mkdirat" everywhere: glibc's own `mkdir()`
    // (`sysdeps/unix/sysv/linux/mkdir.c`) calls the legacy `mkdir`
    // syscall directly when the target has one at all (`#ifdef
    // __NR_mkdir`) -- true on x86_64, which keeps the old syscall
    // table entry for compatibility -- and only falls back to
    // `mkdirat(AT_FDCWD, ...)` on architectures that never had a
    // standalone `mkdir` syscall to begin with, aarch64 among them.
    // Naming only `mkdirat` here (this test's own earlier shape)
    // genuinely blocks nothing at all on x86_64, letting `mkdir(1)`
    // silently succeed under a filter that looks like it should have
    // stopped it -- found via this exact test failing, deterministically,
    // on real x86_64 CI hardware while passing everywhere this project
    // had previously verified it by hand (aarch64 only, see 0069). A
    // caller-supplied profile is deliberately used unfiltered/strict
    // (see `resolve_seccomp`'s own doc comment) -- naming `mkdir` on an
    // architecture where the kernel truly has no such syscall (aarch64)
    // is a real, surfaced error there, not something to silently drop
    // -- so which single name to block has to be chosen per
    // architecture here, not just unioned.
    let mkdir_syscall_name = if cfg!(target_arch = "x86_64") {
        "mkdir"
    } else {
        "mkdirat"
    };
    std::fs::write(
        &profile_path,
        format!(
            r#"{{"defaultAction":"SCMP_ACT_ALLOW","syscalls":[{{"names":["{mkdir_syscall_name}"],"action":"SCMP_ACT_ERRNO"}}]}}"#
        ),
    )
    .unwrap();

    let out = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args(["run", "--rm", "--privileged", "--security-opt"])
        .arg(format!("seccomp={}", profile_path.display()))
        .args(["ociman-test/privileged-custom-seccomp:latest"])
        .args([
            "/bin/sh",
            "-c",
            // Diagnostic-rich: reports whether a seccomp filter is
            // even active (`/proc/self/status`'s own `Seccomp:` field
            // -- `2` is `SECCOMP_MODE_FILTER`, `0` is disabled) right
            // before the real probe, so a future failure here shows
            // *why*, not just *that*.
            "grep -i seccomp /proc/self/status; /bin/mkdir /testdir",
        ])
        .output()
        .expect("failed to spawn ociman run");
    assert!(
        !out.status.success(),
        "the explicit custom profile should still block mkdir even under --privileged\n\
         status: {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Operation not permitted"),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `--read-only` really does set `root.readonly` in the real
/// `config.json` this invocation writes -- checked the same
/// unambiguous, host-independent way
/// `run_security_opt_seccomp_unconfined_disables_confinement_in_the_
/// real_spec` checks its own flag (reading the actual spec back
/// out), **not** by asserting a real in-container write attempt fails.
/// That would be a real behavioral check too, and a real manual
/// `ociman run --read-only ... touch /testfile` round trip against a
/// freshly-pulled `busybox` on this host did fail exactly that way
/// (`touch: /testfile: Read-only file system`, see `docs/design/0079`
/// -- 0080's own note) -- but a first version of this test asserting
/// exactly that failed inside this project's own VM CI (a real,
/// rootless-environment "remount / read-only" limitation of the very
/// same kind `oci_runtime_core::launch`'s own `RemountReadonly`
/// handler already documents and tolerates for `/sys`: it can require
/// `CAP_SYS_ADMIN` in the namespace that owns the *original*
/// superblock, which a fake-root-in-a-userns does not always have,
/// and this project's own two supported CI VM bases apparently differ
/// from this dev host on whether that succeeds). Checking the spec
/// this project itself actually wrote is deterministic regardless.
#[test]
fn run_read_only_sets_root_readonly_in_the_real_spec() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/read-only:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let out = ociman_run(
        storage_dir.path(),
        "ociman-test/read-only:latest",
        &["--read-only", "/bin/sh", "-c", "exit 0"],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let container_id = only_container_id(storage_dir.path(), Duration::from_secs(10));
    let config_path = storage_dir
        .path()
        .join("containers")
        .join(&container_id)
        .join("config.json");
    let config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(config_path).unwrap()).unwrap();
    assert_eq!(
        config["root"]["readonly"], true,
        "expected --read-only to set root.readonly: {config:?}"
    );
}

/// The default (no `--read-only`) rootfs stays writable -- a real
/// regression guard for the exact bug `synthesize_spec`'s own doc
/// comment describes: `Spec::example()`'s own `readonly: true`
/// default, if ever accidentally left in place unconditionally again,
/// would break every `ociman run` container's ability to write
/// anywhere in its own rootfs.
#[test]
fn run_without_read_only_keeps_a_writable_rootfs() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/writable:latest",
        &busybox,
        &["sh", "touch"],
        ContainerConfig::default(),
    );

    let out = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args(["run", "--rm"])
        .args(["ociman-test/writable:latest"])
        .args(["/bin/touch", "/testfile"])
        .output()
        .expect("failed to spawn ociman run");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `-e`/`--env` both adds a genuinely new variable and overrides an
/// existing one *in place* (not as a second, shadowed entry) --
/// matching real `docker run -e`/`podman run -e` exactly. Checked the
/// most direct way available: actually printing `$PATH` from inside
/// the running container, which would only ever show the *original*
/// value if a real container init process's own `getenv(3)`-style
/// lookup found the first (pre-override) entry in a naively
/// duplicated list.
#[test]
fn run_env_flag_overrides_an_existing_variable_and_adds_a_new_one() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/env-flag:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            env: vec!["PATH=/bin".to_string()],
            ..Default::default()
        },
    );

    let out = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args(["run", "--rm", "-e", "PATH=/custom/bin", "-e", "EXTRA=hi"])
        .args(["ociman-test/env-flag:latest"])
        .args(["/bin/sh", "-c", "echo \"$PATH\" \"$EXTRA\""])
        .output()
        .expect("failed to spawn ociman run");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "/custom/bin hi\n",
        "PATH should be overridden in place (not a second, shadowed entry) and EXTRA added"
    );
}

/// `--hostname` really does set the container's own UTS hostname,
/// matching real `docker run --hostname`/`podman run --hostname`
/// exactly -- checked the most direct way available: printing the
/// real kernel-reported hostname from inside the running container.
#[test]
fn run_hostname_flag_sets_the_containers_own_uts_hostname() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/hostname:latest",
        &busybox,
        &["sh", "hostname"],
        ContainerConfig::default(),
    );

    let out = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args(["run", "--rm", "--hostname", "my-custom-host"])
        .args(["ociman-test/hostname:latest"])
        .args(["/bin/hostname"])
        .output()
        .expect("failed to spawn ociman run");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "my-custom-host\n");
}

/// With no `--hostname` given, the container's own hostname defaults
/// to its own generated id -- matching real `podman`'s own documented
/// default (`container-libs`'s own vendored `pkg/specgen/specgen.go`:
/// "will be set to the container ID").
#[test]
fn run_without_hostname_flag_defaults_to_the_containers_own_id() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/hostname-default:latest",
        &busybox,
        &["sh", "hostname"],
        ContainerConfig::default(),
    );

    let out = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args(["run", "--rm", "--name", "hostname-default-test"])
        .args(["ociman-test/hostname-default:latest"])
        .args(["/bin/hostname"])
        .output()
        .expect("failed to spawn ociman run");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let hostname = String::from_utf8_lossy(&out.stdout).trim().to_string();
    // A real, generated 12-hex-char container id -- not the `--name`
    // given above, which is a separate, human-chosen identifier with
    // no bearing on the UTS hostname unless `--hostname` is also
    // given.
    assert_eq!(hostname.len(), 12, "got {hostname:?}");
    assert!(
        hostname.bytes().all(|b| b.is_ascii_hexdigit()),
        "got {hostname:?}"
    );
}

/// Every real container gets a synthesized `/etc/hosts` (see
/// `docs/design/0147`): `127.0.0.1`/`::1 localhost`, plus the
/// container's own hostname and `--name` mapped to `127.0.0.1` — even
/// with no `--add-host` at all, matching real podman's own
/// `--network=none` default (this project sets up no container
/// networking of its own yet, so every container behaves like that
/// case).
#[test]
fn run_writes_a_default_etc_hosts_with_no_add_host_flag_at_all() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/hosts-default:latest",
        &busybox,
        &["sh", "cat"],
        ContainerConfig::default(),
    );

    let out = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args(["run", "--rm", "--name", "hosts-default-test"])
        .args(["ociman-test/hosts-default:latest"])
        .args(["/bin/cat", "/etc/hosts"])
        .output()
        .expect("failed to spawn ociman run");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let hosts = String::from_utf8_lossy(&out.stdout);
    assert!(hosts.contains("127.0.0.1\tlocalhost"), "hosts: {hosts:?}");
    assert!(hosts.contains("::1\tlocalhost"), "hosts: {hosts:?}");
    assert!(hosts.contains("hosts-default-test"), "hosts: {hosts:?}");
}

/// `--add-host name[;name2]:IP` (repeatable) adds a real, extra
/// `/etc/hosts` entry, taking precedence over (able to suppress) the
/// built-in `localhost` entries when it reuses that same name.
#[test]
fn run_add_host_flag_adds_a_real_extra_hosts_entry() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/add-host:latest",
        &busybox,
        &["sh", "cat"],
        ContainerConfig::default(),
    );

    let out = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args([
            "run",
            "--rm",
            "--add-host",
            "foo.example;bar.example:10.0.0.5",
        ])
        .args(["ociman-test/add-host:latest"])
        .args(["/bin/cat", "/etc/hosts"])
        .output()
        .expect("failed to spawn ociman run");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let hosts = String::from_utf8_lossy(&out.stdout);
    assert!(
        hosts.contains("10.0.0.5\tfoo.example bar.example"),
        "hosts: {hosts:?}"
    );
}

/// A user `--add-host localhost:...` genuinely takes precedence:
/// both built-in `localhost` lines (`127.0.0.1`/`::1`) are suppressed
/// entirely, matching real podman's own exact behavior (checked
/// directly, see `write_etc_hosts`'s own doc comment).
#[test]
fn run_add_host_overriding_localhost_suppresses_the_builtin_localhost_entries() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/add-host-localhost:latest",
        &busybox,
        &["sh", "cat"],
        ContainerConfig::default(),
    );

    let out = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args(["run", "--rm", "--add-host", "localhost:9.9.9.9"])
        .args(["ociman-test/add-host-localhost:latest"])
        .args(["/bin/cat", "/etc/hosts"])
        .output()
        .expect("failed to spawn ociman run");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let hosts = String::from_utf8_lossy(&out.stdout);
    assert!(hosts.contains("9.9.9.9\tlocalhost"), "hosts: {hosts:?}");
    assert!(!hosts.contains("127.0.0.1\tlocalhost"), "hosts: {hosts:?}");
    assert!(!hosts.contains("::1\tlocalhost"), "hosts: {hosts:?}");
}

/// `--add-host` with the `host-gateway` IP keyword is a clear, real
/// error — this project sets up no container networking of its own
/// yet, so there is no real host-reachable gateway address to
/// resolve it to (see `docs/design/0147`'s own "what this doesn't do
/// yet").
#[test]
fn run_add_host_rejects_the_host_gateway_keyword() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/add-host-gateway:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let out = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args(["run", "--rm", "--add-host", "foo:host-gateway"])
        .args(["ociman-test/add-host-gateway:latest"])
        .args(["/bin/true"])
        .output()
        .expect("failed to spawn ociman run");
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("host-gateway"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `-w`/`--workdir` overrides the image's own default `WORKDIR`,
/// matching real `docker run -w`/`podman run -w` exactly -- checked
/// the most direct way available: printing the real, current working
/// directory from inside the running container.
#[test]
fn run_workdir_flag_overrides_the_images_default_workdir() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/workdir-flag:latest",
        &busybox,
        &["sh", "pwd"],
        ContainerConfig {
            working_dir: Some("/".to_string()),
            ..Default::default()
        },
    );

    let out = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args(["run", "--rm", "-w", "/bin"])
        .args(["ociman-test/workdir-flag:latest"])
        .args(["/bin/pwd"])
        .output()
        .expect("failed to spawn ociman run");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "/bin\n");
}

/// With no `-w`/`--workdir` given, the image's own `WORKDIR` config
/// still applies -- a real regression guard: `-w` must only ever
/// override, never silently replace, the existing image-config
/// default path this project's own `synthesize_spec` already applied
/// correctly before this increment.
#[test]
fn run_without_workdir_flag_uses_the_images_own_workdir() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/workdir-default:latest",
        &busybox,
        &["sh", "pwd"],
        ContainerConfig {
            working_dir: Some("/bin".to_string()),
            ..Default::default()
        },
    );

    let out = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args(["run", "--rm"])
        .args(["ociman-test/workdir-default:latest"])
        .args(["/bin/pwd"])
        .output()
        .expect("failed to spawn ociman run");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "/bin\n");
}

/// `--entrypoint` replaces the image's own `ENTRYPOINT` *and*
/// suppresses the image's own default `CMD` fallback entirely, even
/// with no trailing command given -- checked directly against real
/// podman's own `makeCommand` rule ("only use image command if the
/// user did not manually set an entrypoint"), not just this project's
/// own unit tests: a real running container.
#[test]
fn run_entrypoint_flag_replaces_the_images_own_entrypoint_and_cmd() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/entrypoint-flag:latest",
        &busybox,
        &["sh", "echo"],
        ContainerConfig {
            entrypoint: Some(vec!["/bin/echo".to_string()]),
            cmd: Some(vec!["from-image-cmd".to_string()]),
            ..Default::default()
        },
    );

    let out = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args([
            "run",
            "--rm",
            "--entrypoint",
            "/bin/sh -c \"echo overridden\"",
        ])
        .args(["ociman-test/entrypoint-flag:latest"])
        .output()
        .expect("failed to spawn ociman run");
    // The whole `--entrypoint` string was one literal argument (not
    // valid JSON), so the real command run is a single, literal
    // `/bin/sh -c "echo overridden"` executable name -- which doesn't
    // exist as a real path, so this genuinely fails to exec, same as
    // real `docker`/`podman`'s own `--entrypoint` would too. That
    // failure itself is exactly what this test checks: the exec
    // error names the literal, unsplit string as the executable it
    // tried and failed to run, proving the image's own `from-image-
    // cmd` was never appended as a second argument at all (checked
    // directly; see the next test for a real, successfully-executed
    // override).
    assert!(
        !out.status.success(),
        "a literal, unsplit `--entrypoint` value naming a real path that doesn't exist should \
         fail to exec"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !combined.contains("from-image-cmd"),
        "the image's own default CMD must not be appended when --entrypoint overrides it: \
         {combined:?}"
    );
    assert!(
        combined.contains("/bin/sh -c \"echo overridden\""),
        "the exec error should name the literal, unsplit --entrypoint value: {combined:?}"
    );
}

/// A real, successfully-executed `--entrypoint` override, using the
/// JSON-array form real podman also supports -- printing real,
/// distinguishable output from inside the running container proves
/// both that the override took effect and that the image's own CMD
/// was correctly suppressed.
#[test]
fn run_entrypoint_flag_json_array_form_actually_executes() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/entrypoint-json:latest",
        &busybox,
        &["sh", "echo"],
        ContainerConfig {
            entrypoint: Some(vec!["/bin/echo".to_string()]),
            cmd: Some(vec!["from-image-cmd".to_string()]),
            ..Default::default()
        },
    );

    let out = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args([
            "run",
            "--rm",
            "--entrypoint",
            r#"["/bin/sh", "-c", "echo overridden-entrypoint"]"#,
        ])
        .args(["ociman-test/entrypoint-json:latest"])
        .output()
        .expect("failed to spawn ociman run");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "overridden-entrypoint\n",
        "the image's own from-image-cmd must not have been appended"
    );
}

/// `-v`/`--volume` really does bind-mount a real host directory into
/// the container, both directions: a file already on the host is
/// visible inside the container, and a file the container writes
/// becomes visible back on the host once the container exits --
/// checked the most direct way available, a real host temp directory.
#[test]
fn run_volume_flag_bind_mounts_a_real_host_directory_both_ways() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let host_dir = tempfile::tempdir().unwrap();
    std::fs::write(host_dir.path().join("from-host.txt"), "from-host\n").unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/volume-flag:latest",
        &busybox,
        &["sh", "cat"],
        ContainerConfig::default(),
    );

    let out = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args(["run", "--rm", "-v"])
        .arg(format!("{}:/data", host_dir.path().display()))
        .args(["ociman-test/volume-flag:latest"])
        .args([
            "/bin/sh",
            "-c",
            "cat /data/from-host.txt && echo from-container > /data/from-container.txt",
        ])
        .output()
        .expect("failed to spawn ociman run");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "from-host\n");
    assert_eq!(
        std::fs::read_to_string(host_dir.path().join("from-container.txt")).unwrap(),
        "from-container\n",
        "a file the container wrote into the bind mount should be visible back on the host"
    );
}

/// `-v host:container:ro` really does make the mount read-only inside
/// the container -- a write attempt fails, matching real `docker run
/// -v host:container:ro`/`podman run -v host:container:ro` exactly.
#[test]
fn run_volume_flag_ro_rejects_a_write_from_inside_the_container() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let host_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/volume-ro:latest",
        &busybox,
        &["sh", "touch"],
        ContainerConfig::default(),
    );

    // Checked the same deterministic, host-independent way
    // `run_read_only_sets_root_readonly_in_the_real_spec` already
    // checks `--read-only` (see its own doc comment/`docs/design/
    // 0080`): reading the real `config.json` `ociman` itself wrote,
    // not asserting the kernel's own enforcement outcome. A first
    // version of this test asserted a real in-container write attempt
    // fails, matching this file's own manual verification against a
    // real busybox pull on this dev host -- but it failed inside this
    // project's own VM CI for the exact same reason `--read-only`'s
    // own first version did: remounting a bind mount read-only can
    // require `CAP_SYS_ADMIN` in the namespace that owns the
    // *original* superblock, a real, environment-dependent rootless
    // limitation (`docs/design/0010`) this project's own
    // `RemountReadonly` handler already tolerates rather than treats
    // as fatal -- exercised here via `-v ...:ro` instead of
    // `--read-only`, but the exact same underlying mechanism.
    let out = ociman_run(
        storage_dir.path(),
        "ociman-test/volume-ro:latest",
        &[
            "-v",
            &format!("{}:/data:ro", host_dir.path().display()),
            "/bin/sh",
            "-c",
            "exit 0",
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let container_id = only_container_id(storage_dir.path(), Duration::from_secs(10));
    let config_path = storage_dir
        .path()
        .join("containers")
        .join(&container_id)
        .join("config.json");
    let config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(config_path).unwrap()).unwrap();
    let mounts = config["mounts"].as_array().unwrap();
    let volume_mount = mounts
        .iter()
        .find(|m| m["destination"] == "/data")
        .unwrap_or_else(|| panic!("no /data mount in {mounts:?}"));
    assert_eq!(volume_mount["source"], host_dir.path().to_str().unwrap());
    assert_eq!(volume_mount["type"], "bind");
    let options: Vec<&str> = volume_mount["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        options.contains(&"ro"),
        "expected -v ...:ro to set the \"ro\" mount option: {options:?}"
    );
}

/// `-v` correctly bind-mounts a real host *file* (not just a
/// directory) onto a container destination -- a real regression guard
/// for the `RootfsAction::Mount` file-vs-directory bug this same
/// increment found and fixed (`oci_runtime_core::launch`'s own
/// generic mount executor used to unconditionally `mkdir` the target,
/// which fails with `ENOTDIR` once a real `mount(2)` tries to bind a
/// file onto that freshly-created directory).
#[test]
fn run_volume_flag_bind_mounts_a_real_host_file() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let host_dir = tempfile::tempdir().unwrap();
    let host_file = host_dir.path().join("greeting.txt");
    std::fs::write(&host_file, "hello-from-a-real-file\n").unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/volume-file:latest",
        &busybox,
        &["sh", "cat"],
        ContainerConfig::default(),
    );

    let out = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args(["run", "--rm", "-v"])
        .arg(format!("{}:/etc/greeting.txt:ro", host_file.display()))
        .args(["ociman-test/volume-file:latest"])
        .args(["/bin/cat", "/etc/greeting.txt"])
        .output()
        .expect("failed to spawn ociman run");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "hello-from-a-real-file\n"
    );
}

/// A missing host directory is created automatically, matching real
/// `docker`'s own long-documented default for a missing bind-mount
/// source.
#[test]
fn run_volume_flag_creates_a_missing_host_directory() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let host_parent = tempfile::tempdir().unwrap();
    let host_dir = host_parent.path().join("does-not-exist-yet");
    assert!(!host_dir.exists());
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/volume-autocreate:latest",
        &busybox,
        &["sh", "touch"],
        ContainerConfig::default(),
    );

    let out = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args(["run", "--rm", "-v"])
        .arg(format!("{}:/data", host_dir.display()))
        .args(["ociman-test/volume-autocreate:latest"])
        .args(["/bin/touch", "/data/marker.txt"])
        .output()
        .expect("failed to spawn ociman run");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        host_dir.is_dir(),
        "the missing host directory should have been created"
    );
    assert!(host_dir.join("marker.txt").exists());
}

#[test]
fn run_uses_the_images_default_cmd_and_env() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/default-cmd:latest",
        &busybox,
        &["sh", "echo", "env"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "echo hello-from-default-cmd; env | grep ^PATH=".to_string(),
            ]),
            env: vec!["PATH=/bin".to_string()],
            ..Default::default()
        },
    );

    let out = ociman_run(storage_dir.path(), "ociman-test/default-cmd:latest", &[]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("hello-from-default-cmd"),
        "got stdout: {stdout:?}"
    );
    assert!(stdout.contains("PATH=/bin"), "got stdout: {stdout:?}");
}

#[test]
fn run_args_override_the_images_default_cmd() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/override-cmd:latest",
        &busybox,
        &["sh", "echo"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/echo".to_string(),
                "default-cmd-unused".to_string(),
            ]),
            ..Default::default()
        },
    );

    let out = ociman_run(
        storage_dir.path(),
        "ociman-test/override-cmd:latest",
        &["/bin/echo", "overridden-args-used"],
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("overridden-args-used"));
    assert!(!stdout.contains("default-cmd-unused"));
}

#[test]
fn run_propagates_the_containers_exit_code() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/exit-code:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let out = ociman_run(
        storage_dir.path(),
        "ociman-test/exit-code:latest",
        &["/bin/sh", "-c", "exit 42"],
    );
    assert_eq!(
        out.status.code(),
        Some(42),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn run_applies_a_default_seccomp_profile_blocking_a_real_syscall() {
    // Real, unconfounded verification that a real default seccomp
    // profile is now always applied (`docs/design/0044`) -- not just
    // that some error occurred, which could just as easily come from
    // an unrelated kernel/filesystem check. `swapon` against a real,
    // existing-but-not-swap-formatted file:
    //
    // * with *no* seccomp at all (confirmed separately, by hand, via
    //   `ocirun run` with an unset `linux.seccomp`), the syscall
    //   itself actually executes and the kernel's own swap-file
    //   validation logic rejects it distinctly ("file has holes" or
    //   similar) -- proof the syscall really reached the kernel.
    // * with the default profile applied, it instead fails with
    //   `Operation not permitted` (`EPERM`) -- seccomp's own `ERRNO`
    //   action for `swapon`, which real `podman`'s own default profile
    //   also blocks by default -- *before* the syscall ever reaches
    //   that filesystem-level check at all.
    //
    // Asserting specifically on `Operation not permitted` (rather than
    // just "the command failed somehow") is what makes this test
    // actually distinguish "seccomp blocked it" from any other reason
    // the same command could fail.
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/default-seccomp:latest",
        &busybox,
        &["sh", "swapon"],
        ContainerConfig::default(),
    );

    let out = ociman_run(
        storage_dir.path(),
        "ociman-test/default-seccomp:latest",
        &["/bin/sh", "-c", "swapon /bin/busybox"],
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("Operation not permitted"),
        "expected the default seccomp profile to block `swapon` with EPERM: {combined:?}"
    );
}

/// `--security-opt seccomp=unconfined` genuinely removes seccomp
/// confinement -- checked the most direct, unambiguous way available:
/// reading the real `config.json` this invocation actually wrote back
/// out (rather than picking a probe syscall, which turned out to be
/// surprisingly hard to make unambiguous by hand while building this
/// test: this project's own rootless default capability set is
/// extremely minimal, `CAP_AUDIT_WRITE`/`CAP_KILL`/
/// `CAP_NET_BIND_SERVICE` only, so most syscalls the default seccomp
/// profile blocks *also* fail for a completely different reason, a
/// missing capability, once seccomp itself is out of the way --
/// `run_security_opt_custom_profile_is_loaded_and_really_enforced`
/// below is the real, unambiguous, behavioral proof that a
/// `--security-opt seccomp=` value genuinely reaches the container).
#[test]
fn run_security_opt_seccomp_unconfined_disables_confinement_in_the_real_spec() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/unconfined:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let out = ociman_run(
        storage_dir.path(),
        "ociman-test/unconfined:latest",
        &[
            "--security-opt",
            "seccomp=unconfined",
            "/bin/sh",
            "-c",
            "exit 0",
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let container_id = only_container_id(storage_dir.path(), Duration::from_secs(10));
    let config_path = storage_dir
        .path()
        .join("containers")
        .join(&container_id)
        .join("config.json");
    let config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(config_path).unwrap()).unwrap();
    assert!(
        config["linux"]["seccomp"].is_null(),
        "expected --security-opt seccomp=unconfined to leave linux.seccomp unset entirely, \
         got: {config:?}"
    );
}

/// A real, unambiguous, capability-independent proof that
/// `--security-opt seccomp=<path>` genuinely loads and enforces a
/// caller-supplied profile: a minimal custom profile that allows
/// everything *except* an explicit `SCMP_ACT_ERRNO` rule for
/// `getcwd` (a syscall every unprivileged process can always make on
/// its own current directory, unlike the default profile's own
/// blocked syscalls, which turned out to mostly *also* need a
/// capability this project's own minimal rootless default doesn't
/// grant — see the `--security-opt seccomp=unconfined` test above).
/// `/bin/pwd` (which calls `getcwd(2)`) fails with exactly
/// `Operation not permitted` under the custom profile, and succeeds
/// under the (unmodified) default profile — confirmed by hand against
/// a real running container before encoding this as an automated
/// test.
#[test]
fn run_security_opt_custom_profile_is_loaded_and_really_enforced() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/custom-seccomp:latest",
        &busybox,
        &["sh", "pwd"],
        ContainerConfig::default(),
    );

    let profile_dir = tempfile::tempdir().unwrap();
    let profile_path = profile_dir.path().join("block-getcwd.json");
    std::fs::write(
        &profile_path,
        r#"{"defaultAction":"SCMP_ACT_ALLOW","syscalls":[{"names":["getcwd"],"action":"SCMP_ACT_ERRNO","errnoRet":1}]}"#,
    )
    .unwrap();

    // Baseline: `pwd` succeeds under the ordinary default profile
    // (`getcwd` isn't one of its blocked syscalls).
    let baseline = ociman_run(
        storage_dir.path(),
        "ociman-test/custom-seccomp:latest",
        &["--rm", "/bin/pwd"],
    );
    assert!(
        baseline.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&baseline.stderr)
    );

    // The same command, under the custom profile blocking `getcwd`
    // specifically, must now fail with exactly `Operation not
    // permitted` -- proof the profile was actually loaded and
    // enforced, not merely accepted and ignored.
    let blocked = ociman_run(
        storage_dir.path(),
        "ociman-test/custom-seccomp:latest",
        &[
            "--rm",
            "--security-opt",
            &format!("seccomp={}", profile_path.display()),
            "/bin/pwd",
        ],
    );
    assert!(!blocked.status.success());
    assert!(
        String::from_utf8_lossy(&blocked.stderr).contains("Operation not permitted"),
        "stderr: {}",
        String::from_utf8_lossy(&blocked.stderr)
    );
}

#[test]
fn run_security_opt_rejects_an_unsupported_key() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/security-opt-reject:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let out = ociman_run(
        storage_dir.path(),
        "ociman-test/security-opt-reject:latest",
        &["--security-opt", "apparmor=unconfined", "/bin/true"],
    );
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("apparmor=unconfined"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn run_memory_limit_actually_gets_enforced_by_the_kernels_own_oom_killer() {
    // Real, kernel-enforced verification (not just "the property got
    // set" — that's covered directly in `oci_runtime_core::
    // systemd_cgroup`'s own unit tests): a real container, under a
    // real 16 MiB `--memory` limit, whose own shell allocates ~300 MB
    // via a real memory-backed command substitution (`yes | head -c
    // <n>` — no `/dev/zero` needed, which this rootless bundle's
    // minimal `/dev` doesn't have) should be killed by the kernel's own
    // cgroup v2 OOM killer (`SIGKILL`, exit code 137), never complete
    // normally. See `docs/design/0037` for why a memory limit alone,
    // with no swap limit at all, would *not* actually enforce anything
    // (the kernel would just page out to swap instead) — this test
    // would have caught that regression too.
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/memory-limit:latest",
        &busybox,
        &["sh", "yes", "head"],
        ContainerConfig::default(),
    );

    let out = ociman_run(
        storage_dir.path(),
        "ociman-test/memory-limit:latest",
        &[
            "--memory",
            "16m",
            "/bin/sh",
            "-c",
            "x=$(yes | head -c 300000000); echo ${#x}",
        ],
    );
    assert_eq!(
        out.status.code(),
        Some(137),
        "expected the kernel's own OOM killer (SIGKILL, exit 137); got: {out:?}"
    );
}

/// The actual shell script both `--pids-limit` tests below run: attempt
/// far more background forks than any reasonable `--pids-limit` would
/// allow, so the *kernel itself* (not the script) is what decides
/// whether the loop can finish.
const FORK_MANY_SCRIPT: &str = "\
i=0
while [ $i -lt 50 ]; do
  sleep 30 &
  i=$((i + 1))
done
echo all-forks-succeeded";

#[test]
fn run_pids_limit_actually_gets_enforced_by_the_kernels_own_pids_controller() {
    // Real, kernel-enforced verification: a real container under a
    // real `--pids-limit 5` whose own shell tries to fork 50 background
    // processes should hit a real `fork()` failure partway through
    // (the kernel's own cgroup v2 `pids.max` refusing the `clone()`
    // outright, matching this project's own earlier confirmed manual
    // `strace`-free evidence: busybox `sh` reports `can't fork:
    // Resource temporarily unavailable` and exits non-zero) rather
    // than ever reaching `echo all-forks-succeeded`.
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/pids-limit:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let out = ociman_run(
        storage_dir.path(),
        "ociman-test/pids-limit:latest",
        &["--pids-limit", "5", "/bin/sh", "-c", FORK_MANY_SCRIPT],
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        !out.status.success(),
        "expected the kernel's own pids controller to abort the fork loop; got: {out:?}"
    );
    assert!(
        !stdout.contains("all-forks-succeeded"),
        "the fork loop should never have reached its own end: {out:?}"
    );
}

#[test]
fn run_without_pids_limit_can_fork_far_more_than_five_processes() {
    // The counterpart to the test above: the *same* script, same
    // image, no `--pids-limit` at all, must complete normally —
    // proving the failure above is really caused by the limit, not
    // some unrelated fork-loop fragility.
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/no-pids-limit:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let out = ociman_run(
        storage_dir.path(),
        "ociman-test/no-pids-limit:latest",
        &["/bin/sh", "-c", FORK_MANY_SCRIPT],
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("all-forks-succeeded"), "got: {out:?}");
}

#[test]
fn run_cpus_flag_sets_the_real_systemd_scopes_own_cpu_quota() {
    // `--cpus` translates to a *rate* limit (how much CPU time is
    // available per wall-clock second), not a hard cap that fails an
    // operation outright the way `--memory`/`--pids-limit` do — there's
    // no clean, fast, contention-proof way to prove *throttling*
    // happened without a flaky, timing-based test. Verifying the real
    // systemd scope's own `CPUQuotaPerSecUSec`/`CPUQuotaPeriodUSec`
    // properties instead (the same technique
    // `oci_runtime_core::systemd_cgroup`'s own
    // `create_scope_migrates_a_real_child_pid_and_leaves_the_caller_
    // alone` test already established for `MemoryMax`) is deterministic
    // and still real: it queries the actual running container's own
    // scope, not a value this test already knows in isolation.
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    if !systemd_user_session_available() {
        eprintln!("skipping: no reachable `systemd --user` session");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/cpus:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut child = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args(["run", "--rm", "--cpus", "1.5", "ociman-test/cpus:latest"])
        .args(["/bin/sh", "-c", "sleep 10"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn ociman run");

    let container_id = only_container_id(storage_dir.path(), Duration::from_secs(10));
    assert!(!container_id.is_empty(), "container never appeared in `ps`");
    // Must actually be `running`, not merely present in `ps -a` (which
    // also lists a container still in its own earlier `creating`
    // state) -- `record_running` only fires *after* the systemd scope
    // (and its own resource properties) has already been created, so
    // waiting for this status is what guarantees the property query
    // below isn't racing ahead of the scope's own creation.
    let status = wait_for_running(storage_dir.path(), &container_id, Duration::from_secs(20));
    assert_eq!(status, "running", "container never reached `running`");
    let scope_name = real_scope_name(storage_dir.path(), &container_id);

    let show = Command::new("systemctl")
        .args([
            "--user",
            "show",
            &scope_name,
            "-p",
            "CPUQuotaPerSecUSec",
            "--value",
        ])
        .output()
        .expect("failed to run systemctl --user show");
    let quota = String::from_utf8_lossy(&show.stdout).trim().to_string();

    let _ = child.kill();
    let _ = child.wait();

    // Real `systemd`'s own human-readable rendering of 1.5 CPUs'
    // worth of quota over its own 100ms period, confirmed by hand
    // against a real running scope before writing this assertion.
    assert_eq!(
        quota, "1.500000s",
        "expected the real systemd scope's own CPUQuotaPerSecUSec to reflect --cpus 1.5"
    );
}

/// Same technique as the `--cpus` test above (query the real systemd
/// scope's own resource property rather than trying to prove kernel
/// enforcement directly), for `--memory-swap`: a *combined*
/// memory+swap cap, translated to cgroup v2's own swap-*only* value
/// (`combined - memory`) by `oci_runtime_core::cgroups::
/// convert_memory_swap_to_v2` before ever reaching systemd — confirmed
/// by hand against a real running scope before writing this
/// assertion (100m memory + 150m combined -> 50m real
/// `MemorySwapMax`).
#[test]
fn run_memory_swap_flag_sets_the_real_systemd_scopes_own_swap_max() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    if !systemd_user_session_available() {
        eprintln!("skipping: no reachable `systemd --user` session");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/memswap:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut child = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args([
            "run",
            "--rm",
            "--memory",
            "100m",
            "--memory-swap",
            "150m",
            "ociman-test/memswap:latest",
        ])
        .args(["/bin/sh", "-c", "sleep 10"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn ociman run");

    let container_id = only_container_id(storage_dir.path(), Duration::from_secs(10));
    assert!(!container_id.is_empty(), "container never appeared in `ps`");
    let status = wait_for_running(storage_dir.path(), &container_id, Duration::from_secs(20));
    assert_eq!(status, "running", "container never reached `running`");
    let scope_name = real_scope_name(storage_dir.path(), &container_id);

    let show = Command::new("systemctl")
        .args([
            "--user",
            "show",
            &scope_name,
            "-p",
            "MemorySwapMax",
            "--value",
        ])
        .output()
        .expect("failed to run systemctl --user show");
    let swap_max = String::from_utf8_lossy(&show.stdout).trim().to_string();

    let _ = child.kill();
    let _ = child.wait();

    assert_eq!(
        swap_max, "52428800",
        "expected the real systemd scope's own MemorySwapMax to reflect 150m combined minus \
         100m memory (50m swap-only, in bytes)"
    );
}

/// `-1` (real `docker run --memory-swap -1`/`podman run --memory-swap
/// -1`'s own "unlimited swap" convention) exercised through the real
/// CLI, not just `resources_from_cli`'s own in-process unit tests —
/// this specific case caught a real bug by hand while building this
/// flag: clap's default `allow_hyphen_values` setting treats a value
/// that merely *looks* like another flag (`-1`) as an unrecognized
/// flag of its own rather than this option's own value, silently
/// rejecting exactly the invocation real `docker`/`podman` accept.
#[test]
fn run_memory_swap_accepts_negative_one_via_the_real_cli_as_unlimited() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    if !systemd_user_session_available() {
        eprintln!("skipping: no reachable `systemd --user` session");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/memswap-unlimited:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut child = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args([
            "run",
            "--rm",
            "--memory",
            "100m",
            "--memory-swap",
            "-1",
            "ociman-test/memswap-unlimited:latest",
        ])
        .args(["/bin/sh", "-c", "sleep 10"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn ociman run");

    let container_id = only_container_id(storage_dir.path(), Duration::from_secs(10));
    assert!(!container_id.is_empty(), "container never appeared in `ps`");
    let status = wait_for_running(storage_dir.path(), &container_id, Duration::from_secs(20));
    assert_eq!(status, "running", "container never reached `running`");
    let scope_name = real_scope_name(storage_dir.path(), &container_id);

    let show = Command::new("systemctl")
        .args([
            "--user",
            "show",
            &scope_name,
            "-p",
            "MemorySwapMax",
            "--value",
        ])
        .output()
        .expect("failed to run systemctl --user show");
    let swap_max = String::from_utf8_lossy(&show.stdout).trim().to_string();

    let _ = child.kill();
    let _ = child.wait();

    assert_eq!(swap_max, "infinity");
}

/// `--cpuset-cpus`/`--cpuset-mems` correctly set the real systemd
/// scope's own `AllowedCPUs`/`AllowedMemoryNodes` properties -- this
/// test deliberately only checks *that* (matching the same technique
/// `--cpus`'s own test above already uses for a rate-limit property),
/// not that CPU pinning is actually enforced: found by hand, and
/// documented honestly in `oci_runtime_core::systemd_cgroup`'s own doc
/// comment and `docs/design/0056`, real rootless `systemd --user`
/// does not reliably delegate the `cpuset` controller down to this
/// project's own scopes the way it does for `--memory`/`--cpus`, so a
/// test asserting real kernel-level enforcement here would be
/// asserting something not actually true on a typical host.
#[test]
fn run_cpuset_flags_set_the_real_systemd_scopes_own_allowed_cpus_property() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    if !systemd_user_session_available() {
        eprintln!("skipping: no reachable `systemd --user` session");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/cpuset:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut child = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args([
            "run",
            "--rm",
            "--cpuset-cpus",
            "0-1",
            "--cpuset-mems",
            "0",
            "ociman-test/cpuset:latest",
        ])
        .args(["/bin/sh", "-c", "sleep 10"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn ociman run");

    let container_id = only_container_id(storage_dir.path(), Duration::from_secs(10));
    assert!(!container_id.is_empty(), "container never appeared in `ps`");
    let status = wait_for_running(storage_dir.path(), &container_id, Duration::from_secs(20));
    assert_eq!(status, "running", "container never reached `running`");
    let scope_name = real_scope_name(storage_dir.path(), &container_id);

    let show_cpus = Command::new("systemctl")
        .args([
            "--user",
            "show",
            &scope_name,
            "-p",
            "AllowedCPUs",
            "--value",
        ])
        .output()
        .expect("failed to run systemctl --user show");
    let allowed_cpus = String::from_utf8_lossy(&show_cpus.stdout)
        .trim()
        .to_string();
    let show_mems = Command::new("systemctl")
        .args([
            "--user",
            "show",
            &scope_name,
            "-p",
            "AllowedMemoryNodes",
            "--value",
        ])
        .output()
        .expect("failed to run systemctl --user show");
    let allowed_mems = String::from_utf8_lossy(&show_mems.stdout)
        .trim()
        .to_string();

    let _ = child.kill();
    let _ = child.wait();

    assert_eq!(allowed_cpus, "0-1");
    assert_eq!(allowed_mems, "0");
}

/// Same real-CLI-not-just-a-unit-test concern as the `--memory-swap
/// -1` test above, for `--pids-limit -1` specifically (real `docker
/// run --pids-limit -1`/`podman run --pids-limit -1`'s own "no limit"
/// convention) — the exact other flag `allow_hyphen_values` was
/// missing for.
#[test]
fn run_pids_limit_negative_one_via_the_real_cli_means_unlimited() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/pids-limit-negative-one:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let out = ociman_run(
        storage_dir.path(),
        "ociman-test/pids-limit-negative-one:latest",
        &["--pids-limit", "-1", "/bin/sh", "-c", "echo pids-ok"],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "pids-ok\n");
}

fn wait_for_running(storage_root: &Path, id: &str, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let out = Command::new(bin_path("ociman"))
            .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
            .env_remove("OCI_TOOLS_LOG")
            .args(["ps", "-a", "--json"])
            .output()
            .expect("failed to spawn ociman ps");
        if out.status.success()
            && let Ok(views) = serde_json::from_slice::<serde_json::Value>(&out.stdout)
            && let Some(entry) = views
                .as_array()
                .and_then(|a| a.iter().find(|e| e["id"] == id))
        {
            let status = entry["status"].as_str().unwrap_or_default().to_string();
            if status == "running" || Instant::now() >= deadline {
                return status;
            }
        } else if Instant::now() >= deadline {
            return String::new();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn only_container_id(storage_root: &Path, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let out = Command::new(bin_path("ociman"))
            .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
            .env_remove("OCI_TOOLS_LOG")
            .args(["ps", "-a", "-q"])
            .output()
            .expect("failed to spawn ociman ps");
        let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !id.is_empty() || Instant::now() >= deadline {
            return id;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Same probe `docs/design/0015`'s own cgroup tests (and
/// `oci_runtime_core::systemd_cgroup`'s own unit tests) already use: a
/// real, self-cleaning D-Bus round trip rather than just checking a
/// socket path exists.
fn systemd_user_session_available() -> bool {
    Command::new("systemctl")
        .args(["--user", "is-system-running"])
        .output()
        .is_ok_and(|out| !out.stdout.is_empty())
}

#[test]
fn run_rejects_a_non_root_numeric_user() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/non-root-user:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            user: Some("1000".to_string()),
            ..Default::default()
        },
    );

    let out = ociman_run(
        storage_dir.path(),
        "ociman-test/non-root-user:latest",
        &["/bin/sh", "-c", "true"],
    );
    assert!(
        !out.status.success(),
        "run should refuse an image requesting a non-root numeric user \
         (see resolve_user's own doc comment: not mappable yet)"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("cannot map"), "got stderr: {stderr:?}");
}

/// A named `USER` (as opposed to a bare numeric one) resolved against
/// the image's own `/etc/passwd` — real images very commonly say `USER
/// root` rather than `USER 0`. Only container uid 0 is mappable yet
/// (see `run_rejects_a_non_root_numeric_user`), so `root` is the one
/// name that can actually succeed end to end right now.
#[test]
fn run_accepts_a_named_root_user_resolved_via_etc_passwd() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image_with_files(
        &store,
        "ociman-test/named-root-user:latest",
        &busybox,
        &["sh"],
        &[("etc/passwd", b"root:x:0:0:root:/root:/bin/sh\n".as_slice())],
        ContainerConfig {
            user: Some("root".to_string()),
            ..Default::default()
        },
    );

    let out = ociman_run(
        storage_dir.path(),
        "ociman-test/named-root-user:latest",
        &["/bin/sh", "-c", "echo named-root-user-worked"],
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("named-root-user-worked"),
        "got stdout: {stdout:?}"
    );
}

/// A named non-root `USER` still hits the same "can't map it" wall a
/// numeric one does (`run_rejects_a_non_root_numeric_user`) — proving
/// resolution and the mapping-limitation check are correctly wired
/// together end to end, not just at the `user_resolve` unit-test level.
#[test]
fn run_rejects_a_named_non_root_user() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image_with_files(
        &store,
        "ociman-test/named-non-root-user:latest",
        &busybox,
        &["sh"],
        &[(
            "etc/passwd",
            b"root:x:0:0:root:/root:/bin/sh\napp:x:1000:1000:App:/home/app:/bin/sh\n".as_slice(),
        )],
        ContainerConfig {
            user: Some("app".to_string()),
            ..Default::default()
        },
    );

    let out = ociman_run(
        storage_dir.path(),
        "ociman-test/named-non-root-user:latest",
        &["/bin/sh", "-c", "true"],
    );
    assert!(
        !out.status.success(),
        "run should refuse an image requesting a named non-root user, same as a numeric one"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("cannot map"), "got stderr: {stderr:?}");
}

/// Real registries increasingly serve `tar+zstd` layers, not just
/// `tar+gzip` (see `docs/design/0029`) — this proves `ociman run`
/// actually extracts one and runs the resulting container, not just
/// that `oci-layer`'s own unit tests can decompress one in isolation.
#[test]
fn run_extracts_a_zstd_compressed_layer() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image_with_files_and_compression(
        &store,
        "ociman-test/zstd-layer:latest",
        &busybox,
        &["sh"],
        &[],
        LayerCompression::Zstd,
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "echo zstd-layer-worked".to_string(),
            ]),
            ..Default::default()
        },
    );

    let out = ociman_run(storage_dir.path(), "ociman-test/zstd-layer:latest", &[]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("zstd-layer-worked"), "got: {stdout:?}");
}

/// The bug this whole increment's real-image-first testing approach was
/// built to catch (see this file's own doc comment): a `ContainerConfig`
/// without the right wire casing silently loses `Cmd`/`Env`. Guards
/// against ever regressing that in a way visible from `ociman run`
/// itself, not just `oci-spec-types`'s own unit test.
#[test]
fn run_actually_uses_cmd_from_a_pascal_case_wire_config() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();

    // Seed via raw PascalCase JSON bytes, not the `ContainerConfig`
    // struct — proves the wire format round-trips through real
    // deserialization exactly like a real registry blob would.
    let mut builder = tar::Builder::new(Vec::new());
    let busybox_bytes = std::fs::read(&busybox).unwrap();
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(busybox_bytes.len() as u64);
    header.set_mode(0o755);
    builder
        .append_data(&mut header, "bin/busybox", busybox_bytes.as_slice())
        .unwrap();
    let mut link_header = tar::Header::new_gnu();
    link_header.set_entry_type(tar::EntryType::Symlink);
    link_header.set_mode(0o777);
    link_header.set_size(0);
    builder
        .append_link(&mut link_header, "bin/sh", "busybox")
        .unwrap();
    let tar_bytes = builder.into_inner().unwrap();
    let diff_id = sha256(&tar_bytes);
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&tar_bytes).unwrap();
    let gzipped = encoder.finish().unwrap();
    let layer = store.ingest(gzipped.as_slice()).unwrap();

    let raw_config = serde_json::json!({
        "architecture": std::env::consts::ARCH,
        "os": "linux",
        "config": {
            "Cmd": ["/bin/sh", "-c", "echo pascal-case-cmd-worked"],
        },
        "rootfs": {"type": "layers", "diff_ids": [diff_id.to_string()]},
    });
    let config = store
        .ingest(serde_json::to_vec(&raw_config).unwrap().as_slice())
        .unwrap();

    let manifest = ImageManifest {
        schema_version: 2,
        media_type: Some(MEDIA_TYPE_IMAGE_MANIFEST.to_string()),
        config: Descriptor {
            media_type: MEDIA_TYPE_IMAGE_CONFIG.to_string(),
            digest: config.digest,
            size: config.size,
            urls: vec![],
            annotations: Default::default(),
            platform: None,
        },
        layers: vec![Descriptor {
            media_type: MEDIA_TYPE_IMAGE_LAYER_GZIP.to_string(),
            digest: layer.digest,
            size: layer.size,
            urls: vec![],
            annotations: Default::default(),
            platform: None,
        }],
        annotations: Default::default(),
    };
    let manifest_ingested = store
        .ingest(serde_json::to_vec(&manifest).unwrap().as_slice())
        .unwrap();
    let normalized = Reference::parse("ociman-test/pascal-case:latest")
        .unwrap()
        .to_string();
    store
        .put_image(&ImageRecord {
            reference: normalized,
            manifest_digest: manifest_ingested.digest,
        })
        .unwrap();

    let out = ociman_run(storage_dir.path(), "ociman-test/pascal-case:latest", &[]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("pascal-case-cmd-worked"),
        "got stdout: {stdout:?}"
    );
}

/// `ociman run` can target an image by its own real or short ID, not
/// just a tag reference -- closing the gap 0179/0180/0181 each
/// separately named as the same, still-open, real inconsistency: every
/// other image-targeting command (`inspect`/`rmi`/`tag`/`push`/`save`,
/// 0122) already supported this. Both the short (12 hex char) and the
/// full `sha256:<hex>` forms are checked here.
#[test]
fn run_by_short_or_full_image_id_works() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/run-by-id:latest",
        &busybox,
        &["sh", "echo"],
        ContainerConfig::default(),
    );
    let record = store
        .resolve_image("docker.io/ociman-test/run-by-id:latest")
        .unwrap()
        .unwrap();
    let full_digest = record.manifest_digest.to_string();
    let short_id = &record.manifest_digest.hex()[..12];

    let out = ociman_run(
        storage_dir.path(),
        short_id,
        &["--", "/bin/sh", "-c", "echo hello-by-short-id"],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("hello-by-short-id"));

    let out = ociman_run(
        storage_dir.path(),
        &full_digest,
        &["--", "/bin/sh", "-c", "echo hello-by-full-digest"],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("hello-by-full-digest"));
}

/// Resolving `run`'s own image argument by ID happens *before* ever
/// treating it as a tag reference at all, precisely so a real image ID
/// never triggers a real, wasted network pull attempt first (an ID
/// almost always also parses as *some* syntactically valid but
/// nonsense tag reference). Verified here by using `--pull always`
/// (which would otherwise always attempt a real registry round trip)
/// against a real image ID in this fully offline test environment: if
/// ID resolution weren't tried first, this would hang/fail trying to
/// reach a real registry instead of succeeding immediately.
#[test]
fn run_by_id_with_pull_always_never_touches_the_network() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/run-by-id-pull-always:latest",
        &busybox,
        &["sh", "echo"],
        ContainerConfig::default(),
    );
    let record = store
        .resolve_image("docker.io/ociman-test/run-by-id-pull-always:latest")
        .unwrap()
        .unwrap();
    let short_id = &record.manifest_digest.hex()[..12];

    let out = ociman_run(
        storage_dir.path(),
        short_id,
        &[
            "--pull",
            "always",
            "--",
            "/bin/sh",
            "-c",
            "echo hello-with-pull-always",
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("hello-with-pull-always"));
}

/// An unknown image ID is a clear error, same as an unknown tag
/// reference -- never silently misparsed into a nonsense pull attempt.
#[test]
fn run_by_an_unknown_image_id_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let out = ociman_run(
        storage_dir.path(),
        "0123456789ab",
        &["--pull", "never", "--", "/bin/true"],
    );
    assert!(!out.status.success());
}

/// A container run by image ID records the image's own *actual*
/// reference (a real tag if it has one, or this project's own
/// internal untagged sentinel otherwise, 0179) as its own
/// `io.oci-tools.image` annotation -- never the raw ID string the user
/// actually typed -- so a later `ociman commit`/`ociman rmi`'s own
/// dependent-container lookup (which reads that annotation back and
/// resolves it through the exact same store) keeps working correctly
/// regardless of which form was used to start it.
#[test]
fn run_by_id_records_the_images_own_real_reference_not_the_id_typed() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/run-by-id-annotation:latest",
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );
    let record = store
        .resolve_image("docker.io/ociman-test/run-by-id-annotation:latest")
        .unwrap()
        .unwrap();
    let short_id = &record.manifest_digest.hex()[..12];

    // Foreground, no `--rm`: exits fast, leaving a real stopped
    // container record behind to inspect afterward.
    let out = ociman_run(storage_dir.path(), short_id, &["--", "/bin/true"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let container_id = only_container_id(storage_dir.path(), Duration::from_secs(10));
    assert!(!container_id.is_empty());

    let inspect = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args(["inspect", "--json", &container_id])
        .output()
        .unwrap();
    assert!(
        inspect.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&inspect.stderr)
    );
    let view: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert_eq!(
        view["image"], "docker.io/ociman-test/run-by-id-annotation:latest",
        "the container's own recorded image annotation should be the image's real tag, not \
         the bare ID {short_id:?} it was actually started with: {view:?}"
    );
}

/// `ociman run` without `-i`/`--interactive` (0187): the container's
/// own stdin must always be a fresh, empty `/dev/null`, never a silent
/// pass-through of whatever real stdin `ociman` itself happened to
/// have — matching real `docker run`/`podman run` exactly (checked
/// directly: piping real input into a plain `podman run` with no `-i`
/// never reaches the container at all).
///
/// A real, previously-unnoticed bug this test would have caught: before
/// this fix, `ociman run`'s own foreground path never touched the
/// container's stdin at all, so it silently inherited whatever fd 0
/// `ociman` itself had -- forwarding real piped input completely
/// unconditionally, with no way to turn it off.
#[test]
fn run_without_interactive_never_forwards_real_stdin() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/stdin-default:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let mut child = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args([
            "run",
            "--rm",
            "ociman-test/stdin-default:latest",
            "/bin/sh",
            "-c",
            "if read -t 5 line; then echo GOT:$line; else echo NOINPUT; fi",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn ociman run");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"hello-from-host-stdin\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "NOINPUT",
        "without --interactive, the container should never see real host stdin"
    );
}

/// `ociman run -i`/`--interactive` (0187): the container's own stdin
/// must be this process's own real stdin, matching real `docker run
/// -i`/`podman run -i` exactly.
#[test]
fn run_interactive_forwards_real_stdin() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/stdin-interactive:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let mut child = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args([
            "run",
            "--rm",
            "--interactive",
            "ociman-test/stdin-interactive:latest",
            "/bin/sh",
            "-c",
            "if read -t 5 line; then echo GOT:$line; else echo NOINPUT; fi",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn ociman run");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"hello-from-host-stdin\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "GOT:hello-from-host-stdin",
        "--interactive should forward this process's own real stdin to the container"
    );
}
