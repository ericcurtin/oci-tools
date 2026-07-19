//! Assembling namespaces, rootless ID mapping, the rootfs setup sequence,
//! and process execution into an actual container: create-and-start
//! ("run") in one step, the same shape as runc/crun's own `run`
//! subcommand — as opposed to the separate `create`+`start` two-phase
//! lifecycle (which needs a persistent background process surviving
//! after the CLI invocation returns, and is not implemented yet; see
//! `docs/design/`).
//!
//! Every piece this module assembles was already built and independently
//! verified in earlier increments: [`crate::namespaces`] (`unshare` +
//! rootless ID mapping), [`crate::rootfs`] (the mount/pivot_root/hostname
//! sequence), [`crate::process`] (`fork`/`waitpid`), and
//! [`oci_mount::syscalls`] (the actual `mount(2)`/`pivot_root(2)` calls).
//! This module is where they meet a real bundle for the first time.

use std::io;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};

use oci_spec_types::runtime::{
    LinuxCapabilities, LinuxIdMapping, LinuxSeccomp, NamespaceType, PosixRlimit, User,
};

use crate::bundle::Bundle;
use crate::cgroups::{self, CgroupWrite};
use crate::exec_fifo;
use crate::identity;
use crate::namespaces;
use crate::process;
use crate::rlimits;
use crate::rootfs::{self, MaskedPathKind, RootfsAction};
use crate::seccomp;

/// The cgroup v2 unified hierarchy's mount point in production. Tests
/// substitute a plain temp directory (see [`crate::cgroups`]'s own
/// doc comment on why plain file writes don't need a real cgroupfs to
/// exercise this logic).
const CGROUP_ROOT: &str = "/sys/fs/cgroup";

/// Exit code used when oci-tools itself (not the container's process)
/// fails to set the container up — matches the Docker/podman convention
/// (125 = the tool itself errored, distinct from 126/127 below for the
/// container's own command being unexecutable/not found).
pub const SETUP_FAILURE_EXIT_CODE: i32 = 125;
/// Exit code used when the container's command exists but could not be
/// executed (e.g. wrong permissions) — matches the Docker/podman/`sh`
/// convention.
pub const COMMAND_NOT_EXECUTABLE_EXIT_CODE: i32 = 126;
/// Exit code used when the container's command was not found — matches
/// the Docker/podman/`sh` convention.
pub const COMMAND_NOT_FOUND_EXIT_CODE: i32 = 127;

/// Create and start `bundle`'s container in one step (fork, `unshare`,
/// rootless ID mapping if needed, the planned rootfs setup, then `exec`
/// the container's process), and wait for it to exit.
///
/// Returns the same exit code the container's own process would report
/// to its own shell (0-255, or `128 + signal` if a signal killed it), or
/// one of the `*_EXIT_CODE` constants above if oci-tools itself failed
/// before the container's process ever ran (every such failure is logged
/// to stderr before that happens).
///
/// # Safety
///
/// Must be called from a single-threaded process — this forks (see
/// [`crate::process::fork_and_wait`]'s safety note, which this inherits).
#[allow(unsafe_code)]
pub unsafe fn run(bundle: &Bundle, rootfs: &Path) -> io::Result<i32> {
    // SAFETY: forwarded from this function's own contract.
    unsafe { run_reporting_pid(bundle, rootfs, |_pid| {}) }
}

/// Like [`run`], but calls `on_pid` with the container's own pid as
/// soon as it's known — before the container's process has necessarily
/// finished setup or started running the user's command — rather than
/// only ever returning the final exit code once everything is over.
///
/// For callers that need a live pid a *concurrent* invocation can act
/// on (`ociman run`, unlike `ocirun run`, persists a container record
/// other `ociman` commands look at while this one is still foreground
/// — see `docs/design/0023`); `run` itself is just this with a no-op
/// callback, so ordinary callers pay only the cost of one extra pipe
/// and a 4-byte read, not a behavioral difference.
///
/// # Safety
///
/// Same contract as [`run`].
#[allow(unsafe_code)]
pub unsafe fn run_reporting_pid(
    bundle: &Bundle,
    rootfs: &Path,
    on_pid: impl FnOnce(i32),
) -> io::Result<i32> {
    let mut child_setup = build_child_setup(bundle, rootfs)?;
    let (read_fd, write_fd) =
        rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC).map_err(io::Error::from)?;
    child_setup.pid_pipe_write = Some(write_fd);

    // SAFETY: forwarded from this function's own contract. Unlike
    // `create`, this direct child's own pid *is* what gets waited on
    // below — no relay-fork subtlety here, since `ChildSetup::run`
    // already relays the grandchild's exit status as its own when one
    // happens (see its own doc comment), so waiting on the direct
    // child always yields the right final status regardless.
    #[allow(unsafe_code)]
    let direct_child_pid = unsafe { process::fork(move || child_setup.run()) }?;

    let container_pid = read_container_pid(read_fd)?;
    on_pid(container_pid);

    let status = process::wait(direct_child_pid)?;
    Ok(process::exit_code_from_wait_status(status))
}

/// Create `bundle`'s container: fork and run the exact same setup
/// sequence [`run`] does (`unshare`, rootless ID mapping, cgroup entry,
/// the planned rootfs setup, dropping to the spec's `process.user`/
/// capabilities/`no_new_privileges`, applying seccomp), but — instead of
/// `exec`ing immediately — leave the container's own init process
/// blocked on `exec_fifo_path` (see [`crate::exec_fifo`]) until a
/// separate `start` unblocks it, and return to the caller as soon as
/// that setup finishes, **without** waiting for the container to
/// actually start running.
///
/// Returns the pid of the container's own init process: the pid
/// namespace's pid 1 if one was requested (not necessarily this
/// function's own direct child — see [`ChildSetup::run`]'s relay-fork
/// note), reported back over an internal pipe so the caller always
/// gets the *real* container pid to persist, regardless of whether a
/// relay fork happened.
///
/// Leaves the container's init process running in the background once
/// this function returns: nothing here waits for it, so once the
/// calling process (e.g. `ocirun create`'s own CLI invocation) exits,
/// the kernel reparents it to the nearest subreaper/init, same as any
/// other backgrounded Unix process — no extra double-fork/
/// daemonization step needed.
///
/// # Safety
///
/// Same contract as [`run`].
#[allow(unsafe_code)]
pub unsafe fn create(bundle: &Bundle, rootfs: &Path, exec_fifo_path: &Path) -> io::Result<i32> {
    let mut child_setup = build_child_setup(bundle, rootfs)?;
    let (read_fd, write_fd) =
        rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC).map_err(io::Error::from)?;
    child_setup.exec_fifo = Some(exec_fifo_path.to_path_buf());
    child_setup.pid_pipe_write = Some(write_fd);

    // SAFETY: forwarded from this function's own contract. The direct
    // child's own pid is deliberately not what this function returns —
    // see the doc comment above.
    #[allow(unsafe_code)]
    let _direct_child_pid = unsafe { process::fork(move || child_setup.run()) }?;

    read_container_pid(read_fd)
}

/// Block until the container process (or its relay, if the pid
/// namespace's real init is a grandchild) reports the real container
/// pid over the pipe [`create`] set up, or report why it never did
/// (setup failed before reaching that point — the failure itself was
/// already printed to stderr by the child, same as [`run`]'s failures).
fn read_container_pid(read_fd: rustix::fd::OwnedFd) -> io::Result<i32> {
    let mut buf = [0u8; 4];
    let mut filled = 0;
    while filled < buf.len() {
        let n = rustix::io::read(&read_fd, &mut buf[filled..]).map_err(io::Error::from)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "container process exited before reporting its pid (setup likely failed)",
            ));
        }
        filled += n;
    }
    Ok(i32::from_ne_bytes(buf))
}

/// Everything [`run`] and [`create`] need identically: compute
/// namespace flags, resolve the cgroup directory and planned resource
/// writes, and plan the rootfs setup sequence, bundled into a
/// [`ChildSetup`] with `exec_fifo`/`pid_pipe_write` left unset (`run`'s
/// shape); [`create`] fills those in afterward.
fn build_child_setup(bundle: &Bundle, rootfs: &Path) -> io::Result<ChildSetup> {
    let process_spec = bundle
        .spec
        .process
        .as_ref()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "config has no process"))?;
    if process_spec.args.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "process.args is empty",
        ));
    }

    let namespaces_list: &[_] = bundle.spec.linux.as_ref().map_or(&[], |l| &l.namespaces);
    let flags = namespaces::clone_flags_for(namespaces_list);
    // A user namespace with no join `path` was created by us, not joined
    // from an existing one, so it's ours to map. (A join-path user
    // namespace has already had its mapping established by whoever
    // created it — validate::validate rejects a config that has neither
    // a path nor mappings, so this crate never has to guess.)
    let needs_self_mapping = namespaces_list
        .iter()
        .any(|ns| ns.kind == NamespaceType::User && ns.path.is_none());
    let uid_mappings: Vec<LinuxIdMapping> = bundle
        .spec
        .linux
        .as_ref()
        .map_or(&[][..], |l| &l.uid_mappings)
        .to_vec();
    let gid_mappings: Vec<LinuxIdMapping> = bundle
        .spec
        .linux
        .as_ref()
        .map_or(&[][..], |l| &l.gid_mappings)
        .to_vec();

    let linux = bundle.spec.linux.as_ref();
    let cgroup_dir = cgroups::directory_for(
        Path::new(CGROUP_ROOT),
        linux.and_then(|l| l.cgroups_path.as_deref()),
    )?;
    let cgroup_writes = linux
        .and_then(|l| l.resources.as_ref())
        .map(cgroups::plan_resources)
        .unwrap_or_default();

    let plan = rootfs::plan_rootfs_setup(bundle, rootfs);
    Ok(ChildSetup {
        flags,
        needs_self_mapping,
        uid_mappings,
        gid_mappings,
        cgroup_dir,
        cgroup_writes,
        plan,
        user: process_spec.user.clone(),
        capabilities: process_spec.capabilities.clone(),
        no_new_privileges: process_spec.no_new_privileges,
        rlimits: process_spec.rlimits.clone(),
        seccomp: linux.and_then(|l| l.seccomp.clone()),
        exec_fifo: None,
        pid_pipe_write: None,
        args: process_spec.args.clone(),
        env: process_spec.env.clone(),
        cwd: process_spec.cwd.clone(),
    })
}

/// Everything the forked child needs to `unshare`, map IDs, run the
/// planned rootfs setup, and finally `exec` — bundled into one value so
/// it can move into the child's closure as a single capture.
struct ChildSetup {
    flags: rustix::thread::UnshareFlags,
    needs_self_mapping: bool,
    uid_mappings: Vec<LinuxIdMapping>,
    gid_mappings: Vec<LinuxIdMapping>,
    cgroup_dir: Option<PathBuf>,
    cgroup_writes: Vec<CgroupWrite>,
    plan: Vec<RootfsAction>,
    user: User,
    capabilities: Option<LinuxCapabilities>,
    no_new_privileges: bool,
    rlimits: Vec<PosixRlimit>,
    seccomp: Option<LinuxSeccomp>,
    /// Set by [`create`] (left `None` by [`run`], which `exec`s
    /// immediately with no synchronization needed): path to block on
    /// before `exec`ing, until a separate `start` unblocks it.
    exec_fifo: Option<PathBuf>,
    /// Set by [`create`]: the write end of a pipe this reports the real
    /// container pid over, for the top-level [`create`] call (which
    /// isn't necessarily this struct's own direct caller — see
    /// [`ChildSetup::run`]'s relay-fork note) to read back.
    pid_pipe_write: Option<rustix::fd::OwnedFd>,
    args: Vec<String>,
    env: Vec<String>,
    cwd: String,
}

impl ChildSetup {
    /// Run the whole child-side sequence: `unshare`, map IDs, then either
    /// run the rootfs setup and `exec` directly, or — if a new PID
    /// namespace was requested — fork *once more* first.
    ///
    /// That second fork matters because `unshare(CLONE_NEWPID)` does not
    /// move the calling process into the new PID namespace at all; only
    /// its *next forked child* becomes a member (and specifically becomes
    /// that namespace's pid 1). Mounting a fresh `/proc` — which the
    /// rootfs setup sequence does for every default bundle — requires the
    /// mounting process to actually own the pid namespace it reflects,
    /// which the process that merely called `unshare` never does. Caught
    /// by actually running this against a real kernel: the very first
    /// attempt at this function (without the relay fork) failed
    /// `mount("proc", ...)` with `EPERM` every time a PID namespace was
    /// requested, exactly as `unshare(2)`'s own documentation says it
    /// should.
    ///
    /// When a relay fork happens, *this* process (not the grandchild)
    /// exits with the same status the container's own process produced,
    /// so whoever is waiting on it (the outer [`run`]) sees that status
    /// either way.
    fn run(&self) -> ! {
        // Applied before `unshare`, matching real crun: rlimits are a
        // plain process attribute with no interaction with namespaces
        // or the rootless ID-mapping dance, and raising one above its
        // current hard limit (if ever needed) is only guaranteed to
        // work with whatever privilege this process has *before*
        // becoming a fake-root-in-a-userns.
        if let Err(e) = rlimits::apply(&self.rlimits) {
            fail(SETUP_FAILURE_EXIT_CODE, &format!("setting rlimits: {e}"));
        }
        // Also strictly before `unshare`: entering the target cgroup
        // *before* a `CLONE_NEWCGROUP` unshare is what makes that
        // namespace root at the container's own cgroup rather than
        // whatever the host process was in — see `cgroups::enter`'s own
        // doc comment.
        if let Some(dir) = &self.cgroup_dir {
            if let Err(e) = std::fs::create_dir_all(dir) {
                fail(
                    SETUP_FAILURE_EXIT_CODE,
                    &format!("creating cgroup directory {}: {e}", dir.display()),
                );
            }
            if let Err(e) = cgroups::apply(dir, &self.cgroup_writes) {
                fail(
                    SETUP_FAILURE_EXIT_CODE,
                    &format!("applying cgroup resources: {e}"),
                );
            }
            if let Err(e) = cgroups::enter(dir) {
                fail(SETUP_FAILURE_EXIT_CODE, &format!("entering cgroup: {e}"));
            }
        }
        if let Err(e) = namespaces::unshare(self.flags) {
            fail(SETUP_FAILURE_EXIT_CODE, &format!("unshare: {e}"));
        }
        if self.needs_self_mapping
            && let Err(e) = namespaces::write_id_mappings(
                Path::new("/proc"),
                "self",
                &self.uid_mappings,
                &self.gid_mappings,
            )
        {
            fail(
                SETUP_FAILURE_EXIT_CODE,
                &format!("writing id mappings: {e}"),
            );
        }

        if self.flags.contains(rustix::thread::UnshareFlags::NEWPID) {
            // SAFETY: this process is still single-threaded (nothing
            // between the last fork and here spawns a thread).
            #[allow(unsafe_code)]
            let grandchild_pid = unsafe { process::fork(|| self.mount_pivot_and_exec()) };
            let grandchild_pid = match grandchild_pid {
                Ok(pid) => pid,
                Err(e) => fail(
                    SETUP_FAILURE_EXIT_CODE,
                    &format!("forking container pid 1: {e}"),
                ),
            };
            // Report the *real* container pid (the grandchild, not this
            // relay process's own pid) before blocking on it — `create`
            // needs this pid available long before the grandchild might
            // ever exit.
            self.report_container_pid(grandchild_pid);
            match process::wait(grandchild_pid) {
                Ok(status) => std::process::exit(process::exit_code_from_wait_status(status)),
                Err(e) => fail(
                    SETUP_FAILURE_EXIT_CODE,
                    &format!("waiting for container pid 1: {e}"),
                ),
            }
        } else {
            // SAFETY: `getpid()` has no safety requirements.
            let own_pid = rustix::process::getpid().as_raw_nonzero().get();
            self.report_container_pid(own_pid);
            self.mount_pivot_and_exec();
        }
    }

    /// Report `pid` (the container's real init pid) to whoever is
    /// reading [`Self::pid_pipe_write`] (only set by [`create`]; a
    /// no-op for [`run`], which has no need to report a pid anywhere).
    /// Best-effort: if the write fails there's nothing more useful to
    /// do than let the reader's own end see EOF/an error.
    fn report_container_pid(&self, pid: i32) {
        if let Some(fd) = &self.pid_pipe_write {
            let _ = rustix::io::write(fd, &pid.to_ne_bytes());
        }
    }

    /// Run the planned rootfs setup, then `exec` the container's
    /// process. Never returns: a successful `exec` replaces the process
    /// image outright, and any failure along the way prints an error and
    /// exits with a matching code (see [`fail`]).
    fn mount_pivot_and_exec(&self) -> ! {
        // Must happen *before* `pivot_root` below (part of the plan
        // loop) — see `exec_fifo`'s own doc comment on why an ordinary
        // by-path open afterward would resolve against the container's
        // new root instead of wherever the fifo actually lives.
        let exec_fifo_fd = self.exec_fifo.as_deref().map(|path| {
            exec_fifo::open_path(path).unwrap_or_else(|e| {
                fail(SETUP_FAILURE_EXIT_CODE, &format!("opening exec fifo: {e}"))
            })
        });

        for action in &self.plan {
            if let Err(e) = execute_rootfs_action(action) {
                fail(SETUP_FAILURE_EXIT_CODE, &format!("{action:?}: {e}"));
            }
        }

        if let Err(e) = identity::apply(
            Path::new("/proc"),
            &self.user,
            self.capabilities.as_ref(),
            self.no_new_privileges,
        ) {
            fail(SETUP_FAILURE_EXIT_CODE, &format!("applying identity: {e}"));
        }

        // Last of all, right before exec: matches real crun (seccomp is
        // applied after the uid/gid/capability drop), and means a
        // rejected profile (see `seccomp`'s own doc comment on its
        // scope limits) still fails loudly before the container's
        // command ever runs, rather than running unfiltered.
        if let Some(profile) = &self.seccomp
            && let Err(e) = seccomp::apply(profile)
        {
            fail(SETUP_FAILURE_EXIT_CODE, &format!("applying seccomp: {e}"));
        }

        // Absolute last step before exec, matching real runc (which
        // applies seccomp "as close to execve as possible" for the same
        // reason, then only *afterward* waits on its own equivalent of
        // this fifo): a `create`-only wait for `start` to unblock this
        // process. `run` never sets `exec_fifo`, so this is a no-op for
        // it.
        if let Some(fd) = &exec_fifo_fd
            && let Err(e) = exec_fifo::wait_for_start(fd)
        {
            fail(SETUP_FAILURE_EXIT_CODE, &format!("waiting for start: {e}"));
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
        // `exec` only returns (as an `Err`) if it failed; on success the
        // process image is replaced and this line never returns at all.
        let err = command.exec();
        let code = match err.kind() {
            io::ErrorKind::NotFound => COMMAND_NOT_FOUND_EXIT_CODE,
            io::ErrorKind::PermissionDenied => COMMAND_NOT_EXECUTABLE_EXIT_CODE,
            _ => SETUP_FAILURE_EXIT_CODE,
        };
        fail(code, &format!("exec {}: {err}", self.args[0]));
    }
}

/// Print an error and exit with `code`. Used for every failure path in a
/// forked child that has not yet (and, after this, never will) `exec`.
fn fail(code: i32, message: &str) -> ! {
    eprintln!("error: {message}");
    std::process::exit(code);
}

/// Perform one planned rootfs-setup step for real.
fn execute_rootfs_action(action: &RootfsAction) -> io::Result<()> {
    match action {
        RootfsAction::MakeMountsPrivate => rustix::mount::mount_change(
            "/",
            rustix::mount::MountPropagationFlags::PRIVATE
                | rustix::mount::MountPropagationFlags::REC,
        )
        .map_err(io::Error::from),
        RootfsAction::BindRootfsOntoItself { rootfs } => {
            rustix::mount::mount_bind_recursive(rootfs, rootfs).map_err(io::Error::from)
        }
        RootfsAction::Mount {
            target,
            source,
            file_system_type,
            parsed,
        } => {
            std::fs::create_dir_all(target)?;
            match oci_mount::mount(
                source.as_deref(),
                target,
                file_system_type.as_deref(),
                parsed,
            ) {
                Err(e)
                    if file_system_type.as_deref() == Some("cgroup2")
                        && matches!(
                            e.kind(),
                            io::ErrorKind::PermissionDenied | io::ErrorKind::ResourceBusy
                        ) =>
                {
                    // Known rootless limitation: when /sys is a
                    // recursive bind of the host's (rather than a fresh
                    // sysfs, which rootless mode can't mount — see
                    // Spec::into_rootless), /sys/fs/cgroup already
                    // reflects the host's real cgroup2 mount as part of
                    // that same recursive bind, making a separate,
                    // explicit cgroup2 mount there either redundant
                    // (`EBUSY`, something is already mounted there) or
                    // disallowed (`EPERM`). Either way there is nothing
                    // more to do: the container already sees a real
                    // cgroup2 hierarchy at this path. Warn and continue
                    // rather than fail the whole container.
                    tracing::warn!(target = %target.display(), error = %e, "cgroup2 mount failed (tolerated)");
                    Ok(())
                }
                other => other,
            }
        }
        RootfsAction::BindMount { source, target } => {
            if source.is_file() {
                if !target.exists() {
                    std::fs::write(target, b"")?;
                }
            } else {
                std::fs::create_dir_all(target)?;
            }
            let parsed = oci_mount::parse_mount_options(&["rbind"]);
            oci_mount::mount(Some(&source.to_string_lossy()), target, None, &parsed)
        }
        RootfsAction::RemountReadonly { target } => {
            let parsed = oci_mount::parse_mount_options(&["remount", "ro", "rbind"]);
            match oci_mount::mount(None, target, None, &parsed) {
                Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                    // Known rootless limitation (see docs/design/0010):
                    // remounting a bind-mount of a host filesystem (e.g.
                    // /sys) read-only can require CAP_SYS_ADMIN in the
                    // namespace that owns the *original* superblock,
                    // which a fake-root-in-a-userns does not have. Warn
                    // and continue rather than fail the whole container.
                    tracing::warn!(target = %target.display(), error = %e, "remount read-only failed (tolerated)");
                    Ok(())
                }
                other => other,
            }
        }
        RootfsAction::MaskPath { target } => match rootfs::classify_masked_path(target) {
            MaskedPathKind::File => {
                let parsed = oci_mount::parse_mount_options(&["rbind"]);
                oci_mount::mount(Some("/dev/null"), target, None, &parsed)
            }
            MaskedPathKind::Directory => {
                std::fs::create_dir_all(target)?;
                let parsed = oci_mount::parse_mount_options(&["ro"]);
                oci_mount::mount(Some("tmpfs"), target, Some("tmpfs"), &parsed)
            }
            MaskedPathKind::Missing => Ok(()),
        },
        RootfsAction::PivotRoot { new_root, put_old } => {
            std::fs::create_dir_all(put_old)?;
            oci_mount::pivot_root(new_root, put_old)
        }
        RootfsAction::UnmountOldRoot { put_old } => {
            let name = put_old.file_name().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "put_old has no filename")
            })?;
            let target = Path::new("/").join(name);
            rustix::mount::unmount(&target, rustix::mount::UnmountFlags::DETACH)
                .map_err(io::Error::from)?;
            let _ = std::fs::remove_dir(&target);
            Ok(())
        }
        RootfsAction::ChangeDirectoryToRoot => std::env::set_current_dir("/"),
        RootfsAction::SetHostname(name) => {
            rustix::system::sethostname(name.as_bytes()).map_err(io::Error::from)
        }
    }
}
