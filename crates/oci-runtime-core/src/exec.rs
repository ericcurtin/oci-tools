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
}

/// Run `request.args` as a new process inside the already-running
/// container whose init process is `pid`, joining `request.namespaces`
/// and applying `request.user`/`capabilities`/`no_new_privileges`/
/// `cwd`/`env`. Returns the same exit code the exec'd process would
/// report to its own shell, or one of `launch`'s own `*_EXIT_CODE`
/// constants if `oci-tools` itself failed before it ever ran.
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
    let needs_pid_relay = request.namespaces.contains(&NamespaceType::Pid);

    let setup = ExecSetup {
        opened,
        needs_pid_relay,
        user: request.user,
        capabilities: request.capabilities,
        no_new_privileges: request.no_new_privileges,
        cwd: request.cwd,
        env: request.env,
        args: request.args,
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
}

impl ExecSetup {
    /// Join the target container's namespaces, then either `exec`
    /// directly or — if a PID namespace was joined — fork once more
    /// first, for the same reason `launch::ChildSetup::run` does: a
    /// `setns(2)` into a PID namespace never moves the calling process
    /// into it, only a *subsequent* forked child becomes a member.
    fn run(mut self) -> ! {
        let opened = std::mem::take(&mut self.opened);
        if let Err(e) = nsenter::join_all(opened) {
            fail(
                SETUP_FAILURE_EXIT_CODE,
                &format!("joining container namespaces: {e}"),
            );
        }

        if self.needs_pid_relay {
            // SAFETY: this process is still single-threaded (nothing
            // between the last fork and here spawns a thread).
            #[allow(unsafe_code)]
            let inner = unsafe { process::fork(|| self.exec_now()) };
            match inner {
                Ok(child_pid) => match process::wait(child_pid) {
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
        let err = command.exec();
        let code = match err.kind() {
            io::ErrorKind::NotFound => COMMAND_NOT_FOUND_EXIT_CODE,
            io::ErrorKind::PermissionDenied => COMMAND_NOT_EXECUTABLE_EXIT_CODE,
            _ => SETUP_FAILURE_EXIT_CODE,
        };
        fail(code, &format!("exec {}: {err}", self.args[0]));
    }
}

/// Print an error and exit with `code` — same convention `launch`'s own
/// `fail` uses (a separate copy, not shared, since the two modules are
/// otherwise independent and this is a two-line function).
fn fail(code: i32, message: &str) -> ! {
    eprintln!("error: {message}");
    std::process::exit(code);
}
