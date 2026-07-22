//! `ocirun` — standalone OCI runtime (crun equivalent).
//!
//! Thin, runc-CLI-compatible wrapper over `oci-runtime-core`, so it can be
//! dropped into other engines. Shipped so far: `spec`, `state`, `list`,
//! `run` (create-and-start in one step), the separate `create`/`start`/
//! `kill`/`delete` two-phase lifecycle, `exec` (running an
//! *additional* process inside an already-running container, joining
//! its existing namespaces rather than creating new ones), and
//! `features` (real, checked support-surface introspection, see
//! `features` module). `prestart`/`createRuntime`/`poststart`/
//! `poststop` lifecycle hooks run for `run`; `createContainer`/
//! `startContainer` run for both `run` and the `create`/`start`
//! two-phase lifecycle (shared code between the two, see
//! `docs/design/0087`); `prestart`/`createRuntime`/`poststart`/
//! `poststop` for the `create`/`start`/`kill`/`delete` lifecycle
//! specifically still remain — see `docs/design/0026`/`0035`/`0087`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context as _;
use clap::Parser;
use oci_runtime_core::state::Status;
use oci_runtime_core::{StateStore, exec_fifo};

mod features;

/// Command-line interface.
#[derive(Debug, Parser)]
#[command(
    name = "ocirun",
    about = "OCI runtime: create/start/kill containers per the OCI runtime spec",
    version = oci_cli_common::version::long(env!("CARGO_PKG_VERSION")),
)]
struct Cli {
    #[command(flatten)]
    global: oci_cli_common::GlobalArgs,

    /// Root directory for storage of container state (should be tmpfs).
    /// Defaults to `/run/ocirun`, or `$XDG_RUNTIME_DIR/ocirun` rootless.
    #[arg(long, global = true, value_name = "DIR")]
    root: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

/// Subcommands shipped so far.
#[derive(Debug, clap::Subcommand)]
enum Command {
    /// Create a new specification file (`config.json`) for a bundle.
    Spec {
        /// Path to the root of the bundle directory (defaults to the
        /// current directory).
        #[arg(short, long, value_name = "DIR")]
        bundle: Option<PathBuf>,
        /// Generate a configuration for a rootless container.
        #[arg(long)]
        rootless: bool,
    },
    /// Output the state of a container.
    State {
        /// The container's ID.
        id: String,
    },
    /// List containers started by `ocirun` with the given root.
    List {
        /// Output format: "table" or "json".
        #[arg(short, long, default_value = "table")]
        format: String,
        /// Display only container IDs.
        #[arg(short, long)]
        quiet: bool,
    },
    /// Create and immediately start a container (combines OCI "create"
    /// and "start" into one step, foreground, like `runc run`/`crun
    /// run`). The container's own exit code becomes `ocirun`'s exit code.
    Run {
        /// The container's ID (accepted for CLI-compatibility; not yet
        /// tracked in the state store — that lands with `create`/
        /// `start`/`delete`).
        id: String,
        /// Path to the root of the bundle directory (defaults to the
        /// current directory).
        #[arg(short, long, value_name = "DIR")]
        bundle: Option<PathBuf>,
        /// Write the container's own pid to this file as soon as it's
        /// known, matching real `runc run --pid-file`/`crun run
        /// --pid-file` — atomically (temp file + rename, matching real
        /// runc's own `createPidFile` exactly, `~/git/runc/
        /// utils_linux.go`), so a concurrent reader can never observe
        /// a partially-written file. Unlike real runc (which aborts
        /// the whole invocation if this write fails), a failure here
        /// is logged and tolerated, not fatal — a deliberate,
        /// documented divergence: this project's own established
        /// pattern for auxiliary bookkeeping writes (`ociman run`'s
        /// own state-record write, cgroup/hook fallbacks) is
        /// "tolerate and log", not "abort a container that's already
        /// running".
        #[arg(long, value_name = "FILE")]
        pid_file: Option<PathBuf>,
    },
    /// Create a container: set up namespaces/mounts/cgroups and leave
    /// its process blocked, waiting for `start`. Returns once setup
    /// finishes (does not wait for `start`); the container process
    /// keeps running in the background.
    Create {
        /// The container's ID.
        id: String,
        /// Path to the root of the bundle directory (defaults to the
        /// current directory).
        #[arg(short, long, value_name = "DIR")]
        bundle: Option<PathBuf>,
        /// Same as `run --pid-file` — see its own doc comment.
        #[arg(long, value_name = "FILE")]
        pid_file: Option<PathBuf>,
    },
    /// Start a previously `create`d container's process running.
    Start {
        /// The container's ID.
        id: String,
    },
    /// Send a signal (default `SIGTERM`) to a container's init process.
    Kill {
        /// The container's ID.
        id: String,
        /// Signal to send: a number, or a name with or without the
        /// `SIG` prefix (case-insensitive) — e.g. `9`, `KILL`, `SIGKILL`.
        signal: Option<String>,
    },
    /// Remove a container's on-disk state. Refuses a still-running
    /// container unless `--force` (which sends `SIGKILL` first).
    Delete {
        /// The container's ID.
        id: String,
        /// Forcibly kill the container first if it is still running.
        #[arg(short, long)]
        force: bool,
    },
    /// Run an additional process inside an already-running container,
    /// joining its existing namespaces (rather than `create`/`run`,
    /// which only ever start a container's *first* process).
    Exec {
        /// The container's ID.
        id: String,
        /// UID (format: `<uid>[:<gid>]`) — numeric only, matching real
        /// `runc exec --user`; overriding to a *named* user needs
        /// `/etc/passwd` resolution inside the rootfs, which is a
        /// higher-level-tool concern (`ociman exec --user` supports
        /// it) rather than this low-level runtime's own.
        #[arg(short, long)]
        user: Option<String>,
        /// Current working directory inside the container.
        #[arg(long)]
        cwd: Option<String>,
        /// Additional `KEY=value` environment variables, appended to
        /// (not replacing) the container's own process environment.
        /// Repeatable.
        #[arg(short, long = "env")]
        env: Vec<String>,
        /// Command and arguments to run inside the container.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        args: Vec<String>,
    },
    /// Show this runtime's own real, checked support surface (hooks,
    /// mount options, namespaces, capabilities, cgroup/seccomp
    /// details) as parsable JSON — see the `features` module's own
    /// doc comment for exactly what's reported and why.
    Features,
    /// List the real processes running inside a container: every pid
    /// in its own cgroup (and any nested sub-cgroups), matching real
    /// `runc ps` exactly (`~/git/runc/ps.go`) — a table (the real host
    /// `ps` binary's own output, filtered to just this container's
    /// pids) by default, or a bare JSON array of pids with `--format
    /// json`. Any extra arguments are passed straight through to the
    /// real host `ps` binary itself (default: `-ef`), so
    /// `ocirun ps <id> -aux` works exactly like `runc ps <id> -aux`
    /// does.
    Ps {
        /// The container's ID.
        id: String,
        /// "table" (default) or "json".
        #[arg(short, long, default_value = "table")]
        format: String,
        /// Arguments passed straight through to the real host `ps`
        /// binary (default: `-ef`).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        ps_args: Vec<String>,
    },
    /// Update a running container's real cgroup resource limits in
    /// place — matching real `runc update`'s own `--resources`/`-r`
    /// JSON-file mode exactly (its own many individual ad-hoc
    /// `--memory`/`--cpu-shares`/... flags aren't supported yet, a
    /// deliberately narrower first slice; see `docs/design/0099`).
    Update {
        /// The container's ID.
        id: String,
        /// Path to a JSON file containing the `LinuxResources` to
        /// apply (same shape as `config.json`'s own
        /// `linux.resources`), or `-` to read it from stdin. Any
        /// field the JSON leaves unset is left completely alone —
        /// this only ever changes what's actually given.
        #[arg(short, long)]
        resources: PathBuf,
    },
    /// Freeze every process in a running container via the real
    /// cgroup v2 freezer (`cgroup.freeze`) — matching real `runc
    /// pause`'s own core effect exactly (see `docs/design/0142` for
    /// this increment's own deliberately narrower scope: this
    /// genuinely freezes the container, but `ocirun state`/`ocirun
    /// list` don't yet report a separate `paused` status the way real
    /// runc's own does).
    Pause {
        /// The container's ID.
        id: String,
    },
    /// Thaw a container previously frozen by `pause`, matching real
    /// `runc resume`'s own core effect exactly.
    Resume {
        /// The container's ID.
        id: String,
    },
}

/// Parse a `runc exec --user`-style `<uid>[:<gid>]` string: `uid` is
/// required and numeric; `gid`, if given, is also numeric — no named-
/// user/group resolution here (that's `ociman exec --user`'s job, via
/// the container's own `/etc/passwd`/`/etc/group`, not this low-level
/// runtime's).
fn parse_numeric_user(s: &str) -> anyhow::Result<(u32, Option<u32>)> {
    let (uid_str, gid_str) = s.split_once(':').unwrap_or((s, ""));
    let uid: u32 = uid_str
        .parse()
        .with_context(|| format!("--user: {uid_str:?} is not a valid numeric uid"))?;
    let gid = if gid_str.is_empty() {
        None
    } else {
        Some(
            gid_str
                .parse()
                .with_context(|| format!("--user: {gid_str:?} is not a valid numeric gid"))?,
        )
    };
    Ok((uid, gid))
}

/// Filename of the OCI runtime-spec bundle configuration, per the spec.
const SPEC_CONFIG: &str = "config.json";

fn main() -> std::process::ExitCode {
    oci_cli_common::run_main(|| {
        let cli = Cli::parse();
        oci_cli_common::logging::init(&cli.global)?;
        tracing::debug!(
            git_hash = oci_cli_common::version::GIT_HASH,
            "ocirun starting"
        );
        let root = cli
            .root
            .unwrap_or_else(|| oci_cli_common::runtime_root::default_root("ocirun"));

        match cli.command {
            None => anyhow::bail!("no command given; try `ocirun --help`"),
            Some(Command::Spec { bundle, rootless }) => cmd_spec(bundle.as_deref(), rootless),
            Some(Command::State { id }) => cmd_state(&root, &id),
            Some(Command::List { format, quiet }) => cmd_list(&root, &format, quiet),
            Some(Command::Run {
                id,
                bundle,
                pid_file,
            }) => cmd_run(&id, bundle.as_deref(), pid_file.as_deref()),
            Some(Command::Create {
                id,
                bundle,
                pid_file,
            }) => cmd_create(&root, &id, bundle.as_deref(), pid_file.as_deref()),
            Some(Command::Start { id }) => cmd_start(&root, &id),
            Some(Command::Kill { id, signal }) => cmd_kill(&root, &id, signal.as_deref()),
            Some(Command::Delete { id, force }) => cmd_delete(&root, &id, force),
            Some(Command::Exec {
                id,
                user,
                cwd,
                env,
                args,
            }) => cmd_exec(&root, &id, user.as_deref(), cwd.as_deref(), &env, &args),
            Some(Command::Features) => oci_cli_common::output::print_json(&features::features()),
            Some(Command::Ps {
                id,
                format,
                ps_args,
            }) => cmd_ps(&root, &id, &format, &ps_args),
            Some(Command::Update { id, resources }) => cmd_update(&root, &id, &resources),
            Some(Command::Pause { id }) => cmd_pause(&root, &id),
            Some(Command::Resume { id }) => cmd_resume(&root, &id),
        }
    })
}

fn cmd_spec(bundle: Option<&Path>, rootless: bool) -> anyhow::Result<()> {
    let dir = bundle.unwrap_or_else(|| Path::new("."));
    let path = dir.join(SPEC_CONFIG);

    if path.exists() {
        anyhow::bail!("file {} exists; remove it first", path.display());
    }

    let mut spec = oci_spec_types::runtime::Spec::example();
    if rootless {
        let (euid, egid) = oci_cli_common::identity::effective_uid_gid();
        spec = spec.into_rootless(euid, egid);
    }

    // Match runc's `MarshalIndent(spec, "", "\t")` formatting and
    // `os.WriteFile(..., 0o666)` permissions (reduced by umask, same as
    // runc gets), so tooling that snapshot-diffs `runc spec` output is not
    // surprised by whitespace alone.
    let mut buf = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"\t");
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);
    serde::Serialize::serialize(&spec, &mut ser).context("serializing config.json")?;

    std::fs::write(&path, &buf).with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666))
            .with_context(|| format!("setting permissions on {}", path.display()))?;
    }

    Ok(())
}

fn cmd_state(root: &Path, id: &str) -> anyhow::Result<()> {
    let store = StateStore::open(root)
        .with_context(|| format!("opening container state root {}", root.display()))?;
    let state = store.load(id)?;
    oci_cli_common::output::print_json(&state.to_view())?;
    Ok(())
}

fn cmd_list(root: &Path, format: &str, quiet: bool) -> anyhow::Result<()> {
    let store = StateStore::open(root)
        .with_context(|| format!("opening container state root {}", root.display()))?;
    let views: Vec<_> = store.list()?.iter().map(|s| s.to_view()).collect();

    if quiet {
        for view in &views {
            println!("{}", view.id);
        }
        return Ok(());
    }

    match format {
        "table" => {
            println!(
                "{:<12}{:<8}{:<10}{:<40}CREATED",
                "ID", "PID", "STATUS", "BUNDLE"
            );
            for view in &views {
                println!(
                    "{:<12}{:<8}{:<10}{:<40}{}",
                    view.id, view.pid, view.status, view.bundle, view.created
                );
            }
        }
        "json" => oci_cli_common::output::print_json(&views)?,
        other => anyhow::bail!("invalid format option: {other:?} (expected \"table\" or \"json\")"),
    }
    Ok(())
}

fn cmd_run(id: &str, bundle: Option<&Path>, pid_file: Option<&Path>) -> anyhow::Result<()> {
    let dir = bundle.unwrap_or_else(|| Path::new("."));
    tracing::debug!(container_id = id, bundle = %dir.display(), "run starting");

    let bundle = oci_runtime_core::Bundle::load(dir)
        .with_context(|| format!("loading bundle from {}", dir.display()))?;
    let rootfs =
        oci_runtime_core::validate::validate(&bundle).context("config.json failed validation")?;

    // `launch::run` itself is just `run_reporting_pid` with a no-op
    // callback (see its own doc comment) — called directly here
    // instead so `--pid-file`'s own callback has somewhere to hook in,
    // without this binary needing to duplicate `run`'s own choice of
    // `CgroupSetup::FromSpec`/no log path.
    //
    // SAFETY: `ocirun`'s own process has not spawned any additional
    // threads by this point (argument parsing and log initialization
    // don't spawn any), so the fork `launch::run_reporting_pid`
    // performs is sound — see its own safety note for the requirement
    // this satisfies.
    #[allow(unsafe_code)]
    let exit_code = unsafe {
        oci_runtime_core::launch::run_reporting_pid(
            id,
            &bundle,
            &rootfs,
            None,
            oci_runtime_core::launch::CgroupSetup::FromSpec,
            |pid| {
                if let Some(path) = pid_file {
                    write_pid_file(path, pid);
                }
            },
        )
    }
    .context("running container")?;

    // The container's own exit code becomes ours, matching runc/crun's
    // `run`: exit code 0 must mean "the container's process exited 0",
    // not merely "ocirun didn't error", so this bypasses
    // oci_cli_common::run_main's usual Ok(())-means-success mapping.
    std::process::exit(exit_code);
}

fn cmd_create(
    root: &Path,
    id: &str,
    bundle: Option<&Path>,
    pid_file: Option<&Path>,
) -> anyhow::Result<()> {
    let dir = bundle.unwrap_or_else(|| Path::new("."));
    tracing::debug!(container_id = id, bundle = %dir.display(), "create starting");

    let loaded = oci_runtime_core::Bundle::load(dir)
        .with_context(|| format!("loading bundle from {}", dir.display()))?;
    let rootfs =
        oci_runtime_core::validate::validate(&loaded).context("config.json failed validation")?;

    let store = StateStore::open(root)
        .with_context(|| format!("opening container state root {}", root.display()))?;
    let annotations = loaded.spec.annotations.clone();
    let mut state = store.create(id, dir, &rootfs, annotations)?;

    let result = (|| -> anyhow::Result<i32> {
        let fifo_path = store.container_dir(id).join(exec_fifo::FILENAME);
        exec_fifo::create(&fifo_path).context("creating exec fifo")?;

        // SAFETY: `ocirun`'s own process has not spawned any additional
        // threads by this point, same as `run`'s own safety note.
        #[allow(unsafe_code)]
        let pid = unsafe { oci_runtime_core::launch::create(id, &loaded, &rootfs, &fifo_path) }
            .context("creating container")?;
        Ok(pid)
    })();

    let pid = match result {
        Ok(pid) => pid,
        Err(e) => {
            // Best-effort cleanup: don't leave a container `list`/state
            // would show as permanently stuck in "creating" behind a
            // failed `create`, matching the "don't leave a half-made
            // state directory behind" precedent `StateStore::create`
            // itself already follows for its own write failure.
            let _ = store.remove(id);
            return Err(e);
        }
    };

    if let Some(path) = pid_file {
        write_pid_file(path, pid);
    }

    state.status = Status::Created;
    state.pid = Some(pid);
    store.write(&state)?;
    Ok(())
}

/// Atomically write `pid` to `path`: create a temp file (`.
/// <basename>`, same directory) then rename into place — matching
/// real runc's own `createPidFile` exactly
/// (`~/git/runc/utils_linux.go`), including its exact file content
/// (the bare decimal pid, no trailing newline), permissions (`0o666`,
/// reduced by umask same as any other file), and use of `O_SYNC` (the
/// write reaches disk before the rename makes it visible) — so a
/// concurrent reader (the whole point of `--pid-file`: a process
/// supervisor watching for it) can never observe a partially-written
/// file. Logged and tolerated on failure, not fatal — see `--pid-file`
/// 's own doc comment on `Command::Run` for why this project
/// deliberately diverges from real runc's own harder failure handling
/// here.
fn write_pid_file(path: &Path, pid: i32) {
    if let Err(e) = write_pid_file_inner(path, pid) {
        tracing::warn!(path = %path.display(), error = %e, "writing --pid-file (tolerated)");
    }
}

fn write_pid_file_inner(path: &Path, pid: i32) -> anyhow::Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;

    let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    let file_name = path
        .file_name()
        .with_context(|| format!("{} has no file name", path.display()))?;
    let tmp_name = {
        let mut name = std::ffi::OsString::from(".");
        name.push(file_name);
        name
    };
    let tmp_path = dir.map_or_else(|| PathBuf::from(&tmp_name), |d| d.join(&tmp_name));

    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o666)
        .custom_flags(libc::O_SYNC)
        .open(&tmp_path)
        .with_context(|| format!("creating {}", tmp_path.display()))?;
    std::io::Write::write_all(&mut file, pid.to_string().as_bytes())
        .with_context(|| format!("writing {}", tmp_path.display()))?;
    drop(file);
    std::fs::rename(&tmp_path, path)
        .with_context(|| format!("renaming {} to {}", tmp_path.display(), path.display()))?;
    Ok(())
}

fn cmd_start(root: &Path, id: &str) -> anyhow::Result<()> {
    let store = StateStore::open(root)
        .with_context(|| format!("opening container state root {}", root.display()))?;
    let mut state = store.load(id)?;
    let status = state.effective_status();
    if status != Status::Created {
        anyhow::bail!("cannot start a container in the {status} state");
    }

    let fifo_path = store.container_dir(id).join(exec_fifo::FILENAME);
    exec_fifo::signal_start(&fifo_path).context("signalling container to start")?;
    // Best-effort: a leftover fifo doesn't stop the container from
    // running, only clutters its state directory.
    let _ = std::fs::remove_file(&fifo_path);

    // Matches real runc's own `Container.exec()` exactly (signal the
    // fifo, then run `poststart` — see `docs/design/0089`): reload the
    // bundle fresh from `state.bundle` rather than keeping one around
    // from `create` time (this is a wholly separate CLI invocation).
    // Best-effort: a bundle that's moved or been removed since
    // `create` shouldn't stop `start` itself from succeeding, matching
    // `remove_cgroup_directory_if_any`'s own established tolerance for
    // exactly this same failure mode.
    if let Ok(bundle) = oci_runtime_core::Bundle::load(&state.bundle) {
        oci_runtime_core::launch::run_poststart_hooks(&bundle, id, state.pid.unwrap_or(0));
    }

    state.status = Status::Running;
    store.write(&state)?;
    Ok(())
}

fn cmd_kill(root: &Path, id: &str, signal: Option<&str>) -> anyhow::Result<()> {
    let store = StateStore::open(root)
        .with_context(|| format!("opening container state root {}", root.display()))?;
    let state = store.load(id)?;
    let Some(pid) = state
        .pid
        .filter(|_| state.effective_status() != Status::Stopped)
    else {
        anyhow::bail!("container {id:?} is not running");
    };

    let signal = oci_runtime_core::signal::parse(signal.unwrap_or("SIGTERM"))?;
    oci_runtime_core::process::kill(pid, signal).context("sending signal")?;
    Ok(())
}

fn cmd_delete(root: &Path, id: &str, force: bool) -> anyhow::Result<()> {
    let store = StateStore::open(root)
        .with_context(|| format!("opening container state root {}", root.display()))?;
    let state = store.load(id)?;
    let status = state.effective_status();

    // Matches real runc's `delete`: a still-`Running` container refuses
    // deletion without `--force`; `Created` (never started, blocked on
    // the exec fifo) or `Stopped` may always be deleted (a `Created`
    // container's process is harmless to kill outright — it never ran
    // the user's command).
    if !force && status == Status::Running {
        anyhow::bail!("cannot delete container {id:?} that is not stopped: {status}");
    }

    if let Some(pid) = state.pid
        && status != Status::Stopped
    {
        // "KILL" always parses; it's a name this crate's own table
        // hardcodes.
        let sigkill = oci_runtime_core::signal::parse("KILL").expect("KILL is always valid");
        let _ = oci_runtime_core::process::kill(pid, sigkill);
        // Bounded wait for the kill to actually take effect (matches
        // runc's own `killContainer`: poll, don't block forever) —
        // proceeding to delete regardless once the deadline passes
        // rather than leaving the container permanently undeletable.
        for _ in 0..50 {
            if !oci_runtime_core::process::alive(pid) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    remove_cgroup_directory_if_any(&state.bundle);
    // Matches real runc's own `destroy()`, which always runs
    // `poststop` hooks as part of tearing a container down (see
    // `docs/design/0089`) — best-effort for the same reason
    // `remove_cgroup_directory_if_any` already is: a moved/removed
    // bundle shouldn't stop `delete` itself from succeeding.
    if let Ok(bundle) = oci_runtime_core::Bundle::load(&state.bundle) {
        oci_runtime_core::launch::run_poststop_hooks(&bundle, id);
    }
    store.remove(id)?;
    Ok(())
}

/// Best-effort cleanup of the cgroup directory (if any) a `create`d
/// container's own process was migrated into — see
/// `oci_runtime_core::cgroups::remove`'s own doc comment for why this
/// is necessary at all (the kernel does not do it on its own). Unlike
/// `launch::run_reporting_pid` (which always has the bundle already
/// loaded), `delete` only has `state.bundle`'s path on hand, so this
/// re-reads `config.json` for the one field it actually needs. A
/// failure (including the bundle no longer being readable at all,
/// which can legitimately happen well after the container that used
/// it is gone) is logged and tolerated: it must never block deleting
/// the container's own state, which is the whole point of `delete`.
fn remove_cgroup_directory_if_any(bundle_path: &str) {
    let Ok(bundle) = oci_runtime_core::Bundle::load(bundle_path) else {
        return;
    };
    let Ok(Some(dir)) = oci_runtime_core::cgroups::directory_for(
        Path::new("/sys/fs/cgroup"),
        bundle
            .spec
            .linux
            .as_ref()
            .and_then(|l| l.cgroups_path.as_deref()),
    ) else {
        return;
    };
    if let Err(e) = oci_runtime_core::cgroups::remove(&dir) {
        tracing::warn!(cgroup = %dir.display(), error = %e, "removing cgroup directory (tolerated)");
    }
}

/// List the real processes running inside a container — matches real
/// `runc ps` exactly (`~/git/runc/ps.go`): get every pid from the
/// container's own cgroup (see
/// `oci_runtime_core::cgroups::all_pids`), then either print them as a
/// bare JSON array (`--format json`) or run the real host `ps` binary
/// and filter its output to just those pids (`--format table`, the
/// default). A container with no `cgroupsPath` at all (this project's
/// own bundles routinely have none — cgroup management is opt-in, see
/// `docs/design/0015`) simply has no pids to report, not an error.
fn cmd_ps(root: &Path, id: &str, format: &str, ps_args: &[String]) -> anyhow::Result<()> {
    let store = StateStore::open(root)
        .with_context(|| format!("opening container state root {}", root.display()))?;
    let state = store.load(id)?;

    let bundle = oci_runtime_core::Bundle::load(&state.bundle)
        .with_context(|| format!("loading bundle from {}", state.bundle))?;
    let cgroup_dir = oci_runtime_core::cgroups::directory_for(
        Path::new("/sys/fs/cgroup"),
        bundle
            .spec
            .linux
            .as_ref()
            .and_then(|l| l.cgroups_path.as_deref()),
    )?;
    let pids = match &cgroup_dir {
        Some(dir) => oci_runtime_core::cgroups::all_pids(dir)
            .with_context(|| format!("listing processes in {}", dir.display()))?,
        None => Vec::new(),
    };

    match format {
        "json" => oci_cli_common::output::print_json(&pids),
        "table" => {
            oci_runtime_core::cgroups::print_ps_table(&pids, ps_args).context("printing ps table")
        }
        other => anyhow::bail!("invalid format option: {other:?} (want \"table\" or \"json\")"),
    }
}

/// Load `id`'s own persisted state and bundle, then resolve its real
/// cgroup v2 directory — shared by `cmd_update`/`cmd_pause`/
/// `cmd_resume` so there is exactly one implementation of "find this
/// container's own cgroup", not three near-identical copies.
fn resolve_cgroup_dir(root: &Path, id: &str) -> anyhow::Result<PathBuf> {
    let store = StateStore::open(root)
        .with_context(|| format!("opening container state root {}", root.display()))?;
    let state = store.load(id)?;
    let bundle = oci_runtime_core::Bundle::load(&state.bundle)
        .with_context(|| format!("loading bundle from {}", state.bundle))?;
    oci_runtime_core::cgroups::directory_for(
        Path::new("/sys/fs/cgroup"),
        bundle
            .spec
            .linux
            .as_ref()
            .and_then(|l| l.cgroups_path.as_deref()),
    )?
    .ok_or_else(|| anyhow::anyhow!("container {id:?} has no cgroup (no cgroupsPath set)"))
}

/// Update a running container's real cgroup resource limits — matches
/// real `runc update --resources=<file>` exactly (`~/git/runc/
/// update.go`): `plan_resources` only ever emits a write for a field
/// the given `LinuxResources` JSON actually sets (every field is
/// `Option`, matching the real runtime-spec's own shape), so a
/// deliberately narrow JSON blob (just `{"memory": {"limit": ...}}`,
/// say) changes only that one thing and leaves every other real
/// cgroup limit exactly as it was — no separate "merge with what's
/// already set" logic is needed for the cgroup-writing side at all.
/// Deliberately narrower than real runc's own full command: no
/// individual `--memory`/`--cpu-shares`/... ad-hoc flags (JSON-file
/// mode only), and the container's own persisted `config.json` is not
/// rewritten to reflect the change (a later `ocirun state` still shows
/// the limits it was *created* with) — see `docs/design/0099`.
fn cmd_update(root: &Path, id: &str, resources_path: &Path) -> anyhow::Result<()> {
    let cgroup_dir = resolve_cgroup_dir(root, id)?;

    let resources: oci_spec_types::runtime::LinuxResources = if resources_path == Path::new("-") {
        serde_json::from_reader(std::io::stdin()).context("reading resources JSON from stdin")?
    } else {
        let file = std::fs::File::open(resources_path)
            .with_context(|| format!("opening {}", resources_path.display()))?;
        serde_json::from_reader(file)
            .with_context(|| format!("parsing {} as JSON", resources_path.display()))?
    };

    let writes = oci_runtime_core::cgroups::plan_resources(&resources);
    oci_runtime_core::cgroups::apply(&cgroup_dir, &writes)
        .with_context(|| format!("applying updated resources to {}", cgroup_dir.display()))?;
    Ok(())
}

/// Matches real runc's own `Pause`: allowed for a container that's
/// `Created` or `Running` (checked directly against `~/git/runc/
/// libcontainer/container_linux.go`'s own `Pause`); anything else
/// (most notably `Stopped`) is a clear error. Freezing an
/// already-frozen cgroup is itself a real, harmless no-op at the
/// kernel level (this project doesn't yet track a separate `Paused`
/// status of its own to short-circuit on first — see this command's
/// own doc comment in `main.rs`), so no extra check is needed for
/// "already paused" specifically.
fn cmd_pause(root: &Path, id: &str) -> anyhow::Result<()> {
    let store = StateStore::open(root)
        .with_context(|| format!("opening container state root {}", root.display()))?;
    let state = store.load(id)?;
    let status = state.effective_status();
    if !matches!(status, Status::Created | Status::Running) {
        anyhow::bail!("cannot pause a container in the {status} state");
    }
    let cgroup_dir = resolve_cgroup_dir(root, id)?;
    oci_runtime_core::cgroups::set_frozen(&cgroup_dir, true)
        .with_context(|| format!("freezing {}", cgroup_dir.display()))
}

/// Matches real runc's own `Resume`: allowed for the same `Created`/
/// `Running` states `pause` itself accepts — this project has no
/// separate `Paused` status of its own to require instead (real
/// runc's own `Resume` requires exactly `Paused`; seeing `Running`
/// here already covers the "was already paused, cgroup-wise" case,
/// since this project reports pause/resume state via the real cgroup
/// freezer directly, not a separate persisted status field).
fn cmd_resume(root: &Path, id: &str) -> anyhow::Result<()> {
    let store = StateStore::open(root)
        .with_context(|| format!("opening container state root {}", root.display()))?;
    let state = store.load(id)?;
    let status = state.effective_status();
    if !matches!(status, Status::Created | Status::Running) {
        anyhow::bail!("cannot resume a container in the {status} state");
    }
    let cgroup_dir = resolve_cgroup_dir(root, id)?;
    oci_runtime_core::cgroups::set_frozen(&cgroup_dir, false)
        .with_context(|| format!("thawing {}", cgroup_dir.display()))
}

fn cmd_exec(
    root: &Path,
    id: &str,
    user: Option<&str>,
    cwd: Option<&str>,
    extra_env: &[String],
    args: &[String],
) -> anyhow::Result<()> {
    let store = StateStore::open(root)
        .with_context(|| format!("opening container state root {}", root.display()))?;
    let state = store.load(id)?;
    let status = state.effective_status();
    if status != Status::Running {
        anyhow::bail!("cannot exec in a container in the {status} state");
    }
    let pid = state
        .pid
        .ok_or_else(|| anyhow::anyhow!("container {id:?} has no recorded pid"))?;

    // The exec'd process joins the *same* namespaces and capability
    // set the container's own init process was given at `create`/`run`
    // time, read back from its own bundle — user/cwd/env default the
    // same way, but `--user`/`--cwd`/`--env` (matching real `runc
    // exec`'s own flags) can override them per invocation.
    let bundle = oci_runtime_core::Bundle::load(Path::new(&state.bundle))
        .with_context(|| format!("loading bundle from {}", state.bundle))?;
    let process_spec = bundle
        .spec
        .process
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("bundle at {} has no process section", state.bundle))?;
    let namespaces: Vec<_> = bundle
        .spec
        .linux
        .as_ref()
        .map_or(&[][..], |l| &l.namespaces)
        .iter()
        .map(|ns| ns.kind)
        .collect();

    let mut effective_user = process_spec.user.clone();
    if let Some(user) = user {
        let (uid, gid) = parse_numeric_user(user)?;
        effective_user.uid = uid;
        // Matches real `runc exec`: `--user 1000` alone only overrides
        // the uid, leaving the container's own default gid in place;
        // `--user 1000:1000` overrides both.
        if let Some(gid) = gid {
            effective_user.gid = gid;
        }
    }
    let mut effective_env = process_spec.env.clone();
    effective_env.extend(extra_env.iter().cloned());

    let request = oci_runtime_core::exec::ExecRequest {
        namespaces,
        user: effective_user,
        capabilities: process_spec.capabilities.clone(),
        no_new_privileges: process_spec.no_new_privileges,
        cwd: cwd
            .map(str::to_string)
            .unwrap_or_else(|| process_spec.cwd.clone()),
        env: effective_env,
        args: args.to_vec(),
    };

    // SAFETY: `ocirun`'s own process has not spawned any additional
    // threads by this point, same as `run`'s/`create`'s own safety
    // note.
    #[allow(unsafe_code)]
    let exit_code = unsafe { oci_runtime_core::exec::exec(pid, request) }.context("exec")?;

    // The exec'd process's own exit code becomes ours, same convention
    // `run`/`create` already follow.
    std::process::exit(exit_code);
}
