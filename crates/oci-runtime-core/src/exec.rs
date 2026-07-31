//! Running an *additional* process inside an already-running container
//! (`ocirun exec`) — unlike [`crate::launch`]'s `create`/`run`, which
//! only ever create a container's *first* process in brand-new
//! namespaces, `exec` joins the target container's *existing*
//! namespaces (via [`crate::nsenter`]) and does no rootfs setup at all
//! (the container's mount namespace already has its rootfs
//! `pivot_root`ed from when it was created) — otherwise applying the
//! same identity/capability drop [`crate::launch`] does for a
//! container's own init process, then `exec`ing.

use std::io;
use std::os::fd::{AsRawFd as _, FromRawFd as _};
use std::os::unix::process::CommandExt as _;
use std::path::Path;

use oci_spec_types::runtime::{LinuxCapabilities, NamespaceType, User};

use crate::identity;
use crate::launch::{
    COMMAND_NOT_EXECUTABLE_EXIT_CODE, COMMAND_NOT_FOUND_EXIT_CODE, SETUP_FAILURE_EXIT_CODE,
};
use crate::nsenter;
use crate::process;

/// Everything [`exec`] needs to know about the new process to run
/// inside the target container — bundled into one value both to keep
/// [`exec`]'s own argument list manageable and because it's exactly
/// what gets moved into the forked child's closure as a single
/// capture (the same shape [`crate::launch`]'s own `ChildSetup` uses).
pub struct ExecRequest {
    /// Namespaces to join — the same list the target container's own
    /// bundle declared at `create`/`run` time.
    pub namespaces: Vec<NamespaceType>,
    /// The identity to drop to before `exec`ing (typically the target
    /// container's own `process.user`, unless overridden).
    pub user: User,
    /// Capability sets to apply (typically the target container's own
    /// `process.capabilities`).
    pub capabilities: Option<LinuxCapabilities>,
    /// Whether to set `PR_SET_NO_NEW_PRIVS` before `exec`ing.
    pub no_new_privileges: bool,
    /// Working directory for the new process, relative to the
    /// container's own rootfs.
    pub cwd: String,
    /// `NAME=value` environment variables for the new process.
    pub env: Vec<String>,
    /// Executable and arguments (exec form; index 0 is the executable).
    pub args: Vec<String>,
    /// `ocirun exec --preserve-fds N` (0294): the number of extra file
    /// descriptors, starting at fd 3 (right after stdio), that this
    /// process's own caller has already arranged to have open and
    /// wants passed through into the exec'd process untouched --
    /// matching real `runc exec`/`crun exec --preserve-fds` exactly,
    /// the same real flag/default `crate::launch::run`/`create`
    /// already implement (`0291`) for the *first* process a container
    /// runs; this is its identical counterpart for an *additional*
    /// one. `0` (every caller except `ocirun`'s own CLI) closes every
    /// fd above stdio before the exec'd process ever runs, matching
    /// real runc/crun's own identical default -- the same real,
    /// previously-missing fd-leak gap `0291` closed for `run`/
    /// `create` also existed here, independently, until now.
    pub preserve_fds: u32,
    /// A real deadline for this exec'd process (0308) — `None` (every
    /// caller except `ociman healthcheck run`) waits forever, matching
    /// this function's own pre-existing behavior exactly. `Some(d)`
    /// kills (`SIGKILL`) and reaps the process if it hasn't exited on
    /// its own within `d`, matching real `docker`/`podman healthcheck
    /// run`'s own "a hung check counts as unhealthy" semantics: the
    /// resulting exit code is the same `128 + SIGKILL` a shell would
    /// report for any other signal-killed process (see
    /// [`process::exit_code_from_wait_status`]), which every existing
    /// caller of this function already treats as "nonzero, not
    /// healthy" with no code changes needed downstream.
    pub timeout: Option<std::time::Duration>,
    /// If true, the exec'd process's own stdin is a fresh `/dev/null`
    /// instead of whatever fd 0 this calling process itself already
    /// has — matching real `podman exec`'s own checked-directly
    /// default exactly (`~/git/podman/cmd/podman/containers/exec.go`:
    /// `AttachInput`/`InputStream` are only ever set when `-i`/
    /// `--interactive` is given; `AttachOutput`/`AttachError` are
    /// unconditional either way, so stdout/stderr are deliberately
    /// *not* gated by this field — matching that same real asymmetry).
    /// Purely a podman-level concept: neither real `runc exec` nor
    /// `crun exec` has any `-i`/interactive flag at all (checked
    /// directly against both installed binaries' own `--help`) —
    /// `ocirun exec`'s own call site always passes `false` here,
    /// matching `crate::launch::run`'s own identical "no attach/
    /// interactive concept, always forward whatever stdio the caller
    /// already set up verbatim" precedent for `ocirun run`/`create`
    /// (0187).
    pub close_stdin: bool,
}

/// Run `request.args` as a new process inside the already-running
/// container whose init process is `pid`, joining `request.namespaces`
/// and applying `request.user`/`capabilities`/`no_new_privileges`/
/// `cwd`/`env`. Returns the same exit code the exec'd process would
/// report to its own shell, one of `launch`'s own `*_EXIT_CODE`
/// constants if `oci-tools` itself failed before it ever ran, or (if
/// `request.timeout` was given and elapsed first) the same `128 +
/// SIGKILL` code a shell reports for any other signal-killed process
/// — see [`ExecRequest::timeout`]'s own doc comment.
///
/// # Safety
///
/// Must be called from a single-threaded process — this forks (see
/// [`crate::process::fork_and_wait`]'s safety note, which this
/// inherits).
#[allow(unsafe_code)]
pub unsafe fn exec(pid: i32, request: ExecRequest) -> io::Result<i32> {
    if request.args.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "exec: no command given",
        ));
    }
    // Opened *before* the fork below, in this (the calling, `ocirun
    // exec` CLI) process's own original namespaces — see `nsenter`'s
    // own doc comment on why joining anything first would make some
    // of these paths unreadable.
    let opened = nsenter::open_all(pid, &request.namespaces)?;
    // Opened here too, for the same reason `nsenter::open_all` just
    // above is: a plain `/dev/null` by-path open must happen before
    // ever joining any namespace this process doesn't already have
    // (in particular a mount namespace whose own `/dev/null` might
    // not even exist, or resolve to something else entirely) — see
    // `launch::run_reporting_pid`'s own identical "open before the
    // fork, in the original process" ordering for `close_stdin`.
    let stdin_fd = if request.close_stdin {
        Some(std::fs::File::open("/dev/null")?)
    } else {
        None
    };
    // A real `timeout` (0308) also needs the inner-fork relay, even
    // when no PID namespace join is otherwise required: the deadline-
    // aware wait lives entirely inside `ExecSetup::run`'s own relay
    // branch (see its doc comment), so that's the one place a timeout
    // can actually be enforced regardless of which namespaces are
    // joined.
    let needs_pid_relay =
        request.namespaces.contains(&NamespaceType::Pid) || request.timeout.is_some();

    let setup = ExecSetup {
        opened,
        needs_pid_relay,
        user: request.user,
        capabilities: request.capabilities,
        no_new_privileges: request.no_new_privileges,
        cwd: request.cwd,
        env: request.env,
        args: request.args,
        preserve_fds: request.preserve_fds,
        timeout: request.timeout,
        stdin_fd,
    };

    // SAFETY: forwarded from this function's own contract.
    #[allow(unsafe_code)]
    let status = unsafe { process::fork_and_wait(move || setup.run()) }?;
    Ok(process::exit_code_from_wait_status(status))
}

struct ExecSetup {
    opened: Vec<nsenter::OpenNamespace>,
    needs_pid_relay: bool,
    user: User,
    capabilities: Option<LinuxCapabilities>,
    no_new_privileges: bool,
    cwd: String,
    env: Vec<String>,
    args: Vec<String>,
    preserve_fds: u32,
    timeout: Option<std::time::Duration>,
    /// See [`ExecRequest::close_stdin`]'s own doc comment. A real,
    /// already-open `/dev/null` handle (not just a bool), opened once
    /// in the original process before any fork — the same reasoning
    /// `launch::ChildSetup::stdin_fd` already established.
    stdin_fd: Option<std::fs::File>,
}

impl ExecSetup {
    /// Join the target container's namespaces, then either `exec`
    /// directly or — if a PID namespace was joined (or a `timeout` was
    /// given, see [`exec`]'s own doc comment for why that also routes
    /// through here) — fork once more first, for the same reason
    /// `launch::ChildSetup::run` does: a `setns(2)` into a PID
    /// namespace never moves the calling process into it, only a
    /// *subsequent* forked child becomes a member.
    fn run(mut self) -> ! {
        let opened = std::mem::take(&mut self.opened);
        if let Err(e) = nsenter::join_all(opened) {
            fail(
                SETUP_FAILURE_EXIT_CODE,
                &format!("joining container namespaces: {e}"),
            );
        }

        if self.needs_pid_relay {
            let timeout = self.timeout;
            // SAFETY: this process is still single-threaded (nothing
            // between the last fork and here spawns a thread).
            #[allow(unsafe_code)]
            let inner = unsafe { process::fork(|| self.exec_now()) };
            match inner {
                Ok(child_pid) => match wait_with_deadline(child_pid, timeout) {
                    Ok(status) => std::process::exit(process::exit_code_from_wait_status(status)),
                    Err(e) => fail(
                        SETUP_FAILURE_EXIT_CODE,
                        &format!("waiting for exec'd process: {e}"),
                    ),
                },
                Err(e) => fail(
                    SETUP_FAILURE_EXIT_CODE,
                    &format!("forking into the joined pid namespace: {e}"),
                ),
            }
        } else {
            self.exec_now();
        }
    }

    /// Apply identity, then `exec`. Never returns: a successful `exec`
    /// replaces the process image outright, and any failure prints an
    /// error and exits with a matching code (see [`fail`]).
    fn exec_now(&self) -> ! {
        if let Err(e) = identity::apply(
            Path::new("/proc"),
            &self.user,
            self.capabilities.as_ref(),
            self.no_new_privileges,
        ) {
            fail(SETUP_FAILURE_EXIT_CODE, &format!("applying identity: {e}"));
        }

        let mut command = std::process::Command::new(&self.args[0]);
        command.args(&self.args[1..]);
        command.current_dir(&self.cwd);
        command.env_clear();
        for kv in &self.env {
            if let Some((key, value)) = kv.split_once('=') {
                command.env(key, value);
            }
        }
        // `close_stdin` (see `ExecRequest::close_stdin`'s own doc
        // comment): `self` is only ever a shared reference here, so
        // the `File` can't be moved out of it directly; reconstructing
        // a fresh `Stdio` from the same raw fd number is sound for the
        // exact same reason `launch::ChildSetup::mount_pivot_and_exec`'s
        // own identical call site already documents -- this process
        // never uses `self.stdin_fd` again, and always terminates from
        // here on by either a successful `exec` (replacing the process
        // image, reclaiming every fd via ordinary kernel process
        // teardown) or `fail`'s own `std::process::exit` (same
        // reclaiming).
        #[allow(unsafe_code)]
        if let Some(fd) = &self.stdin_fd {
            command.stdin(unsafe { std::process::Stdio::from_raw_fd(fd.as_raw_fd()) });
        }
        // Close every fd above stdio (+ any explicitly `--preserve-
        // fds`d ones), matching real runc/crun's own identical
        // default -- see `ExecRequest::preserve_fds`'s own doc
        // comment. A `pre_exec` closure -- not a plain call right here
        // -- specifically because it must run *after* `Command`'s own
        // internal stdio `dup2`s (already registered above) but
        // *before* the real `execve`: the raw source fd behind
        // `stdin_fd` is itself an ordinary fd `>= 3` that this same
        // cleanup would otherwise close *before* `Command` ever got a
        // chance to `dup2` it onto fd 0, breaking `--interactive`
        // outright the moment both are combined.
        let preserve_fds = self.preserve_fds;
        #[allow(unsafe_code)]
        unsafe {
            command.pre_exec(move || process::close_fds_ge_than(3 + preserve_fds));
        }
        let err = command.exec();
        let code = match err.kind() {
            io::ErrorKind::NotFound => COMMAND_NOT_FOUND_EXIT_CODE,
            io::ErrorKind::PermissionDenied => COMMAND_NOT_EXECUTABLE_EXIT_CODE,
            _ => SETUP_FAILURE_EXIT_CODE,
        };
        fail(code, &format!("exec {}: {err}", self.args[0]));
    }
}

/// [`process::wait`], but killing (`SIGKILL`) and reaping `pid` if
/// `timeout` elapses first (0308) — the same "poll, kill + reap on
/// deadline" shape `hooks::wait_with_timeout` already established for
/// a hook's own `Timeout` (via `std::process::Child::try_wait`); this
/// is its equivalent for a bare, `fork`ed pid, via
/// [`process::try_wait`]. `None` waits forever, identical to this
/// function's own pre-existing behavior before `--timeout` existed at
/// all.
fn wait_with_deadline(pid: i32, timeout: Option<std::time::Duration>) -> io::Result<i32> {
    let Some(timeout) = timeout else {
        return process::wait(pid);
    };
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(status) = process::try_wait(pid)? {
            return Ok(status);
        }
        if std::time::Instant::now() >= deadline {
            let _ = process::kill(pid, libc::SIGKILL);
            // The kill above guarantees it exits very soon (if it
            // hasn't already); a final, ordinary blocking wait reaps
            // it without needing to poll any further.
            return process::wait(pid);
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Print an error and exit with `code` — same convention `launch`'s own
/// `fail` uses (a separate copy, not shared, since the two modules are
/// otherwise independent and this is a two-line function).
fn fail(code: i32, message: &str) -> ! {
    eprintln!("error: {message}");
    std::process::exit(code);
}
