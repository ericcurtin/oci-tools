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

    /// Force `--log-level debug` — matching real `podman`/`docker
    /// -D`'s own identical "Docker compatibility" flag exactly
    /// (checked directly, `~/git/podman/cmd/podman/root.go:716-717`:
    /// `lFlags.BoolVarP(&debug, "debug", "D", false, "Docker
    /// compatibility, force setting of log-level")`), and the same
    /// real intent `runc --debug`/`crun --debug` each have too
    /// (`~/git/runc/main.go:106-109,201-203`'s own plain `logrus.
    /// SetLevel(logrus.DebugLevel)`; `~/git/crun/src/crun.c:228,
    /// 291-292`'s own `LIBCRUN_VERBOSITY_DEBUG`) — a real, previously
    /// entirely missing flag on every one of this project's own
    /// binaries; see [`crate::logging::init`]'s own doc comment for
    /// the exact, checked-directly conflict-with-`--log-level` rule
    /// this mirrors from real podman's own identical `loggingHook`.
    /// Real podman's own identical flag is hidden from its own
    /// `--help` (`root.go:717`'s own `MarkHidden("debug")`) — a
    /// deliberate divergence here: this project's own established
    /// convention documents every flag's real semantics directly in
    /// `--help` (this doc comment itself), so hiding it would only
    /// obscure a real, working flag for no functional reason.
    #[arg(short = 'D', long, global = true)]
    pub debug: bool,
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
        assert!(!cli.global.debug);
    }

    #[test]
    fn explicit_values() {
        let cli = TestCli::try_parse_from(["test", "--log-level", "debug", "--json"]).unwrap();
        assert_eq!(cli.global.log_level, "debug");
        assert!(cli.global.json);
    }

    /// `--debug`/`-D` (`0561`) parses on its own -- whether combining
    /// it with a non-default `--log-level` is actually allowed is
    /// [`crate::logging::init`]'s own concern, not clap's; this only
    /// proves both the long and short spelling are accepted at all.
    #[test]
    fn debug_flag_and_its_short_alias_parse() {
        assert!(
            TestCli::try_parse_from(["test", "--debug"])
                .unwrap()
                .global
                .debug
        );
        assert!(
            TestCli::try_parse_from(["test", "-D"])
                .unwrap()
                .global
                .debug
        );
        assert!(!TestCli::try_parse_from(["test"]).unwrap().global.debug);
    }
}
