//! Dockerfile-style `$VAR`/`${VAR}` variable expansion — the engine
//! wired into most [`crate::Instruction`] variants via
//! [`crate::expand_stage`]/[`crate::expand_meta_args`] (`docs/design/
//! 0042`; deliberately *not* `RUN`/`CMD`/`ENTRYPOINT`/`SHELL`/
//! `HEALTHCHECK`/`ONBUILD`'s own command-line text — see `expand_
//! stage`'s own doc comment for why not).
//!
//! Every rule here was checked directly against BuildKit's own
//! implementation (`~/git/moby/vendor/github.com/moby/buildkit/
//! frontend/dockerfile/shell/lex.go`) and its own golden test fixture
//! (`frontend/dockerfile/shell/envVarTest`, data-only — the Go test
//! file that drives it isn't vendored, but the fixture rows
//! themselves are real, exact expected input/output pairs, not
//! reconstructed from documentation prose).
//!
//! # Supported syntax
//!
//! * `$VAR` — a bare variable reference. The name may contain
//!   letters, digits, and underscore; scanning stops at the first
//!   character that's none of those. If the name would start with a
//!   digit, *only* digits are consumed (bash's positional-parameter
//!   convention, e.g. `$1`), never letters after that.
//! * `${VAR}` — the same name rule, braced.
//! * `${VAR:-word}` / `${VAR-word}` — use `word` if `VAR` is unset (or,
//!   for the `:`-form specifically, also if it's set but empty);
//!   otherwise use `VAR`'s own value. `word` is itself recursively
//!   expanded (so `${A:-${B:-c}}` works).
//! * `${VAR:+word}` / `${VAR+word}` — the mirror image: use `word` if
//!   `VAR` *is* set (and, for the `:`-form, also non-empty);
//!   otherwise an empty string.
//! * `${VAR:?message}` / `${VAR?message}` — a hard error (rather than
//!   silently expanding to empty) if `VAR` is unset (or, for the
//!   `:`-form, unset-or-empty), with `message` in the error.
//! * A backslash immediately before `$` escapes it, producing a
//!   literal `$` (so `\$FOO`/`\${FOO}` become the literal text `$FOO`/
//!   `${FOO}`, not an expansion) — a backslash *not* followed by `$`
//!   is left completely untouched (this crate's own lexer already
//!   consumes the file's own line-continuation escape token
//!   separately; word-level backslash-escaping only ever applies to
//!   `$` itself, matching the real implementation exactly, not a
//!   general-purpose escape mechanism).
//! * A reference to a name that was never declared at all expands to
//!   an **empty string**, silently — not an error, and not left as a
//!   literal `$VAR` — matching real BuildKit's own default (non-lint)
//!   behavior exactly (it separately emits a *lint warning* for this
//!   case, which this crate doesn't implement any linting for at
//!   all).
//! * `$$` is **not** a literal-dollar escape (a real, surprising, but
//!   directly-confirmed BuildKit behavior): `$` is one of a small set
//!   of shell "special parameter" names (`@ * # ? - $ !`, alongside
//!   the already-covered leading-digit case), each treated as a
//!   single-character variable name — since none of them is ever
//!   actually declared by anything this crate parses, `$$` expands to
//!   an empty string. Escaping a literal `$` requires a backslash
//!   (`\$`), not doubling it.
//!
//! # Deliberately not supported yet
//!
//! The glob-pattern operators (`${VAR#pattern}`/`${VAR##pattern}`
//! prefix-strip, `${VAR%pattern}`/`${VAR%%pattern}` suffix-strip,
//! `${VAR/pattern/repl}`/`${VAR//pattern/repl}` substitution) — each
//! needs its own glob-to-regex conversion, meaningfully more
//! machinery than the rest of this module, and is rare enough in
//! practice to defer to a later increment; using one of them is a
//! clear parse error here, not a silent misparse.
//!
//! # Wired into [`crate::Instruction`] via per-stage environment scoping
//!
//! Real expansion needs to know the accumulated `ARG`/`ENV`
//! environment *at the point each instruction appears* — which resets
//! at each `FROM` (a new build stage starts with a mostly-fresh
//! environment; only meta-`ARG`s declared before the very first
//! `FROM`, and only if re-declared inside the stage, carry over).
//! [`crate::expand_stage`] (`docs/design/0042`) is what actually
//! threads that scoping rule through the crate's own stage-grouped
//! instruction list (`crate::group_stages`), calling this module's own
//! [`expand`] at each point that needs it. This engine itself was
//! shipped and thoroughly tested standalone first, independent of
//! that grouping, matching this project's own established pattern
//! (e.g. `systemd_cgroup::create_scope` in `oci-tools`' own `docs/
//! design/0033`, tested standalone before `docs/design/0034` wired it
//! in).

use std::collections::HashMap;
use std::iter::Peekable;
use std::str::Chars;

/// Expand every `$VAR`/`${VAR...}` reference in `word`, looking values
/// up in `env`. See the module doc comment for the exact supported
/// syntax.
pub fn expand(word: &str, env: &HashMap<String, String>) -> Result<String, String> {
    let mut chars = word.chars().peekable();
    let mut out = String::new();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'$') => {
                chars.next();
                out.push('$');
            }
            '$' => out.push_str(&process_dollar(&mut chars, env)?),
            c => out.push(c),
        }
    }
    Ok(out)
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_special_param(c: char) -> bool {
    matches!(c, '@' | '*' | '#' | '?' | '-' | '$' | '!')
}

/// Read a variable name starting at the iterator's current position:
/// letters/digits/underscore, except that a name starting with a
/// digit only ever consumes further digits (bash's positional-
/// parameter convention).
fn read_name(chars: &mut Peekable<Chars>) -> String {
    let mut name = String::new();
    let Some(&first) = chars.peek() else {
        return name;
    };
    if first.is_ascii_digit() {
        while let Some(&c) = chars.peek() {
            if c.is_ascii_digit() {
                name.push(c);
                chars.next();
            } else {
                break;
            }
        }
    } else {
        while let Some(&c) = chars.peek() {
            if is_name_char(c) {
                name.push(c);
                chars.next();
            } else {
                break;
            }
        }
    }
    name
}

/// Called right after consuming a `$`: dispatches to the braced
/// (`${...}`), bare-name (`$VAR`), or special-parameter (`$$`, `$?`,
/// ...) form, or falls back to a literal `$` if none of those apply
/// (nothing is consumed from `chars` in that fallback case, so
/// whatever follows is processed normally by the caller).
fn process_dollar(
    chars: &mut Peekable<Chars>,
    env: &HashMap<String, String>,
) -> Result<String, String> {
    match chars.peek() {
        Some('{') => {
            chars.next();
            process_braced(chars, env)
        }
        Some(&c) if c.is_ascii_digit() || c.is_alphabetic() || c == '_' => {
            let name = read_name(chars);
            Ok(env.get(&name).cloned().unwrap_or_default())
        }
        Some(&c) if is_special_param(c) => {
            chars.next();
            Ok(env.get(&c.to_string()).cloned().unwrap_or_default())
        }
        _ => Ok("$".to_string()),
    }
}

/// Called right after consuming `${`: reads the name, then dispatches
/// on whatever modifier (if any) follows it, up to the matching
/// (possibly nested) closing `}`.
fn process_braced(
    chars: &mut Peekable<Chars>,
    env: &HashMap<String, String>,
) -> Result<String, String> {
    let name = read_name(chars);
    if name.is_empty() {
        return Err("bad substitution: empty variable name in ${...}".to_string());
    }
    let set = env.contains_key(&name);
    let value = env.get(&name).cloned().unwrap_or_default();
    match chars.next() {
        Some('}') => Ok(value),
        Some(':') => match chars.next() {
            Some('-') => {
                let word = read_until_closing_brace(chars, env)?;
                Ok(if !set || value.is_empty() {
                    word
                } else {
                    value
                })
            }
            Some('+') => {
                let word = read_until_closing_brace(chars, env)?;
                Ok(if set && !value.is_empty() {
                    word
                } else {
                    String::new()
                })
            }
            Some('?') => {
                let message = read_until_closing_brace(chars, env)?;
                if !set || value.is_empty() {
                    Err(format!(
                        "{name}: {}",
                        if message.is_empty() {
                            "is not allowed to be empty".to_string()
                        } else {
                            message
                        }
                    ))
                } else {
                    Ok(value)
                }
            }
            other => Err(format!(
                "bad substitution: unsupported modifier {:?} after \"${{{name}:\"",
                other.unwrap_or(' ')
            )),
        },
        Some('-') => {
            let word = read_until_closing_brace(chars, env)?;
            Ok(if !set { word } else { value })
        }
        Some('+') => {
            let word = read_until_closing_brace(chars, env)?;
            Ok(if set { word } else { String::new() })
        }
        Some('?') => {
            let message = read_until_closing_brace(chars, env)?;
            if !set {
                Err(format!(
                    "{name}: {}",
                    if message.is_empty() {
                        "is not allowed to be unset".to_string()
                    } else {
                        message
                    }
                ))
            } else {
                Ok(value)
            }
        }
        Some(other) => Err(format!(
            "bad substitution: unexpected {other:?} after \"${{{name}\""
        )),
        None => Err(format!("bad substitution: unterminated \"${{{name}\"")),
    }
}

/// Read (and recursively expand any nested `$`/`${...}` references
/// within) everything up to the next unescaped, non-nested closing
/// `}` — the "default"/"alternate"/"error message" word portion of a
/// `${VAR:-word}`-shaped construct. Nested `${...}` is handled simply
/// by recursing into [`process_dollar`], which itself consumes its
/// own matching `}` before returning, so this loop never mistakes an
/// inner closing brace for its own.
fn read_until_closing_brace(
    chars: &mut Peekable<Chars>,
    env: &HashMap<String, String>,
) -> Result<String, String> {
    let mut out = String::new();
    loop {
        match chars.next() {
            None => return Err("bad substitution: unterminated \"${...}\"".to_string()),
            Some('}') => break,
            Some('\\') if chars.peek() == Some(&'$') => {
                chars.next();
                out.push('$');
            }
            Some('$') => out.push_str(&process_dollar(chars, env)?),
            Some(c) => out.push(c),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn expand_ok(word: &str, env: &HashMap<String, String>) -> String {
        expand(word, env).unwrap()
    }

    // Every case below is a real row from BuildKit's own
    // `frontend/dockerfile/shell/envVarTest` fixture (`PWD=/home` is
    // the fixture's own test environment), not invented.

    #[test]
    fn bare_variable() {
        // Variable-name matching is greedy, exactly like real shells:
        // `$PWDx` looks up a variable literally named `PWDx` (which
        // doesn't exist here), *not* `PWD` followed by a literal `x`
        // — `${PWD}x` (braced) is how real Dockerfiles disambiguate
        // this, confirmed directly rather than assumed.
        let e = env(&[("PWD", "/home")]);
        assert_eq!(expand_ok("he$PWD", &e), "he/home");
        assert_eq!(expand_ok("he${PWD}x", &e), "he/homex");
    }

    #[test]
    fn undeclared_variable_expands_to_empty_not_an_error() {
        let e = env(&[]);
        assert_eq!(expand_ok("he${hi}xx", &e), "hexx");
        assert_eq!(expand_ok("he$hi", &e), "he");
    }

    #[test]
    fn leading_digit_name_only_consumes_digits() {
        let e = env(&[("1", "1value"), ("1x", "wrong")]);
        assert_eq!(expand_ok("he$1x", &e), "he1valuex");
    }

    #[test]
    fn dollar_not_followed_by_a_valid_name_is_literal() {
        let e = env(&[]);
        assert_eq!(expand_ok("he$.x", &e), "he$.x");
    }

    #[test]
    fn braced_basic() {
        let e = env(&[("XXX", "hi")]);
        assert_eq!(expand_ok("he${XXX}xx", &e), "hehixx");
    }

    #[test]
    fn default_value_when_unset() {
        let e = env(&[]);
        assert_eq!(expand_ok("he${XXX:-000}xx", &e), "he000xx");
    }

    #[test]
    fn default_value_not_used_when_set() {
        let e = env(&[("PWD", "/home")]);
        assert_eq!(expand_ok("he${PWD:-000}xx", &e), "he/homexx");
    }

    #[test]
    fn nested_default_value() {
        let e = env(&[("PWD", "/home")]);
        assert_eq!(expand_ok("he${XXX:-$PWD}xx", &e), "he/homexx");
        assert_eq!(expand_ok("he${XXX:-${PWD:-yyy}}xx", &e), "he/homexx");
        assert_eq!(expand_ok("he${XXX:-${YYY:-yyy}}xx", &e), "heyyyxx");
    }

    #[test]
    fn colon_dash_treats_set_but_empty_as_unset() {
        let e = env(&[("NULL", "")]);
        assert_eq!(expand_ok("he${NULL:-def}xx", &e), "hedefxx");
    }

    #[test]
    fn bare_dash_keeps_a_set_but_empty_value() {
        let e = env(&[("NULL", "")]);
        assert_eq!(expand_ok("he${NULL-def}xx", &e), "hexx");
    }

    #[test]
    fn plus_alternate_value() {
        let unset = env(&[]);
        assert_eq!(expand_ok("he${XXX:+000}xx", &unset), "hexx");
        let set = env(&[("PWD", "/home")]);
        assert_eq!(expand_ok("he${PWD:+000}xx", &set), "he000xx");
    }

    #[test]
    fn colon_plus_treats_set_but_empty_as_unset() {
        let e = env(&[("NULL", "")]);
        assert_eq!(expand_ok("he${NULL:+alt}xx", &e), "hexx");
    }

    #[test]
    fn bare_plus_only_cares_whether_its_set_at_all() {
        let e = env(&[("NULL", "")]);
        assert_eq!(expand_ok("he${NULL+alt}xx", &e), "healtxx");
    }

    #[test]
    fn question_mark_errors_when_unset() {
        let e = env(&[]);
        assert!(expand("he${XXX?}", &e).is_err());
    }

    #[test]
    fn question_mark_ok_when_set_even_if_empty() {
        let e = env(&[("NULL", "")]);
        assert_eq!(expand_ok("he${NULL?}", &e), "he");
    }

    #[test]
    fn colon_question_mark_errors_when_set_but_empty_too() {
        let e = env(&[("NULL", "")]);
        assert!(expand("he${NULL:?}", &e).is_err());
    }

    #[test]
    fn question_mark_ok_when_set_and_non_empty() {
        let e = env(&[("PWD", "/home")]);
        assert_eq!(expand_ok("he${PWD?}", &e), "he/home");
    }

    #[test]
    fn backslash_escapes_a_literal_dollar() {
        let e = env(&[("PWD", "/home")]);
        assert_eq!(expand_ok(r"he\$PWD", &e), "he$PWD");
        assert_eq!(expand_ok(r"\${}", &e), "${}");
    }

    #[test]
    fn double_dollar_is_not_a_literal_dollar_escape() {
        // A real, surprising, directly-confirmed BuildKit behavior:
        // `$$` is the "$" special parameter, essentially never set,
        // so it expands to an empty string -- *not* a literal `$`.
        let e = env(&[]);
        assert_eq!(expand_ok("$$", &e), "");
    }

    #[test]
    fn bad_substitution_errors() {
        let e = env(&[]);
        assert!(expand("he${}xx", &e).is_err());
        assert!(expand("he${:xx}", &e).is_err());
        assert!(expand("he${XXX:YYY}", &e).is_err());
        assert!(expand("he${XXX", &e).is_err());
    }
}
