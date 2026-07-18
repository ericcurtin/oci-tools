//! `ocicri` — Kubernetes CRI implementation (cri-o equivalent).
//!
//! gRPC server implementing the Kubernetes CRI (`RuntimeService` +
//! `ImageService`) on a unix socket, backed by `oci-store`,
//! `oci-runtime-core`, and `oci-net` (CNI). Arrives with milestone 7
//! (critest subset: pod sandbox via infra process, container lifecycle,
//! image pull, streaming exec/attach/logs).

use clap::Parser;

/// Command-line interface (milestone 1: global flags only; the CRI server
/// arrives with milestone 7).
#[derive(Debug, Parser)]
#[command(
    name = "ocicri",
    about = "Kubernetes CRI server for OCI containers",
    version = oci_cli_common::version::long(env!("CARGO_PKG_VERSION")),
)]
struct Cli {
    #[command(flatten)]
    global: oci_cli_common::GlobalArgs,
}

fn main() -> std::process::ExitCode {
    oci_cli_common::run_main(|| {
        let cli = Cli::parse();
        oci_cli_common::logging::init(&cli.global)?;
        tracing::debug!(
            git_hash = oci_cli_common::version::GIT_HASH,
            "ocicri starting"
        );
        anyhow::bail!(
            "the CRI server is not implemented yet (milestone 1 skeleton); \
             it arrives with milestone 7"
        );
    })
}
