//! The per-container launcher-keeper process behind `StartContainer`
//! (`docs/design/0238`) — this project's own equivalent of real
//! cri-o's conmon: one small, dedicated process per started
//! container that actually launches it and sticks around to record
//! how it ended.
//!
//! # Why a separate process at all
//!
//! `oci_runtime_core::launch`'s fork-based entry points carry a real
//! "calling process must be single-threaded" safety contract
//! (`process::fork`'s own), which `ocicri`'s multithreaded tokio
//! server can never satisfy directly. So `StartContainer` spawns a
//! *fresh* `ocicri` process instead — `std::process::Command` is a
//! real `fork`+immediate-`exec` (safe from a multithreaded parent),
//! and the fresh child is single-threaded at entry, exactly like
//! `ociman run -d`'s own keeper is at its own fork point. Re-executing
//! the current executable with an internal `__launch` argv is the
//! same technique real `runc` itself uses for its own `runc init`
//! re-exec — a process-model necessity, not a "shell out to an
//! external tool" (this project's own strict shelling-out policy is
//! about the latter).
//!
//! # The tiny on-disk protocol
//!
//! Everything lives in the container's own already-existing bundle
//! directory (0237), all writes atomic (temp file + rename):
//!
//! * `pid` — written the moment the container's real init pid is
//!   known (`run_reporting_pid`'s own `on_pid`); the server's
//!   `StartContainer` waits for exactly this before answering.
//! * `exit.json` — `{exit_code, finished_at_nanos}`, written when the
//!   container actually exits (the launcher's whole reason to stick
//!   around; real conmon's own exit-file equivalent). `exit_code` is
//!   `128 + signal` for a signal death, `oci_runtime_core::process::
//!   exit_code`'s own documented convention.
//! * `start-error` — a human-readable reason, written if the launch
//!   failed before a pid ever existed.
//!
//! The launcher outlives the server on purpose: it's `setsid`-detached
//! and never killed when `ocicri` restarts, so a running container
//! (and its eventual real exit code) survives a server restart —
//! matching real cri-o's own conmon lifetime exactly.

use std::io::Write as _;
use std::path::Path;

/// The argv\[1\] sentinel `main.rs` intercepts before clap/tokio ever
/// run — deliberately un-typeable-looking, matching `runc init`'s own
/// hidden-command spirit (it never appears in `--help`).
pub const LAUNCH_ARGV1: &str = "__launch";

/// File names within the bundle directory (see the module doc
/// comment).
pub const PID_FILENAME: &str = "pid";
pub const EXIT_FILENAME: &str = "exit.json";
pub const START_ERROR_FILENAME: &str = "start-error";

/// What `exit.json` records.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct ExitRecord {
    /// `waitpid`-derived exit code (`128 + signal` for signal death).
    pub exit_code: i32,
    /// When the container actually exited, nanoseconds since epoch.
    pub finished_at_nanos: i64,
}

fn write_atomic(dir: &Path, name: &str, bytes: &[u8]) -> std::io::Result<()> {
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(bytes)?;
    tmp.persist(dir.join(name)).map_err(|e| e.error)?;
    Ok(())
}

/// The `__launch` process's own entire life: launch the bundle's
/// container, report its pid, wait for it, record how it ended.
/// Never returns.
///
/// Exit codes: 0 once the container ran and its exit was recorded
/// (whatever *its* code was — that lands in `exit.json`, not here);
/// 1 for any launch failure (recorded in `start-error` first, so the
/// server never has to parse this process's own stderr).
pub fn main(args: &[String]) -> ! {
    std::process::exit(match run(args) {
        Ok(()) => 0,
        Err(e) => {
            // Best effort: the bundle dir may itself be the problem.
            if let Some(dir) = args.first() {
                let _ = write_atomic(
                    Path::new(dir),
                    START_ERROR_FILENAME,
                    format!("{e:#}").as_bytes(),
                );
            }
            1
        }
    })
}

fn run(args: &[String]) -> anyhow::Result<()> {
    use anyhow::Context as _;

    let [bundle_dir, container_id] = args else {
        anyhow::bail!("usage: ocicri {LAUNCH_ARGV1} <BUNDLE_DIR> <CONTAINER_ID>");
    };
    let dir = Path::new(bundle_dir);

    // Detach from the server's own session/process group, so a
    // Ctrl+C (or service stop) delivered to the server's own group
    // never takes running containers down with it -- the same
    // detachment `ociman`'s own keeper performs, for the same reason.
    // Failure (already a session leader, unlikely) is tolerated.
    let _ = rustix::process::setsid();

    let bundle = oci_runtime_core::Bundle::load(dir)
        .with_context(|| format!("loading bundle from {}", dir.display()))?;
    let rootfs =
        oci_runtime_core::validate::validate(&bundle).context("config.json failed validation")?;

    let dir_for_pid = dir.to_path_buf();
    // SAFETY: this process is genuinely single-threaded here -- it
    // was just exec'd fresh (see the module doc comment), and nothing
    // above spawns a thread (argv parsing, one file read, setsid).
    // The same contract `ocirun run`/`ociman`'s keeper already
    // uphold at their own call sites.
    #[allow(unsafe_code)]
    let exit_code = unsafe {
        oci_runtime_core::launch::run_reporting_pid(
            container_id,
            &bundle,
            &rootfs,
            None,
            // The same systemd transient scope `ociman run` gives
            // every container (and the same graceful "no D-Bus
            // reachable" fallback) -- also what ocicri's own
            // `RuntimeConfig` RPC already tells kubelet this project
            // uses. One scope per container, never reused: a CRI
            // container is started at most once (kubelet restarts
            // mean a *new* container with attempt+1, never a
            // second start of this one).
            oci_runtime_core::launch::CgroupSetup::Systemd {
                scope_name: format!("ocicri-{container_id}.scope"),
                description: format!("ocicri container {container_id}"),
                resources: None,
            },
            // No attach/interactive concept at this layer (real CRI
            // streaming attach is its own future RPC); output capture
            // to the CRI log path is a documented later increment.
            true,
            true,
            |pid| {
                let _ = write_atomic(&dir_for_pid, PID_FILENAME, pid.to_string().as_bytes());
            },
        )
    }
    .context("launching container")?;

    let exit = ExitRecord {
        exit_code,
        finished_at_nanos: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0),
    };
    write_atomic(dir, EXIT_FILENAME, &serde_json::to_vec_pretty(&exit)?)
        .context("writing exit record")?;
    Ok(())
}

/// Reads a container's exit record, if its launcher has written one
/// yet. A missing file is `None`; a malformed one is a real error.
pub fn read_exit(bundle_dir: &Path) -> anyhow::Result<Option<ExitRecord>> {
    match std::fs::read(bundle_dir.join(EXIT_FILENAME)) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Reads the launcher's start-error, if it recorded one.
pub fn read_start_error(bundle_dir: &Path) -> Option<String> {
    std::fs::read_to_string(bundle_dir.join(START_ERROR_FILENAME)).ok()
}

/// Reads the container's pid, if its launcher has reported one yet.
pub fn read_pid(bundle_dir: &Path) -> Option<i32> {
    std::fs::read_to_string(bundle_dir.join(PID_FILENAME))
        .ok()
        .and_then(|s| s.trim().parse().ok())
}
