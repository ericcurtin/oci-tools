//! `.dockerignore` parsing and matching for `ociman build`'s own build
//! context — a direct translation of real BuildKit/classic-builder's
//! own two-part implementation, read directly (not re-derived from
//! documentation prose, which is measurably less precise on several
//! points below): `~/git/moby/vendor/github.com/moby/patternmatcher/
//! ignorefile/ignorefile.go` (`ReadAll`, ported as [`parse`]) for the
//! file's own line syntax, and `~/git/moby/vendor/github.com/moby/
//! patternmatcher/patternmatcher.go` (`New`/`compile`/
//! `MatchesOrParentMatches`) for the actual pattern-matching semantics
//! this module's own [`DockerIgnore`] implements.
//!
//! Every non-obvious rule below was independently confirmed against a
//! real, installed `podman build` (4.9.3) first, not assumed from
//! reading the Go source alone:
//! * A bare pattern like `*.log` only ever matches a *top-level*
//!   context entry — `subdir/nested.log` needs `**/*.log` to match at
//!   any depth, confirmed directly (`podman build` left a nested
//!   `.log` file in place with a bare `*.log` pattern, removed it with
//!   `**/*.log`) — exactly what compiling a bare `*` to `[^/]*` (never
//!   crossing `/`) predicts.
//! * A later `!pattern` re-inclusion for one specific file works even
//!   when an earlier pattern excluded that file's own parent
//!   directory (confirmed: `subdir` excluded, `!subdir/keep.txt` still
//!   kept `subdir/keep.txt`, while `subdir` itself grew no *empty*
//!   directory entry when nothing under it survived) — unlike real
//!   `.gitignore`'s own early-pruning behavior, `.dockerignore`
//!   evaluates every pattern against every path independently, since
//!   [`DockerIgnore::is_ignored`] never prunes traversal based on an
//!   ancestor's own already-decided state.
//! * Neither the Containerfile itself nor `.dockerignore` gets any
//!   special always-included treatment — a bare `*` (excluding
//!   everything, `!keep.txt` re-including just one file) really did
//!   exclude both from the built image, confirmed directly.
//! * An explicitly-named (non-wildcard) `COPY`/`ADD` source that's
//!   itself excluded fails the exact same way a genuinely missing
//!   source would (confirmed directly: `podman build`'s own error is
//!   literally "no such file or directory", after first reporting how
//!   many candidates its own glob step filtered out) — [`ociman
//!   build`'s own `resolve_sources`/`ensure_sources_exist`] (`bin/
//!   ociman/src/build.rs`) deliberately reuses that exact same "source
//!   does not exist" error path for this, rather than inventing a
//!   separate "excluded by .dockerignore" message.
//! * A wildcard `COPY`/`ADD` source silently drops any excluded match
//!   from the expanded list (no error), as long as at least one
//!   surviving, non-excluded match remains — confirmed directly.
//!
//! **Deliberately narrower than real BuildKit** in one specific way:
//! a pattern segment containing `**` *mixed* with other characters
//! (`a**b`, rather than a segment that's *exactly* `**`) falls back to
//! this crate's own [`crate::glob::match_pattern`] for that segment,
//! which collapses consecutive `*`s into an ordinary single-`*`
//! (never crossing `/`) rather than replicating BuildKit's own
//! regex-based "any number of path segments, even zero" semantics for
//! that mid-segment case. Every real `.dockerignore` this crate's own
//! authors could find in practice only ever uses `**` as a whole path
//! segment (`**/foo`, `foo/**`, `**/foo/**`) — exactly what's
//! implemented here — so this is judged a safe, narrow first
//! increment rather than a correctness gap worth the added
//! complexity of a real regex engine (this project avoids one
//! entirely — see the workspace `Cargo.toml`'s own dependency list)
//! purely for an edge case this codebase has never seen in a real
//! Containerfile's own build context.

use crate::glob::{self, BadPattern};

/// Read `.dockerignore` file contents into an ordered list of raw
/// pattern strings, applying exactly `ignorefile.ReadAll`'s own rules:
/// a UTF-8 BOM header, if present, is stripped from the very first
/// line only; a line is a comment (and skipped) only if its *very
/// first* character, before any trimming at all, is `#` (so a line
/// with leading whitespace before a `#` is **not** a comment, matching
/// the real Go source's own check ordering exactly); blank lines
/// (after trimming) are skipped; a leading `!` (negation) is set
/// aside before the rest of the pattern is cleaned; every remaining
/// pattern is lexically cleaned ([`clean_path`], a direct port of
/// Go's own `filepath.Clean`) and a single leading `/`, if present, is
/// stripped (so `/some/path` and `some/path` are equivalent, matching
/// real `.dockerignore` semantics — a leading slash is not a
/// "rooted, top-level-only" marker the way one might expect from
/// shell glob intuition).
pub fn parse(text: &str) -> Vec<String> {
    const UTF8_BOM: &str = "\u{feff}";
    let mut excludes = Vec::new();
    for (index, raw_line) in text.lines().enumerate() {
        let line = if index == 0 {
            raw_line.strip_prefix(UTF8_BOM).unwrap_or(raw_line)
        } else {
            raw_line
        };
        // Comment check happens on the *untrimmed* line, exactly
        // matching `strings.HasPrefix(pattern, "#")` before any
        // `TrimSpace` call in the real source.
        if line.starts_with('#') {
            continue;
        }
        let mut pattern = line.trim();
        if pattern.is_empty() {
            continue;
        }
        let invert = pattern.starts_with('!');
        if invert {
            pattern = pattern[1..].trim();
        }
        let mut cleaned = String::new();
        if !pattern.is_empty() {
            cleaned = clean_path(pattern);
            if cleaned.len() > 1 && cleaned.starts_with('/') {
                cleaned = cleaned[1..].to_string();
            }
        }
        if invert {
            cleaned.insert(0, '!');
        }
        excludes.push(cleaned);
    }
    excludes
}

/// A direct, Unix-only port of Go's own `path/filepath.Clean`
/// (verified directly against the real `go1.22.2` toolchain installed
/// on this development host — see this module's own test for the
/// exact probe cases run through `filepath.Clean` first): returns the
/// shortest, lexically equivalent form of `path` (redundant `/`s
/// collapsed, `.` elements removed, a `..` element resolved against
/// the previous real element when possible), never touching the
/// filesystem itself. An empty input cleans to `"."`, matching Go's
/// own behavior exactly.
pub fn clean_path(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let bytes = path.as_bytes();
    let n = bytes.len();
    let rooted = bytes[0] == b'/';
    // A real port of Go's own `lazybuf`: `buf` only ever grows (it
    // may hold "stale" bytes beyond the current logical length `w`,
    // left over from a backtracked-away `..` element) — `write`
    // overwrites in place when `w` is still within `buf`'s existing
    // length, matching Go's own `lazybuf.append`'s identical trick.
    // Implementing this with a real, always-truncated `Vec` instead
    // (popping bytes immediately on backtrack) looks equivalent but
    // isn't: peeking at the byte a backtrack is about to discard, to
    // decide whether to keep backtracking, needs that stale byte
    // still physically present — an earlier draft of this function
    // popped eagerly instead and silently produced `"abc//ghi"` for
    // `clean_path("abc/def/../ghi")` (want `"abc/ghi"`) as a direct
    // result, caught by this module's own real-Go-toolchain-verified
    // test table below.
    let mut buf: Vec<u8> = Vec::with_capacity(n);
    let mut w = 0usize;
    let mut r = 0usize;
    let mut dotdot = 0usize;

    fn write(buf: &mut Vec<u8>, w: &mut usize, b: u8) {
        if *w == buf.len() {
            buf.push(b);
        } else {
            buf[*w] = b;
        }
        *w += 1;
    }

    if rooted {
        write(&mut buf, &mut w, b'/');
        r = 1;
        dotdot = 1;
    }
    while r < n {
        if bytes[r] == b'/' {
            // Empty path element.
            r += 1;
        } else if bytes[r] == b'.' && (r + 1 == n || bytes[r + 1] == b'/') {
            // `.` element.
            r += 1;
        } else if bytes[r] == b'.'
            && r + 1 < n
            && bytes[r + 1] == b'.'
            && (r + 2 == n || bytes[r + 2] == b'/')
        {
            // `..` element: remove back to the last real separator.
            r += 2;
            if w > dotdot {
                w -= 1;
                while w > dotdot && buf[w] != b'/' {
                    w -= 1;
                }
            } else if !rooted {
                if w > 0 {
                    write(&mut buf, &mut w, b'/');
                }
                write(&mut buf, &mut w, b'.');
                write(&mut buf, &mut w, b'.');
                dotdot = w;
            }
        } else {
            // A real path element.
            if (rooted && w != 1) || (!rooted && w != 0) {
                write(&mut buf, &mut w, b'/');
            }
            while r < n && bytes[r] != b'/' {
                write(&mut buf, &mut w, bytes[r]);
                r += 1;
            }
        }
    }
    if w == 0 {
        write(&mut buf, &mut w, b'.');
    }
    buf.truncate(w);
    // Every byte written above came directly from the input `&str`
    // (already valid UTF-8) or is one of the ASCII bytes `/`/`.`
    // this function itself writes — never invalid UTF-8.
    String::from_utf8(buf).expect("clean_path only ever emits bytes copied from valid UTF-8 input")
}

/// One path segment of a compiled pattern: either a literal `**`
/// (matching zero or more whole path segments, including crossing
/// `/`, exactly BuildKit's own two-star handling), or an ordinary
/// glob segment matched via [`glob::match_pattern`] (which, since a
/// single segment never itself contains `/`, already gives the
/// right "never crosses a path separator" behavior for `*`/`?`/a
/// bracket expression with no extra work needed here).
#[derive(Debug, Clone)]
enum Segment {
    DoubleStar,
    Glob(String),
}

/// One compiled `.dockerignore` pattern, split into path segments,
/// plus whether it's a `!`-negated (re-inclusion) pattern.
#[derive(Debug, Clone)]
struct CompiledPattern {
    segments: Vec<Segment>,
    exclusion: bool,
}

impl CompiledPattern {
    fn compile(raw: &str) -> Result<Self, BadPattern> {
        let mut pattern = raw.trim();
        let mut exclusion = false;
        if let Some(rest) = pattern.strip_prefix('!') {
            if rest.is_empty() {
                // Matches real `patternmatcher.New`'s own explicit
                // rejection of a bare "!" pattern -- reported as the
                // same `BadPattern` this module's own callers already
                // know how to surface, rather than a second error type
                // for what's really the same "this pattern can never
                // be used" outcome.
                return Err(BadPattern);
            }
            exclusion = true;
            pattern = rest;
        }
        let cleaned = clean_path(pattern);
        let mut segments = Vec::new();
        for dir in cleaned.split('/') {
            if dir == "**" {
                segments.push(Segment::DoubleStar);
            } else {
                // Validated eagerly here (an unterminated `[...]`
                // class, for instance) so a malformed pattern is
                // rejected once, at compile time -- matching real
                // `patternmatcher.New`'s own up-front `filepath.
                // Match(p, ".")` syntax check -- rather than only
                // ever surfacing the error lazily, the first time a
                // real context path happens to reach this segment.
                glob::match_pattern(dir, "")?;
                segments.push(Segment::Glob(dir.to_string()));
            }
        }
        Ok(CompiledPattern {
            segments,
            exclusion,
        })
    }

    /// Whether this pattern's own segments match `path_segments`
    /// exactly (every segment consumed on both sides) -- a direct,
    /// recursive port of matching against the regex `compile()` would
    /// have built, without ever constructing one (see this module's
    /// own top doc comment for why a real regex engine is
    /// deliberately not a dependency here).
    fn matches_segments(segments: &[Segment], path_segments: &[&str]) -> bool {
        match segments.first() {
            None => path_segments.is_empty(),
            Some(Segment::DoubleStar) => {
                if segments.len() == 1 {
                    // A *trailing* `**` (the last segment in the
                    // whole pattern) requires at least one further
                    // path segment -- real BuildKit's own `compile()`
                    // takes a fast `prefixMatch` path for exactly
                    // this shape (no other wildcard earlier in the
                    // pattern), checking whether `path` has the
                    // pattern's own literal prefix *up to and
                    // including* the `/` right before the final `**`
                    // as a real string prefix, which by construction
                    // can never equal `path` exactly (there's always
                    // at least one more character after that `/` for
                    // a real prefix match). Confirmed directly
                    // against real `podman build`: a `subdir/**`
                    // pattern removed everything *inside* `subdir`
                    // but left `subdir` itself (now empty) in place —
                    // not "zero or more" the way a `**` in the middle
                    // of a pattern genuinely is (see the general case
                    // below, unchanged).
                    return !path_segments.is_empty();
                }
                (0..=path_segments.len())
                    .any(|i| Self::matches_segments(&segments[1..], &path_segments[i..]))
            }
            Some(Segment::Glob(pat)) => match path_segments.split_first() {
                None => false,
                Some((head, tail)) => {
                    glob::match_pattern(pat, head).unwrap_or(false)
                        && Self::matches_segments(&segments[1..], tail)
                }
            },
        }
    }

    /// Whether this pattern matches `path` itself, *or* any of
    /// `path`'s own ancestor directories -- a direct port of real
    /// `MatchesOrParentMatches`'s own "also check every parent
    /// prefix" loop, reshaped as "try every prefix length of `path`'s
    /// own segments" (equivalent: the full path is the deepest
    /// prefix, and every shorter prefix is exactly one ancestor
    /// directory's own path) rather than replicating the original's
    /// own top-level/`Dir()`-based loop structure line for line.
    fn matches_or_parent_matches(&self, path_segments: &[&str]) -> bool {
        (0..=path_segments.len())
            .any(|k| Self::matches_segments(&self.segments, &path_segments[..k]))
    }
}

/// A compiled `.dockerignore` (or an empty one, for a build context
/// with no `.dockerignore` file at all — [`DockerIgnore::empty`]),
/// ready to answer [`DockerIgnore::is_ignored`] for any real
/// context-relative path.
#[derive(Debug, Clone)]
pub struct DockerIgnore {
    patterns: Vec<CompiledPattern>,
    has_negation: bool,
}

impl DockerIgnore {
    /// No patterns at all -- [`DockerIgnore::is_ignored`] always
    /// returns `false`. What a build context with no `.dockerignore`
    /// file present gets.
    pub fn empty() -> Self {
        DockerIgnore {
            patterns: Vec::new(),
            has_negation: false,
        }
    }

    /// Compile `raw_patterns` (as returned by [`parse`], or any
    /// equivalent list of already-line-split pattern strings) into a
    /// matcher. Fails on the first syntactically invalid pattern,
    /// matching real `patternmatcher.New`'s own all-or-nothing
    /// behavior (a build never starts with a partially-compiled
    /// `.dockerignore`).
    pub fn compile(raw_patterns: &[String]) -> Result<Self, BadPattern> {
        let mut patterns = Vec::with_capacity(raw_patterns.len());
        let mut has_negation = false;
        for raw in raw_patterns {
            if raw.trim().is_empty() {
                continue;
            }
            let compiled = CompiledPattern::compile(raw)?;
            has_negation |= compiled.exclusion;
            patterns.push(compiled);
        }
        Ok(DockerIgnore {
            patterns,
            has_negation,
        })
    }

    /// Whether any pattern is a `!`-negated re-inclusion -- when
    /// `false`, a directory whose own path [`is_ignored`] can never
    /// have a re-included descendant, so a caller walking the build
    /// context (`ociman build`'s own `copy_path_recursive`/
    /// `walk_relative_paths`) can safely skip descending into it
    /// entirely instead of visiting every entry underneath just to
    /// confirm none of them matter — a real, measurable saving for a
    /// context with a large ignored directory (`node_modules`/`.git`,
    /// in practice the overwhelmingly common real-world case), and
    /// exactly what podman/buildah's own `.dockerignore`-aware walk
    /// does too (confirmed directly: an excluded directory with no
    /// negation anywhere in the file gets no empty entry at all in
    /// the committed layer, not even its own now-empty directory
    /// node).
    ///
    /// [`is_ignored`]: DockerIgnore::is_ignored
    pub fn has_negation(&self) -> bool {
        self.has_negation
    }

    /// Whether `path` (a `/`-separated path relative to the build
    /// context root, however written -- a leading `/`, `./`, or `..`
    /// segments are all normalized away via [`clean_path`] before
    /// matching) is excluded by this `.dockerignore`.
    ///
    /// Every pattern is evaluated, in file order, against `path`
    /// itself and every one of its own ancestor directories (see
    /// [`CompiledPattern::matches_or_parent_matches`]); a later
    /// pattern always overrides an earlier one for the exact same
    /// path, and (matching real `MatchesOrParentMatches`'s own
    /// optimization) an exclusion pattern only changes anything once
    /// something already matched, and vice versa for a `!`-negated
    /// one.
    pub fn is_ignored(&self, path: &str) -> bool {
        let mut cleaned = clean_path(path);
        // Same normalization [`parse`] applies to every pattern: a
        // single leading `/` is not a "context-root-only" marker,
        // just an equivalent way of writing the same relative path.
        if cleaned.len() > 1 && cleaned.starts_with('/') {
            cleaned = cleaned[1..].to_string();
        }
        let segments: Vec<&str> = if cleaned == "." {
            Vec::new()
        } else {
            cleaned.split('/').collect()
        };
        let mut matched = false;
        for pattern in &self.patterns {
            if pattern.exclusion != matched {
                continue;
            }
            if pattern.matches_or_parent_matches(&segments) {
                matched = !pattern.exclusion;
            }
        }
        matched
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_path_matches_every_probe_confirmed_against_the_real_go_toolchain() {
        // Every pair here was first run through a real `go run`
        // program calling `filepath.Clean` on the real `go1.22.2`
        // toolchain installed on this development host, confirming
        // the expected result before being copied in here (see this
        // module's own top doc comment).
        let cases: &[(&str, &str)] = &[
            ("", "."),
            ("abc", "abc"),
            ("abc/def/../ghi", "abc/ghi"),
            ("/../abc", "/abc"),
            ("abc//def", "abc/def"),
            ("abc/./def", "abc/def"),
            ("abc/def/..", "abc"),
            (".", "."),
            ("..", ".."),
            ("../..", "../.."),
            ("../../abc", "../../abc"),
            ("/abc/def/", "/abc/def"),
            ("/", "/"),
            ("a/./b/../../c", "c"),
            ("//foo", "/foo"),
            ("./foo", "foo"),
            ("foo/.", "foo"),
            ("a/../../b", "../b"),
            ("..a", "..a"),
            ("a..", "a.."),
            ("a/..b/c", "a/..b/c"),
        ];
        for &(input, want) in cases {
            assert_eq!(clean_path(input), want, "clean_path({input:?})");
        }
    }

    fn ignore(patterns: &[&str]) -> DockerIgnore {
        DockerIgnore::compile(&patterns.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn parse_skips_comments_only_when_hash_is_the_very_first_character() {
        let text = "# real comment\n  # not a comment (leading space)\nkeep\n";
        let parsed = parse(text);
        // Only *leading/trailing* whitespace is trimmed -- the space
        // right after the `#` (not the very first character of the
        // untrimmed line) is preserved verbatim, matching the real
        // source's own single `TrimSpace` call.
        assert_eq!(parsed, vec!["# not a comment (leading space)", "keep"]);
    }

    #[test]
    fn parse_strips_utf8_bom_only_on_the_first_line() {
        let text = "\u{feff}foo\nbar\n";
        assert_eq!(parse(text), vec!["foo", "bar"]);
    }

    #[test]
    fn parse_strips_a_single_leading_slash_but_not_a_bare_slash_alone() {
        assert_eq!(parse("/some/path\n"), vec!["some/path"]);
        assert_eq!(parse("/\n"), vec!["/"]);
    }

    #[test]
    fn parse_preserves_negation_prefix_across_cleaning() {
        assert_eq!(parse("!/some/path\n"), vec!["!some/path"]);
    }

    #[test]
    fn empty_matcher_ignores_nothing() {
        assert!(!DockerIgnore::empty().is_ignored("anything/at/all"));
    }

    #[test]
    fn exact_top_level_pattern_matches_only_that_file() {
        let m = ignore(&["ignored.txt"]);
        assert!(m.is_ignored("ignored.txt"));
        assert!(!m.is_ignored("keep.txt"));
        assert!(!m.is_ignored("subdir/ignored.txt"));
    }

    #[test]
    fn exact_pattern_also_ignores_everything_under_a_matching_directory() {
        // A directory pattern with no wildcard still excludes every
        // real file underneath it -- confirmed directly against real
        // `podman build` (see this module's own top doc comment).
        let m = ignore(&["subdir"]);
        assert!(m.is_ignored("subdir"));
        assert!(m.is_ignored("subdir/file.txt"));
        assert!(m.is_ignored("subdir/nested/file.txt"));
        assert!(!m.is_ignored("other/file.txt"));
    }

    #[test]
    fn bare_star_pattern_never_crosses_a_path_separator() {
        // Confirmed directly against real `podman build`: a bare
        // `*.log` pattern left a nested `subdir/nested.log` in place.
        let m = ignore(&["*.log"]);
        assert!(m.is_ignored("top.log"));
        assert!(!m.is_ignored("subdir/nested.log"));
    }

    #[test]
    fn double_star_prefix_matches_at_any_depth() {
        // Confirmed directly against real `podman build`: `**/*.log`
        // removed both a top-level and a nested `.log` file.
        let m = ignore(&["**/*.log"]);
        assert!(m.is_ignored("top.log"));
        assert!(m.is_ignored("subdir/nested.log"));
        assert!(m.is_ignored("a/b/c/deep.log"));
        assert!(!m.is_ignored("keep.txt"));
    }

    #[test]
    fn trailing_double_star_matches_everything_under_a_directory() {
        let m = ignore(&["subdir/**"]);
        assert!(m.is_ignored("subdir/file.txt"));
        assert!(m.is_ignored("subdir/nested/file.txt"));
        assert!(!m.is_ignored("subdir"));
        assert!(!m.is_ignored("other/file.txt"));
    }

    #[test]
    fn negation_re_includes_one_specific_file_even_under_an_excluded_directory() {
        // Confirmed directly against real `podman build` (see this
        // module's own top doc comment): unlike `.gitignore`, a later
        // `!` pattern can re-include a file even though its own
        // parent directory was excluded by an earlier pattern.
        let m = ignore(&["subdir", "!subdir/keep.txt"]);
        assert!(m.is_ignored("subdir/other.txt"));
        assert!(!m.is_ignored("subdir/keep.txt"));
        assert!(m.has_negation());
    }

    #[test]
    fn star_then_negation_keeps_only_the_re_included_file() {
        // Confirmed directly against real `podman build`: `*` then
        // `!keep.txt` excluded even the Containerfile and
        // `.dockerignore` themselves, leaving only `keep.txt`.
        let m = ignore(&["*", "!keep.txt"]);
        assert!(m.is_ignored("Containerfile"));
        assert!(m.is_ignored(".dockerignore"));
        assert!(!m.is_ignored("keep.txt"));
    }

    #[test]
    fn later_pattern_order_always_overrides_an_earlier_one_for_the_same_path() {
        let m = ignore(&["!keep.txt", "keep.txt"]);
        assert!(m.is_ignored("keep.txt"));
    }

    #[test]
    fn has_negation_is_false_with_no_negation_pattern_at_all() {
        assert!(!ignore(&["a", "b/*.log"]).has_negation());
        assert!(ignore(&["a", "!b"]).has_negation());
    }

    #[test]
    fn a_leading_slash_and_relative_dot_segments_in_the_queried_path_are_normalized_first() {
        let m = ignore(&["foo"]);
        assert!(m.is_ignored("/foo"));
        assert!(m.is_ignored("./foo"));
    }

    #[test]
    fn a_bare_exclamation_point_pattern_is_a_bad_pattern() {
        assert!(DockerIgnore::compile(&["!".to_string()]).is_err());
    }

    #[test]
    fn an_unterminated_bracket_expression_is_rejected_at_compile_time() {
        assert!(DockerIgnore::compile(&["[abc".to_string()]).is_err());
    }

    #[test]
    fn blank_and_whitespace_only_raw_patterns_are_skipped_not_rejected() {
        assert!(DockerIgnore::compile(&["".to_string(), "   ".to_string()]).is_ok());
    }
}
