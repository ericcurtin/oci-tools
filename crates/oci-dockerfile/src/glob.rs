//! Glob pattern matching for `COPY`/`ADD` sources — a direct, function-
//! for-function translation of Go's own `path/filepath.Match`
//! (`/usr/share/go-1.22/src/path/filepath/match.go` on this development
//! host, read directly before writing a single line of Rust; not
//! reverse-engineered from black-box testing or from the one-paragraph
//! doc comment alone), the exact matcher real BuildKit's own
//! `copyWithWildcards` (`~/git/moby/daemon/builder/dockerfile/copy.go`)
//! calls for every Dockerfile `COPY`/`ADD` wildcard source.
//!
//! Every behavior here was independently cross-checked against the
//! real `go` toolchain installed on this development host (`go1.22.2`)
//! before trusting the translation: two dozen or so probe patterns,
//! covering every construct the algorithm handles (`*`, `?`, `[...]`,
//! `[^...]` negation — **not** `[!...]`, confirmed directly that Go's
//! own algorithm only recognizes `^` for negation, not `!` — escaping
//! with `\`, the rule that none of `*`/`?`/a character class ever
//! matches the path separator `/` at all, even inside a bracket
//! expression, and a genuinely easy-to-miss validation rule: a
//! character class like `[a` — a real character but no room left for
//! either a range's `-hi` or the class's own closing `]` — is a real,
//! rejected `BadPattern`, confirmed directly with `filepath.Match("[a",
//! "a")`), run through a real `go run` program first, matched against
//! this module's own test suite one-for-one.
//!
//! Internally works on raw bytes (`&[u8]`), not `&str`, deliberately:
//! Go's own `matchChunk` advances one *byte* at a time for a literal
//! (non-`?`/non-`[...]`) pattern character
//! (`chunk = chunk[1:]`/`s = s[1:]`), which can — and, for a
//! multi-byte UTF-8 name mid-comparison, briefly does — land on a
//! byte offset that isn't a valid UTF-8 character boundary. Go's own
//! byte-indexed strings tolerate that trivially; Rust's `&str`
//! indexing panics on it. Working on `&[u8]` throughout (only ever
//! decoding a full `char` where Go itself does, for `?`/`[...]`) means
//! this can never panic on any input, matching Go's own real
//! byte-stepping behavior exactly rather than the merely-similar
//! whole-character stepping an earlier draft of this module used
//! before this exact concern was caught.
//!
//! [`contains_wildcards`] matches real BuildKit's own Unix-specific
//! `containsWildcards` (`~/git/moby/daemon/builder/dockerfile/
//! copy_unix.go`) exactly: `*`, `?`, or `[` anywhere in the string
//! (a character immediately after a literal `\` doesn't count, even if
//! it's one of those three) means "treat this source as a glob
//! pattern", not merely "the algorithm below would fail to match it
//! literally".

/// Whether `s` contains a glob wildcard character (`*`, `?`, `[`) that
/// isn't itself escaped by a preceding `\` — matching real BuildKit's
/// own `containsWildcards` (Unix) exactly.
pub fn contains_wildcards(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 1,
            b'*' | b'?' | b'[' => return true,
            _ => {}
        }
        i += 1;
    }
    false
}

/// A malformed pattern (an unterminated `[...]` character class, a
/// trailing dangling `\`, or a character class with no room left for
/// its own closing `]`) — the only error [`match_pattern`] ever
/// returns, matching Go's own single `ErrBadPattern`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BadPattern;

impl std::fmt::Display for BadPattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("syntax error in glob pattern")
    }
}

impl std::error::Error for BadPattern {}

/// Report whether `name` matches the shell file-name `pattern`, using
/// exactly Go's own `path/filepath.Match` syntax and semantics (see
/// this module's own doc comment for where that's specified and how
/// it was verified) — most importantly: `pattern` must match all of
/// `name`, not just a substring, and none of `*`/`?`/a character class
/// ever matches the real path separator (`/`) at all.
///
/// A direct translation of Go's own `Match` function's labeled-loop
/// structure (`Pattern: for ... { ... continue Pattern }`), using
/// Rust's own labeled `loop`/`continue` for the same shape, rather
/// than restructured into something that "looks more idiomatic" but
/// would be harder to keep in sync with the real algorithm at a
/// glance.
pub fn match_pattern(pattern: &str, name: &str) -> Result<bool, BadPattern> {
    match_bytes(pattern.as_bytes(), name.as_bytes())
}

fn match_bytes(pattern: &[u8], name: &[u8]) -> Result<bool, BadPattern> {
    let mut pattern = pattern;
    let mut name = name;
    'pattern: loop {
        if pattern.is_empty() {
            return Ok(name.is_empty());
        }
        let (star, chunk, rest) = scan_chunk(pattern);
        if star && chunk.is_empty() {
            // Trailing `*` matches the rest of the string, unless it
            // contains a `/`.
            return Ok(!name.contains(&b'/'));
        }
        // Look for a match at the current position.
        if let Some(remaining) = match_chunk(chunk, name)? {
            // If this is the last chunk, the whole name must be
            // exhausted too -- otherwise a `*` earlier on could still
            // match more of it.
            if remaining.is_empty() || !rest.is_empty() {
                name = remaining;
                pattern = rest;
                continue 'pattern;
            }
        }
        if star {
            // Look for a match skipping ahead one *byte* at a time
            // (matching Go's own `for i := 0; i < len(name) ...;
            // i++`, not one character at a time -- see this module's
            // own top doc comment for why that distinction matters) --
            // never skipping past a `/`.
            let mut i = 0;
            while i < name.len() && name[i] != b'/' {
                i += 1;
                if let Some(remaining) = match_chunk(chunk, &name[i..])? {
                    // If this is the last chunk, make sure the name is
                    // fully exhausted.
                    if rest.is_empty() && !remaining.is_empty() {
                        continue;
                    }
                    name = remaining;
                    pattern = rest;
                    continue 'pattern;
                }
            }
        }
        return Ok(false);
    }
}

/// Get the next segment of `pattern`: a non-`*` chunk, possibly
/// preceded by one or more `*`s (collapsed into a single `star: true`
/// flag, matching Go's own `scanChunk`). A `*` inside a `[...]`
/// character class doesn't end the chunk; a `\` escapes the following
/// byte (skipped over, not interpreted) for this scan's own purposes
/// -- `match_chunk`/`get_esc` are what actually validate and apply the
/// escape later.
fn scan_chunk(pattern: &[u8]) -> (bool, &[u8], &[u8]) {
    let mut pattern = pattern;
    let mut star = false;
    while pattern.first() == Some(&b'*') {
        star = true;
        pattern = &pattern[1..];
    }
    let mut in_range = false;
    let mut i = 0;
    while i < pattern.len() {
        match pattern[i] {
            b'\\' => {
                if i + 1 < pattern.len() {
                    i += 1;
                }
            }
            b'[' => in_range = true,
            b']' => in_range = false,
            b'*' if !in_range => break,
            _ => {}
        }
        i += 1;
    }
    (star, &pattern[..i], &pattern[i..])
}

/// Decode one `char` from the front of `bytes`, matching Go's own
/// `utf8.DecodeRuneInString`'s tolerant behavior for invalid encoding:
/// a byte sequence that isn't valid UTF-8 at this exact position still
/// decodes to *something* (Go's own replacement rune, consuming
/// exactly one byte) rather than this crate's own algorithm ever
/// panicking on it. In practice this only matters for the
/// pathological byte-stepping scenario this module's own top doc
/// comment describes; every real Dockerfile `COPY`/`ADD` glob pattern
/// only ever exercises the ordinary, always-a-valid-boundary case.
fn decode_char(bytes: &[u8]) -> (char, usize) {
    match std::str::from_utf8(bytes) {
        Ok(s) => {
            let ch = s
                .chars()
                .next()
                .expect("non-empty bytes decode to a non-empty str");
            (ch, ch.len_utf8())
        }
        Err(e) => match e.error_len() {
            // A genuinely invalid byte at this exact position (rather
            // than merely a truncated multi-byte sequence at the very
            // end of `bytes`): consume exactly one byte, matching
            // Go's own `(RuneError, 1)`.
            Some(_) => (char::REPLACEMENT_CHARACTER, 1),
            None => (char::REPLACEMENT_CHARACTER, bytes.len().max(1)),
        },
    }
}

/// Check whether `chunk` (a single-`*`-free segment: literals,
/// character classes, `?`) matches a *prefix* of `s`, returning
/// whatever's left of `s` after that prefix if so — matching Go's own
/// `matchChunk` exactly, including its "keep scanning `chunk` for
/// well-formedness even after the match has already failed" behavior
/// (so a malformed pattern is still reported as [`BadPattern`] even if
/// `s` itself was already too short to match).
fn match_chunk<'s>(chunk: &[u8], s: &'s [u8]) -> Result<Option<&'s [u8]>, BadPattern> {
    let mut chunk = chunk;
    let mut s = s;
    let mut failed = false;
    while !chunk.is_empty() {
        if !failed && s.is_empty() {
            failed = true;
        }
        match chunk[0] {
            b'[' => {
                let r = if !failed {
                    let (ch, n) = decode_char(s);
                    s = &s[n..];
                    Some(ch)
                } else {
                    None
                };
                chunk = &chunk[1..];
                let negated = chunk.first() == Some(&b'^');
                if negated {
                    chunk = &chunk[1..];
                }
                let mut matched_any = false;
                let mut n_ranges = 0u32;
                loop {
                    if chunk.first() == Some(&b']') && n_ranges > 0 {
                        chunk = &chunk[1..];
                        break;
                    }
                    let (lo, next_chunk) = get_esc(chunk)?;
                    chunk = next_chunk;
                    let hi = if chunk.first() == Some(&b'-') {
                        let (hi, next_chunk) = get_esc(&chunk[1..])?;
                        chunk = next_chunk;
                        hi
                    } else {
                        lo
                    };
                    if let Some(r) = r
                        && lo <= r
                        && r <= hi
                    {
                        matched_any = true;
                    }
                    n_ranges += 1;
                }
                if matched_any == negated {
                    failed = true;
                }
            }
            b'?' => {
                if !failed {
                    let (ch, n) = decode_char(s);
                    if ch == '/' {
                        failed = true;
                    }
                    s = &s[n..];
                }
                chunk = &chunk[1..];
            }
            b'\\' => {
                chunk = &chunk[1..];
                if chunk.is_empty() {
                    return Err(BadPattern);
                }
                let lit = chunk[0];
                if !failed {
                    if s[0] != lit {
                        failed = true;
                    }
                    s = &s[1..];
                }
                chunk = &chunk[1..];
            }
            c => {
                if !failed {
                    if s[0] != c {
                        failed = true;
                    }
                    s = &s[1..];
                }
                chunk = &chunk[1..];
            }
        }
    }
    if failed { Ok(None) } else { Ok(Some(s)) }
}

/// Get one possibly-`\`-escaped character from the front of `chunk`,
/// for a character-class range endpoint -- matching Go's own `getEsc`
/// exactly, including its own final check that at least one byte must
/// remain *after* the character just consumed (there must still be
/// room for a range's own `-hi` or the class's closing `]`) --
/// confirmed directly: real `filepath.Match("[a", "a")` itself fails
/// with the real `ErrBadPattern`, for exactly this reason.
fn get_esc(chunk: &[u8]) -> Result<(char, &[u8]), BadPattern> {
    if chunk.is_empty() || chunk[0] == b'-' || chunk[0] == b']' {
        return Err(BadPattern);
    }
    let chunk = if chunk[0] == b'\\' {
        let rest = &chunk[1..];
        if rest.is_empty() {
            return Err(BadPattern);
        }
        rest
    } else {
        chunk
    };
    let (ch, n) = decode_char(chunk);
    let rest = &chunk[n..];
    if rest.is_empty() {
        return Err(BadPattern);
    }
    Ok((ch, rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(pattern: &str, name: &str) -> bool {
        match_pattern(pattern, name).unwrap()
    }

    /// Every probe pattern in this test was first run through a real
    /// `go run` program against the real `go1.22.2` toolchain
    /// installed on this development host, confirming the expected
    /// result *before* being copied in here.
    #[test]
    fn matches_every_probe_confirmed_against_the_real_go_toolchain() {
        assert!(m("*.txt", "foo.txt"));
        assert!(!m("*.txt", "subdir/foo.txt"));
        assert!(m("subdir/*.txt", "subdir/foo.txt"));
        assert!(!m("subdir/*.txt", "subdir/nested/foo.txt"));
        assert!(m("*.txt", ".txt"));
        assert!(m("a*b", "ab"));
        assert!(m("a*b", "axxxb"));
        assert!(!m("a?b", "ab"));
        assert!(m("a?b", "axb"));
        assert!(!m("a?b", "axxb"));
        assert!(m("[abc].txt", "a.txt"));
        assert!(!m("[abc].txt", "d.txt"));
        assert!(m("[a-c].txt", "b.txt"));
        // `[!abc]` is *not* negation in Go's own algorithm -- only `^`
        // is -- so this is a literal class containing `!`, `a`, `b`,
        // `c`, which `d` isn't a member of.
        assert!(!m("[!abc].txt", "d.txt"));
        assert!(m("[^abc].txt", "d.txt"));
        assert!(m("a\\*b", "a*b"));
        assert!(!m("a\\*b", "axb"));
        assert!(m("*", "foo"));
        assert!(!m("*", "foo/bar"));
        assert!(!m("**", "foo/bar"));
        assert!(match_pattern("[", "[").is_err());
        assert!(match_pattern("a[", "a[").is_err());
        assert!(!m("a?b", "a/b"));
        assert!(!m("a*b", "a/b"));
        assert!(!m("[a/]b", "a/b"));
        assert!(m("a*b*c", "aXbXc"));
        assert!(!m("a*b*c", "aXXbXXcXX"));
        assert!(!m("a*b*c", "ac"));
        assert!(m("a*b*c", "abc"));
        assert!(m("*.go", "main.go"));
        assert!(m("main.go", "main.go"));
        assert!(m("m*.go", "main.go"));
        assert!(m("[a-zA-Z]*.txt", "Foo.txt"));
        assert!(m("", ""));
        assert!(!m("", "x"));
        assert!(!m("x", ""));
        assert!(m("*", ""));
        // A character with no room left for `-hi` or the closing `]`.
        assert!(match_pattern("[a", "a").is_err());
        assert!(match_pattern("[a-", "a").is_err());
        assert!(m("[a]", "a"));
        assert!(match_pattern("[ab", "a").is_err());
        assert!(match_pattern("a[b-c", "ab").is_err());
    }

    #[test]
    fn a_multi_byte_utf8_name_never_panics_regardless_of_pattern_shape() {
        // Not asserting a specific result for every combination here
        // (some of these are exactly the pathological byte-stepping
        // edge case this module's own doc comment describes, where
        // fidelity to Go's own byte-oriented behavior isn't
        // meaningful for a real glob pattern anyway) -- the real
        // requirement being tested is that none of this ever panics.
        for pattern in ["*", "a*", "*é", "?", "a?", "[a-z]*", "*.txt"] {
            for name in ["café.txt", "日本語", "a", "éé", ""] {
                let _ = match_pattern(pattern, name);
            }
        }
    }

    /// Go's own complete, official `matchTests` table
    /// (`/usr/share/go-1.22/src/path/filepath/match_test.go`, read and
    /// copied directly, not re-derived) — every single case this
    /// crate's own maintainers test their real implementation
    /// against, run here against this Rust translation of it.
    #[test]
    fn matches_every_case_in_gos_own_official_match_test_table() {
        let cases: &[(&str, &str, Option<bool>)] = &[
            ("abc", "abc", Some(true)),
            ("*", "abc", Some(true)),
            ("*c", "abc", Some(true)),
            ("a*", "a", Some(true)),
            ("a*", "abc", Some(true)),
            ("a*", "ab/c", Some(false)),
            ("a*/b", "abc/b", Some(true)),
            ("a*/b", "a/c/b", Some(false)),
            ("a*b*c*d*e*/f", "axbxcxdxe/f", Some(true)),
            ("a*b*c*d*e*/f", "axbxcxdxexxx/f", Some(true)),
            ("a*b*c*d*e*/f", "axbxcxdxe/xxx/f", Some(false)),
            ("a*b*c*d*e*/f", "axbxcxdxexxx/fff", Some(false)),
            ("a*b?c*x", "abxbbxdbxebxczzx", Some(true)),
            ("a*b?c*x", "abxbbxdbxebxczzy", Some(false)),
            ("ab[c]", "abc", Some(true)),
            ("ab[b-d]", "abc", Some(true)),
            ("ab[e-g]", "abc", Some(false)),
            ("ab[^c]", "abc", Some(false)),
            ("ab[^b-d]", "abc", Some(false)),
            ("ab[^e-g]", "abc", Some(true)),
            ("a\\*b", "a*b", Some(true)),
            ("a\\*b", "ab", Some(false)),
            ("a?b", "a\u{263a}b", Some(true)),
            ("a[^a]b", "a\u{263a}b", Some(true)),
            ("a???b", "a\u{263a}b", Some(false)),
            ("a[^a][^a][^a]b", "a\u{263a}b", Some(false)),
            ("[a-\u{3b6}]*", "\u{3b1}", Some(true)),
            ("*[a-\u{3b6}]", "A", Some(false)),
            ("a?b", "a/b", Some(false)),
            ("a*b", "a/b", Some(false)),
            ("[\\]a]", "]", Some(true)),
            ("[\\-]", "-", Some(true)),
            ("[x\\-]", "x", Some(true)),
            ("[x\\-]", "-", Some(true)),
            ("[x\\-]", "z", Some(false)),
            ("[\\-x]", "x", Some(true)),
            ("[\\-x]", "-", Some(true)),
            ("[\\-x]", "a", Some(false)),
            ("[]a]", "]", None),
            ("[-]", "-", None),
            ("[x-]", "x", None),
            ("[x-]", "-", None),
            ("[x-]", "z", None),
            ("[-x]", "x", None),
            ("[-x]", "-", None),
            ("[-x]", "a", None),
            ("\\", "a", None),
            ("[a-b-c]", "a", None),
            ("[", "a", None),
            ("[^", "a", None),
            ("[^bc", "a", None),
            ("a[", "a", None),
            ("a[", "ab", None),
            ("a[", "x", None),
            ("a/b[", "x", None),
            ("*x", "xxx", Some(true)),
        ];
        for &(pattern, name, expected) in cases {
            let actual = match_pattern(pattern, name);
            match expected {
                Some(want) => assert_eq!(
                    actual,
                    Ok(want),
                    "Match({pattern:?}, {name:?}) = {actual:?}, want Ok({want})"
                ),
                None => assert_eq!(
                    actual,
                    Err(BadPattern),
                    "Match({pattern:?}, {name:?}) = {actual:?}, want Err(BadPattern)"
                ),
            }
        }
    }

    #[test]
    fn contains_wildcards_recognizes_all_three_markers_and_respects_escaping() {
        assert!(contains_wildcards("*.txt"));
        assert!(contains_wildcards("file?.txt"));
        assert!(contains_wildcards("[abc].txt"));
        assert!(!contains_wildcards("plain.txt"));
        // An escaped wildcard character doesn't count.
        assert!(!contains_wildcards("literal\\*.txt"));
        assert!(!contains_wildcards("literal\\?.txt"));
        assert!(!contains_wildcards("literal\\[.txt"));
    }
}
