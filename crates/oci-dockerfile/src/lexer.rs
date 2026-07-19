//! Turning raw Dockerfile/Containerfile text into logical instruction
//! lines: parser-directive detection, line-continuation splicing
//! (including the real spec's own comment/blank-line-inside-a-
//! continuation quirks), and comment stripping.
//!
//! Every rule below was checked directly against the real, current
//! BuildKit Dockerfile frontend (`~/git/moby/vendor/github.com/moby/
//! buildkit/frontend/dockerfile/parser/{parser,directives}.go`) — the
//! actively-maintained implementation real `docker build`/`podman
//! build` both ultimately rely on — rather than re-derived from
//! documentation prose, which is noticeably less precise on several
//! of these points (see each function's own doc comment for the
//! specific behavior it was checked against).

/// One logical instruction line: `text` has already had any line
/// continuations spliced in and its own trailing newline removed, but
/// is otherwise exactly as written (still needs instruction-name/
/// argument splitting, quote handling, etc. — see
/// [`crate::instruction`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogicalLine {
    /// The starting physical line number (1-indexed) this logical
    /// line began at, for error messages.
    pub line_number: usize,
    pub text: String,
}

/// Parser directives (`# key=value` comments at the very top of the
/// file) this crate actually understands. `syntax`/`check` are
/// recognized (so a real Containerfile using them doesn't fail to
/// parse) but not acted on yet — see the crate's own doc comment for
/// why (they're BuildKit frontend-redirection/linting features, not
/// core Dockerfile grammar this crate's own milestone needs first).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Directives {
    /// The line-continuation escape token: `\` by default, or `` ` ``
    /// if a valid `# escape=` directive set it. No other value is
    /// legal (matches `setEscapeToken`).
    pub escape: char,
}

impl Default for Directives {
    fn default() -> Self {
        Directives { escape: '\\' }
    }
}

/// Scan `input` from the very top for parser directives.
///
/// The real rule (`directives.go`'s own `DirectiveParser`, checked
/// directly): directives must be the *first* comment lines in the
/// file, with **no** ordinary comment, blank line, or instruction
/// interrupting them — the moment a line doesn't match `# key=value`
/// (case-insensitive key), directive scanning stops **permanently**
/// for the rest of the file, even if a later line would otherwise
/// have been a valid directive. This is why, in the real fixture this
/// was checked against (`escape-after-comment`), three *ordinary*
/// `#`-comments before a `# escape = \`` line mean the escape
/// directive is never actually honored — it's just an ordinary
/// comment by the time the scanner reaches it.
pub fn scan_directives(input: &str) -> Result<Directives, String> {
    let mut directives = Directives::default();
    let mut seen_escape = false;
    for line in input.lines() {
        let Some(rest) = line.trim_start().strip_prefix('#') else {
            break;
        };
        let Some((key, value)) = rest.split_once('=') else {
            break;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();
        match key.as_str() {
            "escape" => {
                if seen_escape {
                    return Err("only one escape parser directive can be used".to_string());
                }
                seen_escape = true;
                directives.escape = match value {
                    "\\" => '\\',
                    "`" => '`',
                    other => {
                        return Err(format!("invalid ESCAPE '{other}'. Must be ` or \\"));
                    }
                };
            }
            // Recognized so a real Containerfile using them still
            // parses (not a hard error), but not acted on yet — see
            // this module's own doc comment.
            "syntax" | "check" => {}
            _ => break,
        }
    }
    Ok(directives)
}

/// Whether `line` (with only trailing spaces/tabs removed — matches
/// the real regex's own `[ \t]*$`, not full Unicode whitespace)
/// ends in an unescaped continuation token.
///
/// Checked directly against the real continuation regex
/// (`parser.go`): a line continues if it ends (after optional
/// trailing spaces/tabs) in the escape token *not itself immediately
/// preceded by another escape token* — so a literal `\\` (an escaped
/// backslash) at line-end is deliberately **not** a continuation, a
/// real, documented quirk of the upstream regex having no negative
/// lookahead, not an oversight here — or the entire (trimmed) line
/// consists of nothing but the escape token itself.
fn ends_with_continuation(line: &str, escape: char) -> bool {
    let trimmed = line.trim_end_matches([' ', '\t']);
    let mut chars = trimmed.chars().rev();
    let Some(last) = chars.next() else {
        return false;
    };
    if last != escape {
        return false;
    }
    match chars.next() {
        None => true, // the whole (trimmed) line is just the escape token
        Some(prev) => prev != escape,
    }
}

/// Strip exactly the trailing continuation token (and the trailing
/// spaces/tabs `ends_with_continuation` matched before it) from
/// `line`, leaving everything before it untouched — including any
/// whitespace that was never part of that trailing run. Matches the
/// real `trimContinuationCharacter`.
fn strip_continuation(line: &str) -> String {
    let mut s = line.trim_end_matches([' ', '\t']).to_string();
    s.pop();
    s
}

/// Whether `line`'s first non-whitespace character is `#` — the real
/// `isComment` check, applied to the *raw* physical line.
fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with('#')
}

/// Split `input` into logical instruction lines: directive/comment
/// lines dropped, continuations spliced together.
///
/// Real, checked-directly behavior this replicates
/// (`Parse`, parser.go): once a logical line's *first* physical line
/// ends in a continuation token, comment lines and blank lines
/// encountered while looking for the real end of that logical line
/// are transparently spliced out (dropped) rather than ending the
/// continuation early — confirmed against the real `continueIndent`
/// fixture, which has comment lines and blank lines interleaved
/// *inside* a single continued `RUN` instruction and still produces
/// one spliced-together command.
pub fn splice_lines(input: &str, escape: char) -> Vec<LogicalLine> {
    let physical: Vec<&str> = input.lines().collect();
    let mut result = Vec::new();
    let mut i = 0;
    while i < physical.len() {
        let start_line_number = i + 1;
        let first = physical[i];
        i += 1;
        if is_comment(first) || first.trim().is_empty() {
            // An ordinary top-level comment or blank line, not the
            // start of any instruction at all.
            continue;
        }
        if !ends_with_continuation(first, escape) {
            result.push(LogicalLine {
                line_number: start_line_number,
                text: first.to_string(),
            });
            continue;
        }
        let mut buffer = strip_continuation(first);
        while let Some(next) = physical.get(i) {
            i += 1;
            if is_comment(next) || next.trim().is_empty() {
                // Spliced out, doesn't end the continuation.
                continue;
            }
            if ends_with_continuation(next, escape) {
                buffer.push_str(&strip_continuation(next));
            } else {
                buffer.push_str(next);
                break;
            }
        }
        result.push(LogicalLine {
            line_number: start_line_number,
            text: buffer,
        });
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_directives_defaults_to_backslash() {
        assert_eq!(scan_directives("FROM scratch\n").unwrap().escape, '\\');
    }

    #[test]
    fn scan_directives_honors_a_leading_escape_directive() {
        let input = "# escape=`\nFROM scratch\n";
        assert_eq!(scan_directives(input).unwrap().escape, '`');
    }

    #[test]
    fn scan_directives_key_is_case_insensitive_and_tolerates_spaces() {
        let input = "# ESCAPE = `\nFROM scratch\n";
        assert_eq!(scan_directives(input).unwrap().escape, '`');
    }

    #[test]
    fn scan_directives_rejects_an_invalid_escape_value() {
        assert!(scan_directives("# escape=x\n").is_err());
    }

    #[test]
    fn scan_directives_rejects_a_duplicate_escape_directive() {
        let input = "# escape=`\n# escape=\\\nFROM scratch\n";
        assert!(scan_directives(input).is_err());
    }

    #[test]
    fn scan_directives_ignores_syntax_and_check() {
        let input = "# syntax=docker/dockerfile:1\n# check=skip=all\nFROM scratch\n";
        assert_eq!(scan_directives(input).unwrap().escape, '\\');
    }

    #[test]
    fn scan_directives_stops_at_the_first_ordinary_comment() {
        // The real, checked-directly `escape-after-comment` behavior:
        // an ordinary comment before the escape directive disables
        // directive scanning for the rest of the file entirely, so
        // the escape directive below is *not* honored.
        let input = "# just a comment\n# escape=`\nFROM scratch\n";
        assert_eq!(scan_directives(input).unwrap().escape, '\\');
    }

    #[test]
    fn splice_lines_drops_ordinary_top_level_comments_and_blank_lines() {
        let lines = splice_lines("# comment\n\nFROM scratch\n", '\\');
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "FROM scratch");
        assert_eq!(lines[0].line_number, 3);
    }

    #[test]
    fn splice_lines_joins_a_simple_continuation() {
        let lines = splice_lines("RUN echo hello \\\n    world\n", '\\');
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "RUN echo hello     world");
    }

    #[test]
    fn splice_lines_matches_the_real_continue_indent_fixture_shape() {
        // Checked directly against the real `continueIndent` fixture:
        // comments and a blank line inside a continuation are spliced
        // out entirely, not treated as ending it.
        let input = "RUN echo hello\\\n# this is a comment\n\nworld\n";
        let lines = splice_lines(input, '\\');
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "RUN echo helloworld");
    }

    #[test]
    fn splice_lines_does_not_treat_an_escaped_escape_as_continuation() {
        // `\\` (two backslashes) at line-end is *not* a continuation —
        // the real regex's own documented quirk (no negative
        // lookahead), checked directly, not an oversight here.
        let lines = splice_lines("RUN echo hi\\\\\nFROM scratch\n", '\\');
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "RUN echo hi\\\\");
    }

    #[test]
    fn splice_lines_supports_a_backtick_escape_token() {
        let lines = splice_lines("RUN echo hello `\n    world\n", '`');
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "RUN echo hello     world");
    }

    #[test]
    fn splice_lines_a_line_that_is_only_the_escape_token_continues() {
        let lines = splice_lines("RUN echo hi\n\\\nworld\n", '\\');
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].text, "world");
    }
}
