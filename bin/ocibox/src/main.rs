//! `ocibox` — pet-container tool (distrobox equivalent).
//!
//! Creates long-lived pet containers (CentOS Stream 10 and Ubuntu 26.04
//! boxes) with home directory, user, and optional host-socket integration.
//! Uses the engine crates as libraries — never by exec'ing the `ociman`
//! binary. Planned commands (milestone 7): `create`, `enter`, `list`, `rm`,
//! `stop`, `upgrade`, `export`.

use clap::Parser;

/// Command-line interface (milestone 1: global flags only; box management
/// arrives with milestone 7).
#[derive(Debug, Parser)]
#[command(
    name = "ocibox",
    about = "Pet containers with home/user/host integration",
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
            "ocibox starting"
        );
        anyhow::bail!(
            "no subcommands are implemented yet (milestone 1 skeleton); \
             box management arrives with milestone 7"
        );
    })
}
