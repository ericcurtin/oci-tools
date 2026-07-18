//! Logging initialization on top of `tracing-subscriber`.
//!
//! Logs always go to **stderr**: stdout is reserved for command output so
//! `--json` mode stays pipeable.

use anyhow::Context as _;

use crate::args::GlobalArgs;

/// Initialize global logging from the shared CLI flags.
pub fn init(args: &GlobalArgs) -> anyhow::Result<()> {
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
}
