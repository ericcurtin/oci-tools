//! Logging initialization on top of `tracing-subscriber`.
//!
//! Logs always go to **stderr**: stdout is reserved for command output so
//! `--json` mode stays pipeable.

use anyhow::Context as _;

use crate::args::GlobalArgs;

/// The `--log-level` default every `GlobalArgs` instance starts with
/// (`GlobalArgs::log_level`'s own `default_value`) — matching real
/// podman's own identical `defaultLogLevel` (`~/git/podman/cmd/
/// podman/root.go:98-99`), and this function's own reference point
/// for detecting whether `--log-level` was *also* explicitly given
/// alongside `--debug` (see [`init`]'s own doc comment).
const DEFAULT_LOG_LEVEL: &str = "warn";

/// Initialize global logging from the shared CLI flags. `--debug`/
/// `-D` (`0561`), when given, forces the filter to `"debug"` exactly
/// like real `podman`/`docker -D`'s own identical flag — matching
/// real podman's own checked-directly `loggingHook` exactly (`~/git/
/// podman/cmd/podman/root.go:492-500`): combining it with an
/// explicit, non-default `--log-level` is a real, immediate error
/// (podman's own exact wording, `"Setting --log-level and --debug is
/// not allowed"`), never a silent "one wins" resolution.
pub fn init(args: &GlobalArgs) -> anyhow::Result<()> {
    if args.debug {
        anyhow::ensure!(
            args.log_level == DEFAULT_LOG_LEVEL,
            "Setting --log-level and --debug is not allowed"
        );
        return init_with_filter("debug");
    }
    init_with_filter(&args.log_level)
}

/// Initialize global logging from an
/// [`EnvFilter`](tracing_subscriber::EnvFilter) directive string.
pub fn init_with_filter(filter: &str) -> anyhow::Result<()> {
    let filter = tracing_subscriber::EnvFilter::try_new(filter)
        .with_context(|| format!("invalid log filter {filter:?} (try --log-level debug)"))?;

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init()
        .map_err(|err| anyhow::anyhow!("failed to initialize logging: {err}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_filter() {
        let err = init_with_filter("foo=bar=baz").unwrap_err();
        assert!(
            err.to_string().contains("invalid log filter"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn accepts_plain_level_and_directives() {
        // Only parse here; actually installing the global subscriber twice
        // would fail, and other tests may have installed one already.
        for filter in ["warn", "debug", "oci_registry=trace,info"] {
            tracing_subscriber::EnvFilter::try_new(filter)
                .unwrap_or_else(|err| panic!("filter {filter:?} should parse: {err}"));
        }
    }

    /// `--debug` (`0561`) combined with a non-default `--log-level`
    /// is a real, immediate error, matching real podman's own
    /// `loggingHook` exactly -- fires before `init_with_filter` ever
    /// reaches its own `try_init()` call, the same "safe to run
    /// alongside every other test in this same process" property
    /// [`rejects_invalid_filter`] above already relies on.
    #[test]
    fn debug_flag_conflicts_with_a_non_default_log_level() {
        let args = GlobalArgs {
            log_level: "error".to_string(),
            json: false,
            debug: true,
        };
        let err = init(&args).unwrap_err();
        assert!(
            err.to_string()
                .contains("Setting --log-level and --debug is not allowed"),
            "unexpected error: {err:#}"
        );
    }
}
