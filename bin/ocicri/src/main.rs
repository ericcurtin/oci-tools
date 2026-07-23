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
//! container lifecycle's own record slice (`CreateContainer`/
//! `ContainerStatus`/`ListContainers`/`RemoveContainer` — every
//! record honestly `CONTAINER_CREATED` until `StartContainer` itself
//! exists, with a real, verified launch-ready bundle prepared at
//! create time, see `docs/design/0236`-`0237`), and all of `ImageService`
//! (`ListImages`/`StreamImages`/`ImageStatus`/`PullImage`/
//! `RemoveImage`/`ImageFsInfo`, reusing this project's own
//! already-tested `oci_store`/`oci_registry` primitives directly —
//! see `image_service.rs`'s own module doc comment). Every remaining
//! RPC (start/stop, exec/attach/port-forward, stats, events, ...)
//! deliberately returns a real `Status::unimplemented` naming
//! itself, rather than accepting a request this project can't
//! actually act on yet.
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

mod bundle;
mod container;
mod image_service;
mod records;
mod runtime_service;
mod sandbox;
mod stream;

use std::path::PathBuf;

use anyhow::Context as _;
use clap::Parser;
use oci_cri_types as cri;

/// Command-line interface. Real `cri-o` itself has no subcommands at
/// all — invoking it just *is* running the server — so neither does
/// `ocicri`; global flags plus `--listen` are everything this first
/// slice needs.
#[derive(Debug, Parser)]
#[command(
    name = "ocicri",
    about = "Kubernetes CRI server for OCI containers",
    version = oci_cli_common::version::long(env!("CARGO_PKG_VERSION")),
)]
struct Cli {
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

fn default_socket_path() -> PathBuf {
    oci_cli_common::runtime_root::default_root("ocicri").join("ocicri.sock")
}

fn main() -> std::process::ExitCode {
    oci_cli_common::run_main(|| {
        let cli = Cli::parse();
        oci_cli_common::logging::init(&cli.global)?;
        tracing::debug!(
            git_hash = oci_cli_common::version::GIT_HASH,
            "ocicri starting"
        );

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
