//! Global command-line arguments shared by every oci-tools binary.

/// Flags accepted by all oci-tools binaries, flattened into each CLI via
/// `#[command(flatten)]`.
#[derive(Debug, Clone, clap::Args)]
pub struct GlobalArgs {
    /// Log filter: error, warn, info, debug, trace, or any tracing
    /// EnvFilter directive (e.g. "oci_registry=debug,warn"). Logs go to
    /// stderr.
    #[arg(
        long,
        global = true,
        env = "OCI_TOOLS_LOG",
        default_value = "warn",
        value_name = "FILTER"
    )]
    pub log_level: String,

    /// Emit machine-readable JSON on stdout (for commands that support it).
    #[arg(long, global = true)]
    pub json: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Debug, Parser)]
    struct TestCli {
        #[command(flatten)]
        global: GlobalArgs,
    }

    #[test]
    fn defaults() {
        let cli = TestCli::try_parse_from(["test"]).unwrap();
        assert_eq!(cli.global.log_level, "warn");
        assert!(!cli.global.json);
    }

    #[test]
    fn explicit_values() {
        let cli = TestCli::try_parse_from(["test", "--log-level", "debug", "--json"]).unwrap();
        assert_eq!(cli.global.log_level, "debug");
        assert!(cli.global.json);
    }
}
