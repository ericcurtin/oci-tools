//! `ocicri` — Kubernetes CRI implementation (cri-o equivalent).
//!
//! A real gRPC server implementing the Kubernetes CRI v1 protocol
//! (`oci_cri_types`'s own `proto/api.proto`, vendored unmodified from
//! real `cri-o`'s own `k8s.io/cri-api` — see its own `proto/
//! README.md`) over a Unix domain socket, matching real `cri-o`'s own
//! `crio.sock` model exactly
//! (kubelet talks to *either* real runtime over the identical wire
//! protocol; nothing about this project's own socket path or listener
//! setup is CRI-specific in a way a real `crictl`/kubelet couldn't
//! talk to).
//!
//! Genuinely implemented so far: `RuntimeService.Version`/`Status`/
//! `RuntimeConfig`/`UpdateRuntimeConfig`/`ListMetricDescriptors`, the
//! full pod-sandbox lifecycle (`RunPodSandbox`/`StopPodSandbox`/
//! `RemovePodSandbox`/`PodSandboxStatus`/`ListPodSandbox`/
//! `StreamPodSandboxes` — a real, persistent, record-keeping state
//! machine with real CRI semantics, deliberately no infra
//! process/pinned namespaces yet, see `docs/design/0233`-`0234`), the
//! full container lifecycle (`CreateContainer` with a real, verified
//! launch-ready bundle prepared at create time, `StartContainer`/
//! `StopContainer` running real container processes via the
//! per-container launcher-keeper — this project's own conmon
//! equivalent — plus `ContainerStatus`/`ListContainers`/
//! `RemoveContainer` reconciling against the keeper's own real exit
//! records, `ExecSync` — kubelet's own exec probes — via a second
//! hidden re-exec entry point, real cgroup-backed container
//! stats, and real CRI-format log files at kubelet's own log path
//! including `ReopenContainerLog` rotation support, see
//! `docs/design/0236`-`0238`, `0240`-`0243`),
//! and all of `ImageService`
//! (`ListImages`/`StreamImages`/`ImageStatus`/`PullImage`/
//! `RemoveImage`/`ImageFsInfo`, reusing this project's own
//! already-tested `oci_store`/`oci_registry` primitives directly —
//! see `image_service.rs`'s own module doc comment). Every remaining
//! RPC (exec/attach/port-forward, stats, events, ...) deliberately
//! returns a real `Status::unimplemented` naming itself, rather than
//! accepting a request this project can't actually act on yet.
//!
//! Unlike every other binary in this workspace, `ocicri` is a real,
//! long-lived server process, not a short-lived CLI invocation — the
//! one deliberate exception to this project's own "beat every
//! benchmark, especially startup time" design pillar, since a
//! server's own *serving* performance (not its own one-time process
//! startup) is what actually matters here. This is also the only
//! binary in the workspace linking `tokio`/`tonic`/`prost`: every
//! other binary's own hot per-invocation startup path is completely
//! unaffected.
//!
//! A real subcommand *does* exist, though — `ocicri version`
//! (`docs/design/0532`), correcting an earlier, checked-directly-wrong
//! claim this module's own doc comment used to make here ("real
//! `cri-o` itself has no subcommands at all"): real `crio` does,
//! checked directly (`~/git/cri-o/cmd/crio/main.go:161-168`:
//! `app.Commands = criocli.DefaultCommands` plus `CheckCommand`/
//! `ConfigCommand`/`PublishCommand`/`StatusCommand`/`VersionCommand`/
//! `WipeCommand`) — a bare invocation with no subcommand at all is
//! what runs the server (`app.Action`), the exact same "no
//! subcommand" default this project's own `Cli::command: Option
//! <Command>` still uses.

mod bundle;
mod container;
mod image_service;
mod launcher;
mod records;
mod runtime_service;
mod sandbox;
mod stream;

use std::path::PathBuf;

use anyhow::Context as _;
use clap::Parser;
use oci_cri_types as cri;

/// Command-line interface. A bare invocation (`command: None`) starts
/// the server — matching real bare `crio`'s own identical `app.
/// Action` default exactly (see this module's own doc comment for why
/// real `crio` *does* have real subcommands too, despite what an
/// earlier version of this doc comment used to claim). Global flags
/// plus `--listen` apply either way.
#[derive(Debug, Parser)]
#[command(
    name = "ocicri",
    about = "Kubernetes CRI server for OCI containers",
    version = oci_cli_common::version::long(env!("CARGO_PKG_VERSION")),
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    global: oci_cli_common::GlobalArgs,
    /// Unix domain socket path to listen on — matching real `cri-o`'s
    /// own `--listen` flag exactly (its own default is
    /// `/var/run/crio/crio.sock`). Defaults to `ocicri.sock` under
    /// this project's own shared runtime-root convention
    /// (`oci_cli_common::runtime_root`, the same one `ocirun --root`'s
    /// own default already uses: `/run/ocicri` for root,
    /// `$XDG_RUNTIME_DIR/ocicri` rootless).
    #[arg(long = "listen", value_name = "PATH")]
    listen: Option<PathBuf>,
}

/// See [`Cli::command`]'s own doc comment. Only `version` so far —
/// real `crio`'s own other subcommands (`check`/`config`/`publish`/
/// `status`/`wipe`) are all real, separate, much bigger gaps
/// (respectively: a standalone healthcheck-config CLI, a config-file
/// generator/validator, a systemd-notify-socket publisher, a runtime
/// status dump, and an on-disk-state wipe tool) — each its own future
/// increment, not folded in here.
#[derive(Debug, clap::Subcommand)]
enum Command {
    /// "display detailed version information" (real `crio version`'s
    /// own `Usage` string, quoted verbatim) — matching real `cri-o
    /// version` exactly (checked directly, `~/git/cri-o/internal/
    /// criocli/version.go:17-49`). Real `crio`'s own separate
    /// `--json`/`-j` flag on this specific subcommand is folded into
    /// this project's own already-global `--json` instead (matching
    /// every other `ocicri`/`ociman` command's own identical
    /// convention) rather than a second, redundant one.
    Version,
}

fn default_socket_path() -> PathBuf {
    oci_cli_common::runtime_root::default_root("ocicri").join("ocicri.sock")
}

fn main() -> std::process::ExitCode {
    // The internal per-container launcher-keeper re-exec
    // (`docs/design/0238`, see `launcher.rs`'s own module doc
    // comment) -- intercepted before clap or tokio ever run, so the
    // launcher process is genuinely single-threaded at its own
    // `oci_runtime_core::launch` call, exactly like `runc init`'s own
    // hidden re-exec entry point. Never reachable from any real CLI
    // surface (`__launch` appears in no help text).
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some(launcher::LAUNCH_ARGV1) {
        launcher::main(&args[2..]);
    }
    if args.get(1).map(String::as_str) == Some(launcher::EXEC_ARGV1) {
        launcher::exec_main(&args[2..]);
    }

    oci_cli_common::run_main(|| {
        let cli = Cli::parse();
        oci_cli_common::logging::init(&cli.global)?;
        tracing::debug!(
            git_hash = oci_cli_common::version::GIT_HASH,
            "ocicri starting"
        );

        if let Some(Command::Version) = cli.command {
            return cmd_version(cli.global.json);
        }

        let socket_path = cli.listen.unwrap_or_else(default_socket_path);

        // A real, long-lived server needs a real async runtime to
        // drive it -- the one place in this whole workspace `tokio`
        // is used at all (see this module's own doc comment for why
        // that's fine: `ocicri` is a server, not a hot-path CLI
        // invocation).
        let runtime = tokio::runtime::Runtime::new().context("starting the tokio runtime")?;
        runtime.block_on(serve(&socket_path))
    })
}

async fn serve(socket_path: &std::path::Path) -> anyhow::Result<()> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    // A stale socket file from a previous, uncleanly-terminated run
    // would otherwise make `UnixListener::bind` fail with `EADDRINUSE`
    // -- matching real `cri-o`'s own identical "remove any existing
    // socket before binding" startup behavior.
    match std::fs::remove_file(socket_path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(e).with_context(|| format!("removing stale {}", socket_path.display()));
        }
    }

    let listener = tokio::net::UnixListener::bind(socket_path)
        .with_context(|| format!("binding unix socket {}", socket_path.display()))?;
    let incoming = tokio_stream::wrappers::UnixListenerStream::new(listener);

    tracing::info!(socket = %socket_path.display(), "ocicri listening");

    tonic::transport::Server::builder()
        .add_service(cri::runtime_service_server::RuntimeServiceServer::new(
            runtime_service::RuntimeServiceImpl::default(),
        ))
        .add_service(cri::image_service_server::ImageServiceServer::new(
            image_service::ImageServiceImpl,
        ))
        .serve_with_incoming(incoming)
        .await
        .context("serving CRI gRPC requests")
}

/// `ocicri version`'s own report — the subset of real `crio version`'s
/// own `Info` struct (`~/git/cri-o/internal/version/version.go:35-49`)
/// this project has an honest, directly-checkable value for
/// (`Version`/`GitCommit`/`Platform`), the same "keep the field names
/// real crio itself uses, only for the fields with an honest value"
/// shape `ociman version`'s own `VersionReport` already established
/// for real `podman version` — deliberately omitting `GoVersion`/
/// `Compiler`/`Linkmode`/`BuildTags`/`LDFlags` (this project isn't
/// Go), `GitCommitDate`/`BuildDate` (no build-time timestamp
/// embedding here, only the git hash), `GitTreeState` (no working-
/// tree-dirty detection at build time), `SeccompEnabled`/
/// `AppArmorEnabled` (no seccomp/AppArmor subsystem in this project at
/// all yet), and `Dependencies` (real crio's own `--verbose`-only Go
/// module list — not accepted here either, the exact same "real
/// upstream field/flag with no honest Rust equivalent" reasoning).
/// JSON key casing matches this project's own already-established
/// `snake_case` convention (`ociman version`'s own `VersionReport`),
/// not real crio's own `camelCase` struct tags — checked directly:
/// `ociman version --json`'s own `git_commit`/`os_arch` never chased
/// real podman's own JSON key spelling either, the same precedent.
#[derive(Debug, serde::Serialize)]
struct VersionReport {
    version: String,
    git_commit: String,
    platform: String,
}

fn version_report() -> VersionReport {
    let platform = oci_spec_types::image::Platform::host();
    VersionReport {
        version: env!("CARGO_PKG_VERSION").to_string(),
        git_commit: oci_cli_common::version::GIT_HASH.to_string(),
        platform: format!("{}/{}", platform.os, platform.architecture),
    }
}

/// Plain-text output deliberately doesn't chase real crio's own
/// reflection-driven `tabwriter` column alignment byte-for-byte
/// (`(*Info).String()`, `~/git/cri-o/internal/version/version.go:228-
/// 273`): that alignment is computed from *all* of real crio's own
/// fields, most of which this report has no honest equivalent for at
/// all (see [`VersionReport`]'s own doc comment) — chasing an exact
/// column width real crio would only ever produce with fields this
/// project can't honestly populate would be cargo-culting, not real
/// compatibility. The three real field *names* below are still real
/// crio's own exact identifiers, unlike `ociman version`'s own
/// `podman`-style `"Git Commit"`/`"OS/Arch"` labels.
fn cmd_version(json: bool) -> anyhow::Result<()> {
    let report = version_report();
    if json {
        oci_cli_common::output::print_json(&report)?;
        return Ok(());
    }
    println!("Version:    {}", report.version);
    println!("GitCommit:  {}", report.git_commit);
    println!("Platform:   {}", report.platform);
    Ok(())
}
