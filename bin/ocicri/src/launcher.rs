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

/// The `ExecSync` helper's own argv\[1\] sentinel (`docs/design/0240`)
/// — same interception, same single-threaded-at-entry reasoning as
/// [`LAUNCH_ARGV1`]: `oci_runtime_core::exec::exec` forks
/// (`fork_and_wait`), which the tokio server can never do directly.
pub const EXEC_ARGV1: &str = "__exec";

/// The exit code [`exec_main`] reports when the exec *setup itself*
/// failed before the command ever ran (bundle unreadable, `setns`
/// denied, ...) — the shell's own conventional "command invoked
/// cannot execute" code, with the real reason on stderr (which the
/// server already captures and returns verbatim in
/// `ExecSyncResponse.stderr`, so a kubelet probe sees both a failing
/// code and a human-readable why). A real command exiting 126 on its
/// own is indistinguishable, exactly as it is for a real shell.
pub const EXEC_SETUP_FAILED_CODE: i32 = 126;

/// The `__exec` process's own entire life (`docs/design/0240`): join
/// the running container of `<PID>` exactly like `ociman exec` does —
/// the same shared `oci_runtime_core::exec` machinery, the same
/// "everything comes from the target's own bundle" defaults
/// (namespaces/user/capabilities/no-new-privileges/cwd/env; the CRI
/// `ExecSyncRequest` has no per-call overrides for any of these) —
/// run `<CMD...>`, and exit with its code. stdout/stderr are simply
/// inherited: the server spawns this helper with real pipes and
/// captures both directly, no on-disk protocol needed (unlike
/// [`main`]'s launcher-keeper, nothing here outlives the RPC).
/// Never returns.
pub fn exec_main(args: &[String]) -> ! {
    std::process::exit(match exec_run(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("ocicri {EXEC_ARGV1}: {e:#}");
            EXEC_SETUP_FAILED_CODE
        }
    })
}

fn exec_run(args: &[String]) -> anyhow::Result<i32> {
    use anyhow::Context as _;

    let [bundle_dir, pid, cmd @ ..] = args else {
        anyhow::bail!("usage: ocicri {EXEC_ARGV1} <BUNDLE_DIR> <PID> <CMD...>");
    };
    anyhow::ensure!(!cmd.is_empty(), "exec command cannot be empty");
    let pid: i32 = pid.parse().context("parsing target pid")?;
    wait_until_execed(pid)?;

    // Own process group (`setsid`), so the server can kill this whole
    // helper *tree* (this process plus the forked, namespace-joined
    // exec child below) with one negative-pid SIGKILL on timeout --
    // `setns` changes namespaces, never process-group membership, so
    // the group stays whole even after the child joins the container.
    let _ = rustix::process::setsid();

    let bundle = oci_runtime_core::Bundle::load(Path::new(bundle_dir))
        .with_context(|| format!("loading bundle from {bundle_dir}"))?;
    let process_spec = bundle
        .spec
        .process
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("bundle has no process section"))?;
    let namespaces: Vec<_> = bundle
        .spec
        .linux
        .as_ref()
        .map_or(&[][..], |l| &l.namespaces)
        .iter()
        .map(|ns| ns.kind)
        .collect();

    let request = oci_runtime_core::exec::ExecRequest {
        namespaces,
        user: process_spec.user.clone(),
        capabilities: process_spec.capabilities.clone(),
        no_new_privileges: process_spec.no_new_privileges,
        cwd: process_spec.cwd.clone(),
        env: process_spec.env.clone(),
        args: cmd.to_vec(),
    };

    // SAFETY: this process is genuinely single-threaded here -- just
    // exec'd fresh (see `main`'s identical note; nothing above spawns
    // a thread).
    #[allow(unsafe_code)]
    let exit_code = unsafe { oci_runtime_core::exec::exec(pid, request) }
        .context("executing inside container")?;
    Ok(exit_code)
}

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

    let (bundle_dir, container_id, log_path) = match args {
        [bundle_dir, container_id] => (bundle_dir, container_id, None),
        [bundle_dir, container_id, log_path] => (bundle_dir, container_id, Some(log_path)),
        _ => anyhow::bail!("usage: ocicri {LAUNCH_ARGV1} <BUNDLE_DIR> <CONTAINER_ID> [LOG_PATH]"),
    };
    let dir = Path::new(bundle_dir);

    // Detach from the server's own session/process group, so a
    // Ctrl+C (or service stop) delivered to the server's own group
    // never takes running containers down with it -- the same
    // detachment `ociman`'s own keeper performs, for the same reason.
    // Failure (already a session leader, unlikely) is tolerated.
    let _ = rustix::process::setsid();

    // CRI logging (`docs/design/0242`): when kubelet gave this
    // container a log path, the container's own stdout/stderr become
    // real pipes into a dedicated logger process -- this project's
    // own version of the other half of conmon's job (the first half,
    // keeping the exit code, is this process itself). Set up
    // *before* `run_reporting_pid`'s own fork, while this process is
    // still single-threaded: the logger is a forked process, never a
    // thread, for exactly that reason.
    let discard_output = if let Some(log_path) = log_path {
        setup_cri_logging(Path::new(log_path)).context("setting up CRI logging")?;
        false // The container inherits the pipe fds now on 1/2.
    } else {
        true // No log path: discard, as before.
    };

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
            // streaming attach is its own future RPC). Output goes to
            // the CRI log pipes when a log path was given (fds 1/2
            // are already the pipes by now -- see `setup_cri_logging`)
            // and is discarded otherwise.
            true,
            discard_output,
            |pid| {
                let _ = write_atomic(&dir_for_pid, PID_FILENAME, pid.to_string().as_bytes());
            },
        )
    }
    .context("launching container")?;

    // Release this process's own copies of the log pipes (fds 1/2)
    // *before* recording the exit, so the logger sees EOF and
    // finishes the log file no later than the exit becomes visible --
    // a reader acting on the recorded exit never races a
    // still-incomplete log.
    if log_path.is_some()
        && let Ok(devnull) = std::fs::OpenOptions::new().write(true).open("/dev/null")
    {
        let _ = rustix::stdio::dup2_stdout(&devnull);
        let _ = rustix::stdio::dup2_stderr(&devnull);
    }

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

/// The Kubernetes CRI logging format, one line:
/// `<RFC3339Nano> <stream> <P|F> <content>\n` — checked against real
/// conmon's own output and the kubelet parser's fixtures
/// (`2016-10-06T00:17:09.669794202Z stdout F log content`). `F` is a
/// full (newline-terminated) line; `P` a partial one (cut by a
/// too-long line or EOF), which the reader reassembles.
fn format_cri_log_line(timestamp: &str, stream: &str, complete: bool, content: &[u8]) -> Vec<u8> {
    let tag = if complete { "F" } else { "P" };
    let mut line = Vec::with_capacity(timestamp.len() + stream.len() + content.len() + 8);
    line.extend_from_slice(timestamp.as_bytes());
    line.push(b' ');
    line.extend_from_slice(stream.as_bytes());
    line.push(b' ');
    line.extend_from_slice(tag.as_bytes());
    line.push(b' ');
    line.extend_from_slice(content);
    line.push(b'\n');
    line
}

/// A single line longer than this is cut into `P` (partial) entries —
/// real conmon's own `STDIO_BUF_SIZE`-driven behavior (it emits a
/// partial entry whenever a read fills its buffer without a newline).
const CRI_LOG_MAX_LINE: usize = 8192;

/// Copies one stream's pipe into CRI-format lines. Runs on its own
/// thread inside the logger process (which, unlike this launcher at
/// its own fork points, is free to spawn threads — it never forks).
fn copy_stream_as_cri(
    pipe: std::os::fd::OwnedFd,
    stream: &str,
    file: &std::sync::Mutex<std::fs::File>,
) {
    use std::io::{Read, Write};
    let mut reader = std::io::BufReader::new(std::fs::File::from(pipe));
    let mut pending: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        pending.extend_from_slice(&chunk[..n]);
        // Emit complete lines up to the cap; anything longer gets cut
        // into `P` chunks *before* its own eventual newline is even
        // looked at (checked in that order deliberately -- with bytes
        // accumulating across reads, a newline can arrive after the
        // cap is already exceeded, and the cap must still win).
        loop {
            let newline_at = pending.iter().position(|&b| b == b'\n');
            match newline_at {
                Some(at) if at <= CRI_LOG_MAX_LINE => {
                    let rest = pending.split_off(at + 1);
                    pending.pop(); // The newline itself isn't content.
                    let ts = oci_spec_types::format_rfc3339_nanos_utc(std::time::SystemTime::now());
                    if let Ok(mut f) = file.lock() {
                        let _ = f.write_all(&format_cri_log_line(&ts, stream, true, &pending));
                    }
                    pending = rest;
                }
                _ if pending.len() >= CRI_LOG_MAX_LINE => {
                    let rest = pending.split_off(CRI_LOG_MAX_LINE);
                    let ts = oci_spec_types::format_rfc3339_nanos_utc(std::time::SystemTime::now());
                    if let Ok(mut f) = file.lock() {
                        let _ = f.write_all(&format_cri_log_line(&ts, stream, false, &pending));
                    }
                    pending = rest;
                }
                _ => break,
            }
        }
    }
    // EOF with an unterminated tail: a real partial entry, exactly
    // what the CRI format's own `P` tag exists for.
    if !pending.is_empty() {
        let ts = oci_spec_types::format_rfc3339_nanos_utc(std::time::SystemTime::now());
        if let Ok(mut f) = file.lock() {
            let _ = f.write_all(&format_cri_log_line(&ts, stream, false, &pending));
        }
    }
}

/// Wires this (still single-threaded) launcher's own fds 1/2 to fresh
/// pipes and forks the logger process that turns them into a real
/// CRI-format log file at `log_path` — so the container, which
/// inherits 1/2 through `run_reporting_pid`, streams straight into
/// the logger with no thread ever existing in *this* process before
/// its own container fork.
///
/// The logger: closes its own inherited copies of the write ends
/// (else it would never see EOF), creates the log file (and parent
/// directories — kubelet's own `<sandbox log_directory>/<container
/// log_path>` routinely has a `<name>/` subdirectory), and drains
/// both streams until EOF, which arrives once the container *and*
/// this launcher have both let go (the launcher does so explicitly
/// right before recording the exit).
fn setup_cri_logging(log_path: &Path) -> anyhow::Result<()> {
    use anyhow::Context as _;

    let (stdout_read, stdout_write) =
        rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC).context("creating pipe")?;
    let (stderr_read, stderr_write) =
        rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC).context("creating pipe")?;

    // Wire this launcher's own fds 1/2 to the write ends *first*
    // (then drop the originals -- 1/2 themselves keep the pipes
    // open), so the fork below only needs to move the read ends into
    // the logger's closure and the parent needs nothing back from it.
    rustix::stdio::dup2_stdout(&stdout_write).context("wiring stdout pipe")?;
    rustix::stdio::dup2_stderr(&stderr_write).context("wiring stderr pipe")?;
    drop(stdout_write);
    drop(stderr_write);

    let log_path = log_path.to_path_buf();
    // SAFETY: single-threaded (see this function's own doc comment) --
    // the same contract every other fork in this file already
    // documents.
    #[allow(unsafe_code)]
    unsafe {
        oci_runtime_core::process::fork(move || {
            // Release this logger's own inherited copies of the write
            // ends (its fds 1/2, wired just above in the parent) --
            // holding them would mean never seeing EOF on the reads.
            if let Ok(devnull) = std::fs::OpenOptions::new().write(true).open("/dev/null") {
                let _ = rustix::stdio::dup2_stdout(&devnull);
                let _ = rustix::stdio::dup2_stderr(&devnull);
            }
            if let Some(parent) = log_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let Ok(file) = std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&log_path)
            else {
                std::process::exit(1);
            };
            let file = std::sync::Mutex::new(file);
            std::thread::scope(|scope| {
                scope.spawn(|| copy_stream_as_cri(stdout_read, "stdout", &file));
                copy_stream_as_cri(stderr_read, "stderr", &file);
            });
            std::process::exit(0);
        })
    }
    .context("forking the CRI logger")?;
    Ok(())
}

/// Blocks until the container process has genuinely `exec`'d its own
/// command — a real race found the hard way (`docs/design/0240`): the
/// launcher's own pid file is written the moment the container pid
/// *exists* (`on_pid`, before the child has finished its rootfs
/// setup and exec'd), and 0238 already documented that `RUNNING` is
/// therefore reported pre-exec. An exec that joins the target's
/// namespaces inside that window sees a half-set-up world (pre-pivot
/// mount namespace, pre-exec argv at pid 1 — both actually observed
/// in this project's own test suite as a real flake before this
/// gate). Real runc doesn't have the window at exactly this point
/// because its own `create` completes all setup before pausing at
/// the start fifo; the equivalent safe point here is "the target's
/// own `/proc/<pid>/cmdline` no longer carries this binary's own
/// pre-exec argv", which flips at exactly the `execve` that ends
/// setup — cheap, dependency-free, and checked against nothing but
/// the kernel's own accounting.
fn wait_until_execed(pid: i32) -> anyhow::Result<()> {
    let own_exe = std::env::current_exe()
        .map(|p| p.into_os_string().into_encoded_bytes())
        .unwrap_or_default();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let cmdline = match std::fs::read(format!("/proc/{pid}/cmdline")) {
            Ok(bytes) => bytes,
            Err(_) => anyhow::bail!("container process {pid} exited before exec"),
        };
        // An empty cmdline is a zombie: it exited already.
        anyhow::ensure!(
            !cmdline.is_empty(),
            "container process {pid} exited before exec"
        );
        // Pre-exec, argv[0] is this same ocicri binary (the launcher
        // re-exec that forked it); post-exec it's the container's own
        // command.
        if own_exe.is_empty() || !cmdline.starts_with(&own_exe) {
            return Ok(());
        }
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "container process {pid} never finished starting (still in pre-exec setup)"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cri_log_lines_have_the_exact_documented_shape() {
        assert_eq!(
            format_cri_log_line(
                "2016-10-06T00:17:09.669794202Z",
                "stdout",
                true,
                b"log content"
            ),
            b"2016-10-06T00:17:09.669794202Z stdout F log content\n"
        );
        assert_eq!(
            format_cri_log_line(
                "2016-10-06T00:17:09.669794202Z",
                "stderr",
                false,
                b"partial"
            ),
            b"2016-10-06T00:17:09.669794202Z stderr P partial\n"
        );
    }

    /// `copy_stream_as_cri` against a real pipe: complete lines get
    /// `F` entries, an oversize line is cut into `P` chunks plus its
    /// terminated tail, and an unterminated EOF tail becomes a final
    /// `P` — the exact reassembly contract kubelet's own log parser
    /// expects.
    #[test]
    fn copy_stream_splits_full_partial_and_oversize_lines() {
        use std::io::Write as _;

        let (read, write) = rustix::pipe::pipe().unwrap();
        let file = tempfile::NamedTempFile::new().unwrap();
        let sink = std::sync::Mutex::new(file.reopen().unwrap());

        let mut writer = std::fs::File::from(write);
        let long = vec![b'x'; CRI_LOG_MAX_LINE + 5];
        writer.write_all(b"hello\n").unwrap();
        writer.write_all(&long).unwrap();
        writer.write_all(b"\n").unwrap();
        writer.write_all(b"tail-without-newline").unwrap();
        drop(writer); // EOF.

        copy_stream_as_cri(read, "stdout", &sink);

        let contents = std::fs::read_to_string(file.path()).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 4, "{contents:?}");
        let fields: Vec<Vec<&str>> = lines.iter().map(|l| l.splitn(4, ' ').collect()).collect();
        // Every line: RFC3339Nano timestamp, stream, tag, content.
        for f in &fields {
            assert_eq!(f[1], "stdout");
            assert!(f[0].ends_with('Z') && f[0].contains('.'), "{f:?}");
        }
        assert_eq!((fields[0][2], fields[0][3]), ("F", "hello"));
        // The oversize line: one P chunk of exactly the cutoff, then
        // the terminated remainder as F.
        assert_eq!(fields[1][2], "P");
        assert_eq!(fields[1][3].len(), CRI_LOG_MAX_LINE);
        assert_eq!((fields[2][2], fields[2][3]), ("F", "xxxxx"));
        // The unterminated EOF tail.
        assert_eq!((fields[3][2], fields[3][3]), ("P", "tail-without-newline"));
    }
}
