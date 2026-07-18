//! `ociboot` — bootable-container OS manager (bootc equivalent).
//!
//! Manages transactional OS deployments built from OCI images: flattened
//! erofs images sealed with fsverity, BLS boot entries, boot counting with
//! auto-rollback, persistent /var and three-way-merged /etc — with no
//! dependency on ostree or composefs.
//!
//! Milestone plan: `install to-disk` + boot flow (milestone 5);
//! `upgrade`/`switch`/`rollback`/`status`/`gc`, /etc merge, boot counting,
//! layered mode (milestone 6). Shares `oci-registry`/`oci-store` with
//! `ociman` — one pull path for containers and OS images alike.

use clap::Parser;

/// Command-line interface (milestone 1: global flags only; `install` arrives
/// with milestone 5).
#[derive(Debug, Parser)]
#[command(
    name = "ociboot",
    about = "Bootable-container OS manager (erofs + fsverity, no ostree)",
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
            "ociboot starting"
        );
        anyhow::bail!(
            "no subcommands are implemented yet (milestone 1 skeleton); \
             `install` arrives with milestone 5"
        );
    })
}
