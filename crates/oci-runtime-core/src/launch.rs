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

use std::io::{self, Read as _, Write as _};
use std::os::fd::{AsRawFd as _, FromRawFd as _};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};

use oci_spec_types::runtime::{
    LinuxCapabilities, LinuxIdMapping, LinuxResources, LinuxSeccomp, NamespaceType, PosixRlimit,
    User,
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
use crate::systemd_cgroup;

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
pub unsafe fn run(id: &str, bundle: &Bundle, rootfs: &Path) -> io::Result<i32> {
    // SAFETY: forwarded from this function's own contract.
    unsafe { run_reporting_pid(id, bundle, rootfs, None, CgroupSetup::FromSpec, |_pid| {}) }
}

/// How a container's cgroup gets set up — see `docs/design/0033` and
/// `crate::systemd_cgroup` for why a second driver exists at all.
pub enum CgroupSetup {
    /// Whatever `bundle.spec.linux.cgroupsPath`/`resources` already
    /// specify (the cgroupfs driver this crate has had since 0015, or
    /// no cgroup at all if `cgroupsPath` is unset) — the *only* mode
    /// `ocirun` itself uses, via plain [`run`], matching real
    /// `runc`/`crun`'s own spec-driven behavior exactly.
    FromSpec,
    /// Ignore `cgroupsPath` entirely: create a transient systemd scope
    /// instead (`systemd_cgroup::create_scope`), named `scope_name`
    /// with `description`, translating `resources` (if any) into
    /// systemd unit properties (see
    /// `systemd_cgroup::resource_properties`) rather than dropping them
    /// — see `docs/design/0037`. Falls back to no cgroup at all (logged
    /// via `tracing::warn!`, not fatal) if no D-Bus session is
    /// reachable — matches this project's own "tolerate known rootless
    /// limitations" pattern used elsewhere (e.g. a rootless `/sys`
    /// remount failure).
    Systemd {
        /// Must end in `.scope` — see
        /// `systemd_cgroup::create_scope`'s own doc comment.
        scope_name: String,
        /// Free-form text systemd stores as the unit's own
        /// `Description=`.
        description: String,
        /// The same `LinuxResources` the cgroupfs driver
        /// (`CgroupSetup::FromSpec`) would otherwise translate into
        /// raw file writes — translated into systemd unit properties
        /// instead here, so both drivers honor the same limits for
        /// the same spec. Boxed: `LinuxResources` is large enough
        /// (several nested structs) that an unboxed `Option` here
        /// would make every `CgroupSetup::FromSpec` value pay for
        /// space it never uses (clippy's own `large_enum_variant`).
        resources: Option<Box<LinuxResources>>,
    },
}

/// Like [`run`], but calls `on_pid` with the container's own pid as
/// soon as it's known — before the container's process has necessarily
/// finished setup or started running the user's command — rather than
/// only ever returning the final exit code once everything is over,
/// and (if `log_path` is given) also captures the container's own
/// stdout/stderr to that file as it runs, in addition to this
/// process's own stdout — see [`setup_log_tee_pipe`]'s doc comment for
/// how and why.
///
/// For callers that need a live pid a *concurrent* invocation can act
/// on (`ociman run`, unlike `ocirun run`, persists a container record
/// other `ociman` commands look at while this one is still foreground
/// — see `docs/design/0023`); `run` itself is just this with a no-op
/// callback and no log path, so ordinary callers pay only the cost of
/// one extra pipe and a 4-byte read, not a behavioral difference.
///
/// Also runs `bundle.spec.hooks`'s `poststart` (right after `on_pid`)
/// and `poststop` (right after the container exits) hooks, if any are
/// configured — see `docs/design/0026` for why only those two of the
/// six real hook points are executed here. A failing hook is logged
/// and tolerated, never changes the container's own exit code: a
/// broken notify/cleanup script isn't a reason to report the
/// container itself as having failed.
///
/// # Safety
///
/// Same contract as [`run`].
#[allow(unsafe_code)]
pub unsafe fn run_reporting_pid(
    id: &str,
    bundle: &Bundle,
    rootfs: &Path,
    log_path: Option<&Path>,
    cgroup_setup: CgroupSetup,
    on_pid: impl FnOnce(i32),
) -> io::Result<i32> {
    let mut child_setup = build_child_setup(bundle, rootfs)?;
    let (read_fd, write_fd) =
        rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC).map_err(io::Error::from)?;
    child_setup.pid_pipe_write = Some(write_fd);

    // Only the pipe(s) are created here, deliberately not yet a reader
    // thread for the log one below: spawning a thread before the fork
    // a few lines down would make *this* process multi-threaded right
    // at the moment it forks, which `process::fork`'s own safety
    // contract forbids (see its doc comment) — the thread has to wait
    // until after the fork returns here.
    let log_read_fd = log_path
        .map(|_| setup_log_tee_pipe(&mut child_setup))
        .transpose()?;

    // If the systemd driver is in use, the spec's own `cgroupsPath`-
    // derived plan (if any) is entirely superseded, and the child
    // needs a way to pause until this process has actually finished
    // trying to migrate it into a real cgroup over D-Bus — see
    // `CgroupSetup::Systemd`'s own doc comment and `docs/design/0033`.
    let cgroup_ready_write = match &cgroup_setup {
        CgroupSetup::FromSpec => None,
        CgroupSetup::Systemd { .. } => {
            child_setup.cgroup_dir = None;
            child_setup.cgroup_writes = Vec::new();
            let (ready_read, ready_write) =
                rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC)
                    .map_err(io::Error::from)?;
            child_setup.cgroup_ready_read = Some(ready_read);
            Some(ready_write)
        }
    };

    // Same shape as the systemd cgroup driver's own readiness pipe just
    // above: only actually created when there's a real `prestart`/
    // `createRuntime` hook to run, so an ordinary container (no bundle
    // this project's own benchmark has ever used sets these) pays
    // nothing beyond the one `Option` check both here and in
    // `ChildSetup::mount_pivot_and_exec`.
    let needs_pre_pivot_hooks = bundle
        .spec
        .hooks
        .as_ref()
        .is_some_and(|h| !h.prestart.is_empty() || !h.create_runtime.is_empty());
    let hooks_ready_write = if needs_pre_pivot_hooks {
        let (ready_read, ready_write) =
            rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC).map_err(io::Error::from)?;
        child_setup.hooks_ready_read = Some(ready_read);
        Some(ready_write)
    } else {
        None
    };

    // SAFETY: forwarded from this function's own contract. Unlike
    // `create`, this direct child's own pid *is* what gets waited on
    // below — no relay-fork subtlety here, since `ChildSetup::run`
    // already relays the grandchild's exit status as its own when one
    // happens (see its own doc comment), so waiting on the direct
    // child always yields the right final status regardless.
    #[allow(unsafe_code)]
    let direct_child_pid = unsafe { process::fork(move || child_setup.run()) }?;

    // Attempt the D-Bus migration for the *direct* child — not
    // `container_pid` below, which isn't known yet at this point, and
    // critically must not be waited for here either: the direct
    // child's own `cgroup_ready_read` wait happens *before* it does
    // any pid-namespace relay-forking of its own (see
    // `ChildSetup::run`'s doc comment on ordering), which is also
    // *before* it ever reports a container pid over the separate
    // pid-reporting pipe below — waiting for `container_pid` first
    // would deadlock against the child waiting for this signal first.
    // Migrating the direct child specifically still correctly covers
    // the eventual real container pid too (the grandchild in the
    // pid-namespace case): cgroup membership is inherited across
    // `fork`, exactly like the cgroupfs driver's own migration
    // (`cgroups::enter`, called by the same direct child before any of
    // its own further forking) already relies on.
    if let CgroupSetup::Systemd {
        scope_name,
        description,
        resources,
    } = &cgroup_setup
    {
        if let Err(e) = systemd_cgroup::create_scope(
            direct_child_pid as u32,
            scope_name,
            description,
            resources.as_deref(),
        ) {
            tracing::warn!(
                scope = %scope_name,
                error = %e,
                "systemd cgroup driver unavailable (tolerated, container has no cgroup)"
            );
        }
        if let Some(write_fd) = &cgroup_ready_write {
            // Best-effort: if this write somehow fails, the child's own
            // read below still eventually unblocks on `EOF` once this
            // function returns and drops `write_fd`.
            let _ = rustix::io::write(write_fd, b"\0");
        }
    }

    // Safe to spawn the reader thread now: the fork this process is
    // ever going to do for this container has already happened.
    let log_thread = match (log_read_fd, log_path) {
        (Some(read_fd), Some(path)) => Some(spawn_log_tee_thread(read_fd, path)?),
        _ => None,
    };

    let container_pid = read_container_pid(read_fd)?;

    // The container's own process is now paused right where the real
    // spec requires `prestart`/`createRuntime` to run (see
    // `ChildSetup::hooks_ready_read`'s own doc comment): namespaces
    // exist, `pivot_root` hasn't happened yet. Unlike `poststart`/
    // `poststop` below, a failure here is **fatal** — matching real
    // `crun`'s own `do_hooks` call sites for these two hook points,
    // which abort container creation outright rather than merely
    // logging and continuing (see `docs/design/0035`).
    if let Some(write_fd) = &hooks_ready_write {
        if let Err(e) = run_pre_pivot_hooks(bundle, id, container_pid) {
            // The container process is still blocked on its own read of
            // the very pipe `write_fd` is the other end of — it will
            // never receive the "go" byte now, so it must be killed
            // outright rather than left to hang forever.
            let _ = process::kill(container_pid, libc::SIGKILL);
            let _ = process::wait(direct_child_pid);
            if let Some(thread) = log_thread {
                let _ = thread.join();
            }
            remove_cgroup_directory_if_any(bundle);
            return Err(e);
        }
        // Best-effort: if this write somehow fails, the child's own
        // read still eventually unblocks on `EOF` once this function
        // returns and drops `write_fd`.
        let _ = rustix::io::write(write_fd, b"\0");
    }

    on_pid(container_pid);
    run_lifecycle_hooks(bundle, id, container_pid, "running", &Hook::Poststart);

    let status = process::wait(direct_child_pid)?;
    if let Some(thread) = log_thread {
        // Best-effort: once the container itself has finished, a
        // panic in the tee thread (which would only ever come from a
        // poisoned stdout lock, not from anything this container did)
        // isn't a reason to fail the whole `run`.
        let _ = thread.join();
    }
    remove_cgroup_directory_if_any(bundle);
    run_lifecycle_hooks(bundle, id, 0, "stopped", &Hook::Poststop);
    Ok(process::exit_code_from_wait_status(status))
}

/// Remove `bundle`'s own cgroup directory (the same one
/// `build_child_setup` computed and the container's own process
/// migrated into), now that it has exited and left it empty — see
/// [`cgroups::remove`]'s own doc comment for why this is necessary at
/// all (the kernel does not do it on its own) and tolerant of races.
/// A bundle with no `cgroupsPath` set has nothing to remove; a failure
/// is logged and tolerated, the same "don't fail the container over
/// cleanup" reasoning `run_lifecycle_hooks` already applies.
fn remove_cgroup_directory_if_any(bundle: &Bundle) {
    let Ok(Some(dir)) = cgroups::directory_for(
        Path::new(CGROUP_ROOT),
        bundle
            .spec
            .linux
            .as_ref()
            .and_then(|l| l.cgroups_path.as_deref()),
    ) else {
        return;
    };
    if let Err(e) = cgroups::remove(&dir) {
        tracing::warn!(cgroup = %dir.display(), error = %e, "removing cgroup directory (tolerated)");
    }
}

/// Which of the two implemented hook points [`run_lifecycle_hooks`] is
/// running — just selects which list off `bundle.spec.hooks` to use
/// and whether a failing hook should still let the rest run
/// ([`crate::hooks::run`]'s own `keep_going`).
enum Hook {
    Poststart,
    Poststop,
}

/// Run `bundle.spec.hooks`'s `poststart`/`poststop` list (see [`Hook`])
/// with `id`/`pid`/`status` folded into the state JSON piped to each
/// hook's stdin (see `crate::hooks::HookState`'s own doc comment for
/// exactly which fields and why). A failure is logged via
/// `tracing::warn!` and otherwise ignored — see this function's own
/// caller's doc comment for why that's deliberate, not an oversight.
fn run_lifecycle_hooks(bundle: &Bundle, id: &str, pid: i32, status: &str, which: &Hook) {
    let Some(hooks) = &bundle.spec.hooks else {
        return;
    };
    let (list, keep_going, name) = match which {
        Hook::Poststart => (&hooks.poststart, false, "poststart"),
        Hook::Poststop => (&hooks.poststop, true, "poststop"),
    };
    if list.is_empty() {
        return;
    }
    let state = crate::hooks::HookState {
        oci_version: &bundle.spec.version,
        id,
        status,
        pid,
        bundle: bundle.path.display().to_string(),
        annotations: bundle.spec.annotations.clone(),
    };
    let state_json = match serde_json::to_vec(&state) {
        Ok(json) => json,
        Err(e) => {
            tracing::warn!(hook = name, error = %e, "serializing hook state (tolerated)");
            return;
        }
    };
    if let Err(e) = crate::hooks::run(list, &state_json, keep_going) {
        tracing::warn!(hook = name, error = %e, "lifecycle hook failed (tolerated)");
    }
}

/// Run `bundle.spec.hooks`'s `prestart` list, then its `createRuntime`
/// list (in that order, matching real `crun`'s own `do_hooks` call
/// sites — checked directly against
/// `~/git/crun/src/libcrun/container.c`, not the spec prose alone),
/// each with `keep_going: false` (a failing hook stops the rest of its
/// own list) and status `"created"` (matching the spec: both hook
/// points run as part of the `create` operation, before the container
/// has ever actually started). If `prestart` fails, `createRuntime`
/// never runs at all — same as `crun`'s own early `goto fail`.
///
/// Unlike [`run_lifecycle_hooks`]'s `poststart`/`poststop`, a failure
/// here is propagated to the caller rather than merely logged: these
/// two hook points exist specifically to let a hook reject or
/// reconfigure the container *before* it actually starts (e.g. a CNI
/// plugin failing to set up networking), so silently continuing would
/// defeat their entire purpose. See `docs/design/0035`.
fn run_pre_pivot_hooks(bundle: &Bundle, id: &str, pid: i32) -> io::Result<()> {
    let Some(hooks) = &bundle.spec.hooks else {
        return Ok(());
    };
    if hooks.prestart.is_empty() && hooks.create_runtime.is_empty() {
        return Ok(());
    }
    let state = crate::hooks::HookState {
        oci_version: &bundle.spec.version,
        id,
        status: "created",
        pid,
        bundle: bundle.path.display().to_string(),
        annotations: bundle.spec.annotations.clone(),
    };
    let state_json = serde_json::to_vec(&state)
        .map_err(|e| io::Error::other(format!("serializing hook state: {e}")))?;
    crate::hooks::run(&hooks.prestart, &state_json, false)
        .map_err(|e| io::Error::other(format!("prestart hook failed: {e}")))?;
    crate::hooks::run(&hooks.create_runtime, &state_json, false)
        .map_err(|e| io::Error::other(format!("createRuntime hook failed: {e}")))?;
    Ok(())
}

/// Wire `child_setup`'s stdout and stderr to two ends of a fresh pipe
/// (both streams combined into one — this doesn't yet distinguish
/// which produced which byte, see `docs/design/0025`), returning the
/// read end for [`spawn_log_tee_thread`] to consume once it's safe to
/// spawn a thread (i.e. after the fork in [`run_reporting_pid`], not
/// before).
///
/// The two write ends are stored as genuine `OwnedFd`s (moved wholesale
/// into `child_setup`, exactly like `pid_pipe_write` already is), not
/// plain numbers: that's what makes *this* process's own copies of
/// them actually get closed once `process::fork` returns here (the
/// closure — and everything it captured, including these two fds —
/// gets dropped normally when `fork`'s own stack frame unwinds in the
/// parent, since the parent branch never calls it). Without that, this
/// process would itself retain the pipe's write end open forever,
/// and its own reader thread would never see EOF even after the
/// container has fully exited — caught by exactly that hang the first
/// time this was tried with plain `RawFd`s taken via `into_raw_fd`
/// (which does not close anything, unlike letting `OwnedFd`'s own
/// `Drop` run).
fn setup_log_tee_pipe(child_setup: &mut ChildSetup) -> io::Result<rustix::fd::OwnedFd> {
    let (read_fd, write_fd) =
        rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC).map_err(io::Error::from)?;
    let write_fd_2 = rustix::io::dup(&write_fd).map_err(io::Error::from)?;
    child_setup.stdout_log_fd = Some(write_fd);
    child_setup.stderr_log_fd = Some(write_fd_2);
    Ok(read_fd)
}

/// Spawn the background thread that actually implements the "tee":
/// copy everything written to `read_fd` (the container's combined
/// stdout/stderr, see [`setup_log_tee_pipe`]) to both this process's
/// own stdout (so a foreground `run` still shows live output, exactly
/// as it did before logging existed) and appended to `log_path`.
///
/// Must only be called after the fork that (indirectly, via
/// `ChildSetup`) handed the *other* end of this same pipe to the
/// container — see [`run_reporting_pid`]'s own fork-safety note on why.
fn spawn_log_tee_thread(
    read_fd: rustix::fd::OwnedFd,
    log_path: &Path,
) -> io::Result<std::thread::JoinHandle<()>> {
    let mut log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    let mut reader = std::fs::File::from(read_fd);
    Ok(std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let mut stdout = io::stdout();
            let _ = stdout.write_all(&buf[..n]);
            let _ = stdout.flush();
            let _ = log_file.write_all(&buf[..n]);
        }
    }))
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
        stdout_log_fd: None,
        stderr_log_fd: None,
        cgroup_ready_read: None,
        hooks_ready_read: None,
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
    /// Set by [`run_reporting_pid`] (via [`setup_log_tee_pipe`]) when a
    /// log path was given: the write end of the pipe whose read end a
    /// background thread in the *caller's* process tees to both a log
    /// file and its own stdout. `None` for every other caller (`create`
    /// never sets these; plain [`run`] leaves them `None` too). A real
    /// `OwnedFd`, not a plain number, for the same reason
    /// [`Self::pid_pipe_write`] is: see [`setup_log_tee_pipe`]'s own
    /// doc comment.
    stdout_log_fd: Option<rustix::fd::OwnedFd>,
    /// Same pipe as [`Self::stdout_log_fd`] (a second, `dup`'d write
    /// end of the very same one) — both streams are combined, not kept
    /// separate, see `docs/design/0025`.
    stderr_log_fd: Option<rustix::fd::OwnedFd>,
    /// Set only when [`CgroupSetup::Systemd`] is in use (`None` for
    /// `create`, and for plain [`run`]'s own `CgroupSetup::FromSpec`):
    /// the read end of a pipe the child blocks on, right where the
    /// cgroupfs driver's own migration would otherwise happen (see
    /// [`ChildSetup::run`]'s own doc comment on why that ordering
    /// matters), until [`run_reporting_pid`] has finished attempting
    /// the real, D-Bus-driven migration.
    cgroup_ready_read: Option<rustix::fd::OwnedFd>,
    /// Set only when `bundle.spec.hooks` has any `prestart`/
    /// `createRuntime` entries: the read end of a pipe the container's
    /// own process (the grandchild, if a PID namespace was requested;
    /// this same process otherwise — see [`ChildSetup::run`]'s own
    /// relay-fork note) blocks on right before doing anything else in
    /// [`ChildSetup::mount_pivot_and_exec`] — namespaces already exist
    /// by this point but `pivot_root` hasn't happened yet, exactly the
    /// "runtime namespace, after namespace creation, before
    /// `pivot_root`" timing the real spec requires for both hook
    /// points (see `docs/design/0035`) — until [`run_reporting_pid`]
    /// has finished running them in the *runtime's* own process (this
    /// project has no separate persistent runtime process, so that's
    /// simply `run_reporting_pid`'s own caller, matching real `crun`'s
    /// `do_hooks` being called from its own main process while the
    /// forked container process waits on a sync socket).
    hooks_ready_read: Option<rustix::fd::OwnedFd>,
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
        // doc comment. The systemd driver (`cgroup_ready_read`) needs
        // the exact same ordering guarantee, just achieved a different
        // way: blocking here until the *parent* has finished migrating
        // this same process into a real cgroup over D-Bus, rather than
        // this process writing `cgroup.procs` directly.
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
        } else if let Some(ready_read) = &self.cgroup_ready_read {
            let mut buf = [0u8; 1];
            if let Err(e) = rustix::io::read(ready_read, &mut buf) {
                fail(
                    SETUP_FAILURE_EXIT_CODE,
                    &format!("waiting for cgroup migration: {e}"),
                );
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
        // First of all, before anything else here (including opening
        // the exec fifo below): give `run_reporting_pid` a chance to
        // run `prestart`/`createRuntime` hooks in its own (the
        // "runtime namespace") process — see [`Self::hooks_ready_read`]'s
        // own doc comment for exactly why this is the right point.
        // `None` (the overwhelmingly common case: no bundle this
        // project's own benchmark has ever used sets these hooks) costs
        // nothing beyond the `Option` check itself.
        if let Some(ready_read) = &self.hooks_ready_read {
            let mut buf = [0u8; 1];
            if let Err(e) = rustix::io::read(ready_read, &mut buf) {
                fail(
                    SETUP_FAILURE_EXIT_CODE,
                    &format!("waiting for pre-pivot hooks: {e}"),
                );
            }
        }
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
        // `self` is only ever a shared reference here, so the `OwnedFd`s
        // themselves can't be moved out of it directly; reconstructing
        // a fresh `Stdio` from the same raw number is sound because
        // this process (which never uses `self.stdout_log_fd`/
        // `stderr_log_fd` again) always terminates from here on by
        // either a successful `exec` (replacing the process image,
        // which reclaims every fd via ordinary kernel process teardown
        // — Rust's own `Drop` for the original `OwnedFd` field never
        // gets to run either way) or `fail`'s `std::process::exit`
        // (same reclaiming, same reasoning).
        #[allow(unsafe_code)]
        if let Some(fd) = &self.stdout_log_fd {
            command.stdout(unsafe { std::process::Stdio::from_raw_fd(fd.as_raw_fd()) });
        }
        #[allow(unsafe_code)]
        if let Some(fd) = &self.stderr_log_fd {
            command.stderr(unsafe { std::process::Stdio::from_raw_fd(fd.as_raw_fd()) });
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
            // A bind mount whose *real* source is a regular file (e.g.
            // `ociman run -v /etc/localtime:/etc/localtime:ro`) needs a
            // regular-file target, not a directory — `mount(2)`
            // rejects binding a file onto a directory (`ENOTDIR`).
            // Checked directly against this crate's own
            // `RootfsAction::BindMount` (used for `readonly_paths`/
            // masked paths), which already makes exactly this
            // distinction; `Mount` (this arm, used for every ordinary
            // `spec.mounts` entry) didn't, before this fix — harmless
            // for every mount this project ever generated on its own
            // (`proc`/`tmpfs`/`sysfs`/`devpts`/`mqueue`/`cgroup`
            // pseudo-sources, and `readonly_paths`' own always-
            // existing-directory-or-already-mounted case), but a real,
            // previously-latent bug for a genuine file-source bind
            // mount, only now reachable via `-v`.
            let is_bind = parsed.set_flags & oci_mount::options::flags::BIND != 0;
            let source_is_file =
                is_bind && source.as_deref().is_some_and(|s| Path::new(s).is_file());
            if source_is_file {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                if !target.exists() {
                    std::fs::write(target, b"")?;
                }
            } else {
                std::fs::create_dir_all(target)?;
            }
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
