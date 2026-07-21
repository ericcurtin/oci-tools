//! Kernel command-line parsing and editing.
//!
//! [`Cmdline`] wraps a single kernel command-line string (`/proc/
//! cmdline`'s own syntax: whitespace-separated tokens, each either a
//! bare switch like `quiet` or a `key=value` pair, a value allowed to
//! be double-quote-wrapped so it can contain literal whitespace) and
//! provides the add/remove primitives a future `ociboot kargs`
//! subcommand needs, and what a real image's own declared kernel
//! arguments would eventually get diffed against (see `oci-bls`'s own
//! module doc comment: "kernel argument (kargs) editing shared by
//! `ociboot kargs` and install").
//!
//! A direct, deliberately narrower port of real bootc's own
//! `bootc-kernel-cmdline` crate (`~/git/bootc/crates/kernel_cmdline/
//! src/{bytes,utf8}.rs`, read directly before writing a single line of
//! this) — every operation's exact semantics (the tokenizer's own
//! quote-toggling whitespace split; [`Parameter::parse`]'s two-step
//! quote-stripping — a *whole-token* leading/trailing quote is
//! stripped first, *then* a value's own leading quote, if any, is
//! stripped a second time, matching several genuinely non-obvious
//! real edge cases the real crate's own test suite calls "pathological";
//! [`Action`]; [`Cmdline::add`]/[`add_or_modify`](Cmdline::add_or_modify)/
//! [`remove`](Cmdline::remove)/[`remove_exact`](Cmdline::remove_exact);
//! and the real "dashes and underscores are equivalent for key
//! comparison" rule) cross-checked directly against the real crate's
//! own test suite — several of this module's own tests below are a
//! direct, attributed transcription of real bootc's own test cases,
//! including the "pathological" ones, not just the straightforward
//! ones.
//!
//! **Deliberately narrower than real bootc** in two specific ways:
//! - UTF-8 only — real bootc also has a raw-byte `bytes` module
//!   tolerating a non-UTF-8 `/proc/cmdline`, a real but rare edge case
//!   this project hasn't needed yet (no current caller reads a live
//!   `/proc/cmdline` at all).
//! - Always-owned, no borrowed/`Cow` complexity — kargs editing is
//!   never a hot path the way container startup is, so the extra
//!   allocation an owned `String` per [`Parameter`] costs is not worth
//!   real bootc's own zero-copy design here.

use std::fmt;

/// Possible outcomes for [`Cmdline::add`]/[`Cmdline::add_or_modify`] —
/// a direct port of real bootc's own `Action` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    /// The parameter did not exist before and was added.
    Added,
    /// The parameter existed before, but contained a different value.
    /// The value was updated to the newly-requested value.
    Modified,
    /// The parameter existed before, and contained the same value as
    /// the newly-requested value. No modification was made.
    Existed,
}

/// One parsed kernel command-line parameter: a bare switch (`value`
/// is `None`) or a `key=value` pair. Two parameters are equal if
/// their own keys match (dashes and underscores treated as
/// equivalent — real bootc's own documented rule) and their values
/// are exactly equal.
#[derive(Debug, Clone)]
pub struct Parameter {
    /// The whole original token, verbatim (quote characters included
    /// exactly as given) — what actually gets appended back onto a
    /// [`Cmdline`] by [`Cmdline::add`]/[`Cmdline::add_or_modify`],
    /// never a value re-quoted/reformatted from the parsed `key`/
    /// `value` fields (matching real bootc's own identical choice:
    /// its own `Parameter.parameter` field is the literal original
    /// slice, re-used verbatim by `add`/`add_or_modify`).
    token: String,
    key: String,
    value: Option<String>,
}

impl Parameter {
    /// Parse a single parameter from `input`. If `input` contains more
    /// than one whitespace-separated parameter, only the first one is
    /// parsed and the rest is discarded — matching real bootc's own
    /// `Parameter::parse` exactly. Returns `None` for empty or
    /// whitespace-only input.
    pub fn parse(input: &str) -> Option<Self> {
        let token = split_tokens(input).next()?;
        Some(Self::parse_token(token))
    }

    fn parse_token(token: &str) -> Self {
        // *Only* the first and last double quotes of the *whole*
        // token are stripped here.
        let dequoted = token.strip_prefix('"').unwrap_or(token);
        let dequoted = dequoted.strip_suffix('"').unwrap_or(dequoted);
        match dequoted.split_once('=') {
            None => Parameter {
                token: token.to_string(),
                key: dequoted.to_string(),
                value: None,
            },
            Some((key, value)) => {
                // If there is a quote right after the `=`, strip it
                // too — if there was a closing quote at the very end
                // of the value, it was already removed above.
                let value = value.strip_prefix('"').unwrap_or(value);
                Parameter {
                    token: token.to_string(),
                    key: key.to_string(),
                    value: Some(value.to_string()),
                }
            }
        }
    }

    /// The parameter's own key (post-dequoting).
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The parameter's own value (post-dequoting), if it has one.
    pub fn value(&self) -> Option<&str> {
        self.value.as_deref()
    }

    fn key_matches(&self, other_key: &str) -> bool {
        canonical_key(&self.key) == canonical_key(other_key)
    }
}

/// Real bootc's own documented equality rule: keys compared with
/// dashes and underscores treated as equivalent; values compared
/// exactly.
impl PartialEq for Parameter {
    fn eq(&self, other: &Self) -> bool {
        self.key_matches(&other.key) && self.value == other.value
    }
}

impl Eq for Parameter {}

/// Canonicalize a key for comparison purposes: dashes become
/// underscores (never the other direction — matching real bootc's own
/// `ParameterKey::iter`, which maps `-` to `_`, not vice versa).
fn canonical_key(key: &str) -> String {
    key.replace('-', "_")
}

/// Split `input` into whitespace-separated tokens, treating a run of
/// characters between a pair of double quotes as part of the same
/// token even if it contains whitespace — a direct port of real
/// bootc's own `CmdlineIterBytes`.
fn split_tokens(input: &str) -> impl Iterator<Item = &str> {
    let mut remaining = input;
    std::iter::from_fn(move || {
        remaining = remaining.trim_start();
        if remaining.is_empty() {
            return None;
        }
        let mut in_quotes = false;
        let end = remaining.char_indices().find_map(|(i, c)| {
            if c == '"' {
                in_quotes = !in_quotes;
            }
            (!in_quotes && c.is_whitespace()).then_some(i)
        });
        let end = end.unwrap_or(remaining.len());
        let (token, rest) = remaining.split_at(end);
        remaining = rest;
        Some(token)
    })
}

/// A parsed kernel command line — see this module's own doc comment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cmdline(String);

impl Cmdline {
    /// A new, empty command line.
    pub fn new() -> Self {
        Self::default()
    }

    /// An iterator over every parameter in this command line, in
    /// order.
    pub fn iter(&self) -> impl Iterator<Item = Parameter> + '_ {
        split_tokens(&self.0).map(Parameter::parse_token)
    }

    /// The first parameter matching `key` (dashes/underscores
    /// equivalent), if any.
    pub fn find(&self, key: &str) -> Option<Parameter> {
        self.iter().find(|p| p.key_matches(key))
    }

    /// The value of the first parameter matching `key`, if any.
    pub fn value_of(&self, key: &str) -> Option<String> {
        self.find(key).and_then(|p| p.value)
    }

    /// Add `param` to the command line if the *exact* same parameter
    /// (same key and value) doesn't already exist — never modifies an
    /// existing parameter with the same key but a different value
    /// in place, unlike [`add_or_modify`](Self::add_or_modify); a
    /// duplicate key with a different value is simply appended too
    /// (real kernel command lines do allow e.g. multiple `console=`
    /// parameters).
    pub fn add(&mut self, param: &Parameter) -> Action {
        if self.iter().any(|p| p == *param) {
            return Action::Existed;
        }
        self.push_token(&param.token);
        Action::Added
    }

    /// Add `param`, replacing the *first* existing parameter with a
    /// matching key in place (dropping any later duplicate of that
    /// same key entirely) if one exists, or appending it if not.
    pub fn add_or_modify(&mut self, param: &Parameter) -> Action {
        let mut new_tokens: Vec<String> = Vec::new();
        let mut seen_key = false;
        let mut modified = false;
        for p in self.iter() {
            if p.key_matches(&param.key) {
                if !seen_key {
                    if p != *param {
                        modified = true;
                    }
                    new_tokens.push(param.token.clone());
                } else {
                    // A later duplicate of the same key: dropping it
                    // is itself a modification.
                    modified = true;
                }
                seen_key = true;
            } else {
                new_tokens.push(p.token.clone());
            }
        }
        if !seen_key {
            new_tokens.push(param.token.clone());
            self.0 = new_tokens.join(" ");
            return Action::Added;
        }
        self.0 = new_tokens.join(" ");
        if modified {
            Action::Modified
        } else {
            Action::Existed
        }
    }

    /// Remove every parameter whose key matches `key`
    /// (dashes/underscores equivalent). Returns whether anything was
    /// removed.
    pub fn remove(&mut self, key: &str) -> bool {
        let mut kept: Vec<String> = Vec::new();
        let mut removed = false;
        for p in self.iter() {
            if p.key_matches(key) {
                removed = true;
            } else {
                kept.push(p.token);
            }
        }
        if removed {
            self.0 = kept.join(" ");
        }
        removed
    }

    /// Remove every parameter that exactly matches `param` (same key
    /// and value). Returns whether anything was removed.
    pub fn remove_exact(&mut self, param: &Parameter) -> bool {
        let mut kept: Vec<String> = Vec::new();
        let mut removed = false;
        for p in self.iter() {
            if p == *param {
                removed = true;
            } else {
                kept.push(p.token);
            }
        }
        if removed {
            self.0 = kept.join(" ");
        }
        removed
    }

    fn push_token(&mut self, token: &str) {
        if !self.0.is_empty() && !self.0.ends_with(char::is_whitespace) {
            self.0.push(' ');
        }
        self.0.push_str(token);
    }
}

impl<T: AsRef<str> + ?Sized> From<&T> for Cmdline {
    fn from(input: &T) -> Self {
        Cmdline(input.as_ref().to_string())
    }
}

impl From<String> for Cmdline {
    fn from(input: String) -> Self {
        Cmdline(input)
    }
}

impl fmt::Display for Cmdline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn param(s: &str) -> Parameter {
        Parameter::parse(s).unwrap()
    }

    #[test]
    fn parse_a_bare_switch_has_no_value() {
        let p = Parameter::parse("foo").unwrap();
        assert_eq!(p.key(), "foo");
        assert_eq!(p.value(), None);
    }

    #[test]
    fn parse_only_consumes_the_first_parameter_and_discards_the_rest() {
        let p = Parameter::parse("foo=bar baz").unwrap();
        assert_eq!(p.key(), "foo");
        assert_eq!(p.value(), Some("bar"));
    }

    #[test]
    fn parse_of_empty_or_whitespace_only_input_is_none() {
        assert!(Parameter::parse("").is_none());
        assert!(Parameter::parse("   ").is_none());
    }

    #[test]
    fn parse_trims_surrounding_whitespace() {
        let p = Parameter::parse("  foo=bar  ").unwrap();
        assert_eq!(p.key(), "foo");
        assert_eq!(p.value(), Some("bar"));
    }

    /// Every case here is a direct transcription of real bootc's own
    /// `test_parameter_quoted` (`~/git/bootc/crates/kernel_cmdline/
    /// src/bytes.rs`).
    #[test]
    fn quoting_matches_real_bootcs_own_test_cases() {
        assert_eq!(param(r#"foo="quoted value""#).value(), Some("quoted value"));
        assert_eq!(
            param(r#"foo="unclosed quotes"#).value(),
            Some("unclosed quotes")
        );
        assert_eq!(
            param(r#"foo=trailing_quotes""#).value(),
            Some("trailing_quotes")
        );
        assert_eq!(
            param(r#""foo=quoted value""#),
            param(r#"foo="quoted value""#)
        );
    }

    /// Direct transcriptions of real bootc's own `test_parameter_
    /// pathological` — genuinely non-obvious edge cases of the real,
    /// two-step quote-stripping algorithm, verified here byte-for-
    /// byte the same as the real crate's own test suite.
    #[test]
    fn pathological_quoting_matches_real_bootcs_own_test_cases() {
        // You can quote just the key part of a key-value param, but
        // the end quote is actually part of the key as far as the
        // kernel is concerned...
        let p = param(r#""foo"=bar"#);
        assert_eq!(p.key(), "foo\"");
        assert_eq!(p.value(), Some("bar"));
        // ...and it is definitely not equal to an unquoted foo...
        assert_ne!(p, param("foo=bar"));

        // ...but if you close the quote immediately after the equals
        // sign, it does get removed...
        let p = param(r#""foo="bar"#);
        assert_eq!(p.key(), "foo");
        assert_eq!(p.value(), Some("bar"));
        // ...so of course this makes sense...
        assert_eq!(p, param("foo=bar"));

        // Quotes only get stripped from the absolute ends of values --
        // once, not repeatedly.
        let p = param(r#"foo="internal"quotes"are"ok""#);
        assert_eq!(p.value(), Some(r#"internal"quotes"are"ok"#));
    }

    #[test]
    fn key_comparison_treats_dashes_and_underscores_as_equivalent() {
        // Direct transcription of real bootc's own
        // `test_parameter_equality`.
        assert_eq!(param("a-delimited-param"), param("a_delimited_param"));
        assert_eq!(
            param("a-delimited-param=same_values"),
            param("a_delimited_param=same_values")
        );
        assert_ne!(
            param("a-delimited-param=different_values"),
            param("a_delimited_param=DiFfErEnT_valUEZ")
        );
        // A bare switch and a `key=value` sharing the same key text
        // are still never equal to each other.
        assert_ne!(param("same_key"), param("same_key=but_with_a_value"));
        // Substrings are not equal.
        assert_ne!(param("foo"), param("foobar"));
    }

    #[test]
    fn cmdline_iter_yields_every_parameter_in_order() {
        let cmdline = Cmdline::from("quiet console=ttyS0 root=/dev/sda1");
        let keys: Vec<String> = cmdline.iter().map(|p| p.key).collect();
        assert_eq!(keys, vec!["quiet", "console", "root"]);
    }

    #[test]
    fn cmdline_find_and_value_of_use_dash_underscore_equivalence() {
        let cmdline = Cmdline::from("dash-key=value1 under_key=value2");
        assert_eq!(cmdline.value_of("dash_key"), Some("value1".to_string()));
        assert_eq!(cmdline.value_of("dash-key"), Some("value1".to_string()));
        assert_eq!(cmdline.value_of("under-key"), Some("value2".to_string()));
        assert_eq!(cmdline.value_of("missing"), None);
    }

    /// Direct transcription of real bootc's own `test_add`.
    #[test]
    fn add_appends_a_new_parameter_and_allows_a_duplicate_key_with_a_different_value() {
        let mut kargs = Cmdline::from("console=tty0 console=ttyS1");

        assert_eq!(kargs.add(&param("console=ttyS2")), Action::Added);
        let keys_and_values: Vec<(String, Option<String>)> = kargs
            .iter()
            .map(|p| (p.key().to_string(), p.value().map(str::to_string)))
            .collect();
        assert_eq!(
            keys_and_values,
            vec![
                ("console".to_string(), Some("tty0".to_string())),
                ("console".to_string(), Some("ttyS1".to_string())),
                ("console".to_string(), Some("ttyS2".to_string())),
            ]
        );

        // An exact duplicate is a no-op.
        assert_eq!(kargs.add(&param("console=ttyS1")), Action::Existed);
        assert_eq!(kargs.iter().count(), 3);

        assert_eq!(kargs.add(&param("quiet")), Action::Added);
        assert_eq!(kargs.iter().count(), 4);
    }

    #[test]
    fn add_to_an_empty_cmdline_has_no_leading_space() {
        let mut kargs = Cmdline::new();
        assert_eq!(kargs.add(&param("foo")), Action::Added);
        assert_eq!(kargs.to_string(), "foo");
    }

    /// Direct transcription of real bootc's own `test_add_or_modify`.
    #[test]
    fn add_or_modify_replaces_an_existing_keys_own_value_in_place() {
        let mut kargs = Cmdline::from("foo=bar");

        assert_eq!(kargs.add_or_modify(&param("baz")), Action::Added);
        assert_eq!(kargs.to_string(), "foo=bar baz");

        assert_eq!(kargs.add_or_modify(&param("foo=fuz")), Action::Modified);
        assert_eq!(kargs.to_string(), "foo=fuz baz");

        // Same value again: no real change.
        assert_eq!(kargs.add_or_modify(&param("foo=fuz")), Action::Existed);
        assert_eq!(kargs.to_string(), "foo=fuz baz");
    }

    /// Direct transcription of real bootc's own
    /// `test_add_or_modify_duplicate_parameters`.
    #[test]
    fn add_or_modify_collapses_every_duplicate_key_into_the_new_single_value() {
        let mut kargs = Cmdline::from("a=1 a=2");
        assert_eq!(kargs.add_or_modify(&param("a=3")), Action::Modified);
        assert_eq!(kargs.to_string(), "a=3");
    }

    #[test]
    fn remove_drops_every_parameter_with_a_matching_key() {
        // Direct transcription of real bootc's own `test_remove`/
        // `test_remove_duplicates`.
        let mut kargs = Cmdline::from("foo bar baz");
        assert!(kargs.remove("bar"));
        assert_eq!(kargs.to_string(), "foo baz");
        assert!(!kargs.remove("missing"));
        assert_eq!(kargs.to_string(), "foo baz");

        let mut kargs = Cmdline::from("a=1 b=2 a=3");
        assert!(kargs.remove("a"));
        assert_eq!(kargs.to_string(), "b=2");
    }

    /// Direct transcription of real bootc's own `test_remove_exact`.
    #[test]
    fn remove_exact_only_drops_parameters_matching_key_and_value() {
        let mut kargs = Cmdline::from("foo foo=bar foo=baz");
        assert!(kargs.remove_exact(&param("foo=bar")));
        assert_eq!(kargs.to_string(), "foo foo=baz");
        assert!(!kargs.remove_exact(&param("foo=wuz")));
        assert_eq!(kargs.to_string(), "foo foo=baz");
    }

    #[test]
    fn cmdline_display_round_trips_a_simple_string() {
        let cmdline = Cmdline::from("quiet root=/dev/sda1");
        assert_eq!(cmdline.to_string(), "quiet root=/dev/sda1");
    }
}
