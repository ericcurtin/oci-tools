//! Structured (`--json`) output helpers.
//!
//! Commands that support `--json` print exactly one JSON document to stdout
//! and keep all diagnostics on stderr.

use std::io::Write as _;

/// Serialize `value` as pretty-printed JSON.
pub fn json_string<T: serde::Serialize>(value: &T) -> anyhow::Result<String> {
    Ok(serde_json::to_string_pretty(value)?)
}

/// Print `value` as pretty-printed JSON on stdout, followed by a newline.
pub fn print_json<T: serde::Serialize>(value: &T) -> anyhow::Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, value)?;
    writeln!(stdout)?;
    Ok(())
}

/// Print `value` as compact single-line JSON on stdout (one document per
/// line; suitable for streaming records).
pub fn print_json_line<T: serde::Serialize>(value: &T) -> anyhow::Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, value)?;
    writeln!(stdout)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Serialize)]
    struct Sample {
        name: &'static str,
        size: u64,
    }

    #[test]
    fn pretty_json_round_trips() {
        let s = json_string(&Sample {
            name: "blob",
            size: 42,
        })
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed["name"], "blob");
        assert_eq!(parsed["size"], 42);
        // Pretty output is multi-line.
        assert!(s.contains('\n'));
    }
}
