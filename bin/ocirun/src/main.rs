//! `ocirun` — standalone OCI runtime (crun equivalent).
//!
//! Thin, runc-CLI-compatible wrapper over `oci-runtime-core`, so it can be
//! dropped into other engines. Planned commands (milestone 3): `create`,
//! `start`, `state`, `kill`, `delete`, `exec`, `run`, `spec`, `features`.

use clap::Parser;

/// Command-line interface (milestone 1: global flags only; the OCI runtime
/// command set arrives with milestone 3).
#[derive(Debug, Parser)]
#[command(
    name = "ocirun",
    about = "OCI runtime: create/start/kill containers per the OCI runtime spec",
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
            "ocirun starting"
        );
        anyhow::bail!(
            "no subcommands are implemented yet (milestone 1 skeleton); \
             the OCI runtime command set arrives with milestone 3"
        );
    })
}
