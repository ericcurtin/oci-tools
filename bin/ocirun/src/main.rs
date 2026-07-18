//! `ocirun` — standalone OCI runtime (crun equivalent).
//!
//! Thin, runc-CLI-compatible wrapper over `oci-runtime-core`, so it can be
//! dropped into other engines. Shipped so far: `spec`, `state`, `list`,
//! `run` (create-and-start in one step; the separate `create`/`start`/
//! `kill`/`delete`/`exec` two-phase lifecycle — which needs a persistent
//! background process surviving after the CLI invocation returns, and
//! state-store integration with a live pid to track — is not implemented
//! yet).

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clap::Parser;
use oci_runtime_core::StateStore;

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

/// Subcommands shipped so far. `create`/`start`/`kill`/`delete`/`exec`/
/// `features` arrive with the rest of milestone 3.
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
                "no command given; try `ocirun --help` (`spec`/`state`/`list`/`run` are \
                 implemented; `create`/`start`/`kill`/`delete`/`exec`/`features` arrive \
                 with the rest of milestone 3)"
            ),
            Some(Command::Spec { bundle, rootless }) => cmd_spec(bundle.as_deref(), rootless),
            Some(Command::State { id }) => cmd_state(&root, &id),
            Some(Command::List { format, quiet }) => cmd_list(&root, &format, quiet),
            Some(Command::Run { id, bundle }) => cmd_run(&id, bundle.as_deref()),
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
    let exit_code =
        unsafe { oci_runtime_core::launch::run(&bundle, &rootfs) }.context("running container")?;

    // The container's own exit code becomes ours, matching runc/crun's
    // `run`: exit code 0 must mean "the container's process exited 0",
    // not merely "ocirun didn't error", so this bypasses
    // oci_cli_common::run_main's usual Ok(())-means-success mapping.
    std::process::exit(exit_code);
}
