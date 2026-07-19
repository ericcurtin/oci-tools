//! `ocirun` — standalone OCI runtime (crun equivalent).
//!
//! Thin, runc-CLI-compatible wrapper over `oci-runtime-core`, so it can be
//! dropped into other engines. Shipped so far: `spec`, `state`, `list`,
//! `run` (create-and-start in one step), the separate `create`/`start`/
//! `kill`/`delete` two-phase lifecycle, and `exec` (running an
//! *additional* process inside an already-running container, joining
//! its existing namespaces rather than creating new ones).
//! `poststart`/`poststop` lifecycle hooks run for `run`; the other four
//! hook points, and hooks for the `create`/`start`/`kill`/`delete`
//! lifecycle, remain — see `docs/design/0026`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context as _;
use clap::Parser;
use oci_runtime_core::state::Status;
use oci_runtime_core::{StateStore, exec_fifo};

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

/// Subcommands shipped so far. `exec`/`features` arrive with the rest
/// of milestone 3.
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
        /// Command and arguments to run inside the container.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        args: Vec<String>,
    },
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
            None => anyhow::bail!(
                "no command given; try `ocirun --help` (`exec`/`features` arrive with the \
                 rest of milestone 3)"
            ),
            Some(Command::Spec { bundle, rootless }) => cmd_spec(bundle.as_deref(), rootless),
            Some(Command::State { id }) => cmd_state(&root, &id),
            Some(Command::List { format, quiet }) => cmd_list(&root, &format, quiet),
            Some(Command::Run { id, bundle }) => cmd_run(&id, bundle.as_deref()),
            Some(Command::Create { id, bundle }) => cmd_create(&root, &id, bundle.as_deref()),
            Some(Command::Start { id }) => cmd_start(&root, &id),
            Some(Command::Kill { id, signal }) => cmd_kill(&root, &id, signal.as_deref()),
            Some(Command::Delete { id, force }) => cmd_delete(&root, &id, force),
            Some(Command::Exec { id, args }) => cmd_exec(&root, &id, &args),
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

fn cmd_run(id: &str, bundle: Option<&Path>) -> anyhow::Result<()> {
    let dir = bundle.unwrap_or_else(|| Path::new("."));
    tracing::debug!(container_id = id, bundle = %dir.display(), "run starting");

    let bundle = oci_runtime_core::Bundle::load(dir)
        .with_context(|| format!("loading bundle from {}", dir.display()))?;
    let rootfs =
        oci_runtime_core::validate::validate(&bundle).context("config.json failed validation")?;

    // SAFETY: `ocirun`'s own process has not spawned any additional
    // threads by this point (argument parsing and log initialization
    // don't spawn any), so the fork `launch::run` performs is sound —
    // see its own safety note for the requirement this satisfies.
    #[allow(unsafe_code)]
    let exit_code = unsafe { oci_runtime_core::launch::run(id, &bundle, &rootfs) }
        .context("running container")?;

    // The container's own exit code becomes ours, matching runc/crun's
    // `run`: exit code 0 must mean "the container's process exited 0",
    // not merely "ocirun didn't error", so this bypasses
    // oci_cli_common::run_main's usual Ok(())-means-success mapping.
    std::process::exit(exit_code);
}

fn cmd_create(root: &Path, id: &str, bundle: Option<&Path>) -> anyhow::Result<()> {
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
        let pid = unsafe { oci_runtime_core::launch::create(&loaded, &rootfs, &fifo_path) }
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

    state.status = Status::Created;
    state.pid = Some(pid);
    store.write(&state)?;
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

fn cmd_exec(root: &Path, id: &str, args: &[String]) -> anyhow::Result<()> {
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

    // The exec'd process joins the *same* namespaces, user/capability
    // set, working directory, and environment the container's own init
    // process was given at `create`/`run` time — read back from its own
    // bundle rather than re-specified on this command line, matching
    // real `runc exec`'s default (no `--user`/`--cwd`/`--env` override
    // support yet).
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

    let request = oci_runtime_core::exec::ExecRequest {
        namespaces,
        user: process_spec.user.clone(),
        capabilities: process_spec.capabilities.clone(),
        no_new_privileges: process_spec.no_new_privileges,
        cwd: process_spec.cwd.clone(),
        env: process_spec.env.clone(),
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
