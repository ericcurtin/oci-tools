//! `ociman` — daemonless container engine for OCI images (podman equivalent).
//!
//! Thin frontend: all engine logic lives in `crates/*` (`oci-registry`,
//! `oci-store`, `oci-runtime-core`, `oci-dockerfile`, `oci-net`).
//!
//! Milestone plan: `pull`/`images`/`inspect` (milestone 2),
//! `run`/`exec`/`ps`/`logs` rootless (milestone 3), `build` (milestone 4),
//! then the full podman-style v1 command set.

use clap::Parser;

/// Command-line interface (milestone 1: global flags only; subcommands arrive
/// with their milestones).
#[derive(Debug, Parser)]
#[command(
    name = "ociman",
    about = "Daemonless container engine for OCI images",
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
            "ociman starting"
        );
        anyhow::bail!(
            "no subcommands are implemented yet (milestone 1 skeleton); \
             `pull`, `images`, and `inspect` arrive with milestone 2"
        );
    })
}
