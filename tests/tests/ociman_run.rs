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
    let scope_name = format!("ociman-{container_id}.scope");

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
    let scope_name = format!("ociman-{container_id}.scope");

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
    let scope_name = format!("ociman-{container_id}.scope");

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
    let scope_name = format!("ociman-{container_id}.scope");

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
