//! Uniform error rendering and process exit handling.

use std::process::ExitCode;

/// Run the fallible body of a binary's `main`, rendering any error chain to
/// stderr in the standard oci-tools format and mapping the result to an exit
/// code (0 on success, 1 on error).
///
/// ```no_run
/// fn main() -> std::process::ExitCode {
///     oci_cli_common::run_main(|| {
///         // parse CLI, init logging, dispatch...
///         Ok(())
///     })
/// }
/// ```
pub fn run_main(body: impl FnOnce() -> anyhow::Result<()>) -> ExitCode {
    match body() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            report(&err);
            ExitCode::FAILURE
        }
    }
}

/// Print an error chain to stderr in the standard format.
pub fn report(err: &anyhow::Error) {
    eprintln!("{}", render(err));
}

/// Render an error chain as `error: <top>` followed by one indented
/// `caused by:` line per source.
pub fn render(err: &anyhow::Error) -> String {
    use std::fmt::Write as _;

    let mut out = format!("error: {err}");
    for cause in err.chain().skip(1) {
        // Writing to a String cannot fail.
        let _ = write!(out, "\n  caused by: {cause}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context as _;

    #[test]
    fn renders_single_error() {
        let err = anyhow::anyhow!("boom");
        assert_eq!(render(&err), "error: boom");
    }

    #[test]
    fn renders_error_chain() {
        let err = std::io::Error::new(std::io::ErrorKind::NotFound, "no such blob");
        let err = anyhow::Result::<()>::Err(err.into())
            .context("pulling layer 3")
            .context("pulling image quay.io/x")
            .unwrap_err();

        let rendered = render(&err);
        assert_eq!(
            rendered,
            "error: pulling image quay.io/x\n  \
             caused by: pulling layer 3\n  \
             caused by: no such blob"
        );
    }
}
