# Design note 0039: Dockerfile/Containerfile parser (milestone 4, foundational primitive)

Status: implemented (the lexer + per-instruction parser only — see
"What's still not here"; not yet wired into any binary)
Scope: `crates/oci-dockerfile` (previously an empty stub since
milestone 1).

The first increment of milestone 4. Matches this project's own
established pattern for starting a new area (`oci-spec-types` before
anything used its types in milestone 2; `systemd_cgroup::create_scope`
before anything called it in 0033): ship and thoroughly test the
foundational primitive on its own before any binary (`ociman build`,
which doesn't exist as a command yet) assembles it into something a
user actually runs.

## Grounded directly against the real, current implementation, not documentation prose

Every lexical/grammar rule below was checked directly against
BuildKit's own Dockerfile frontend
(`~/git/moby/vendor/github.com/moby/buildkit/frontend/dockerfile/
{parser,instructions}/*.go`) — the actively-maintained parser real
`docker build`/`podman build` both ultimately rely on (podman/buildah's
own vendored parser, `~/git/podman/vendor/github.com/openshift/
imagebuilder/dockerfile/parser`, turned out to be a stale fork of an
older version of the same code, missing several of BuildKit's newer
behaviors — checked, not assumed, before treating BuildKit's copy as
the one authoritative reference). Several of the rules this uncovered
are measurably more precise than what the human-readable Dockerfile
reference documentation says, and would have been easy to get subtly
wrong by reasoning from prose alone:

* **Parser directives (`# escape=`, `# syntax=`, `# check=`) are only
  honored if they're the *very first* comment lines in the file, with
  nothing else — not even an ordinary comment — interrupting them.**
  The moment directive-scanning hits a line that isn't a `# key=value`
  comment with a recognized key, it stops *permanently* for the rest
  of the file. A real, official BuildKit test fixture
  (`escape-after-comment`) exists specifically to nail this down: three
  ordinary comments before a `# escape=` line mean that escape
  directive is never actually honored at all, and the default backslash
  escape stays in effect — this crate's own
  `scan_directives_stops_at_the_first_ordinary_comment` test is that
  same case.
* **Comments and blank lines inside a multi-line continuation are
  transparently spliced out, not treated as ending it.** A `RUN`
  instruction can have a `# comment` line, or an entirely blank line,
  in the middle of its own backslash-continued lines, and both are
  silently dropped while the continuation keeps going — confirmed
  against the real `continueIndent` fixture, replicated here as
  `splice_lines_matches_the_real_continue_indent_fixture_shape`.
* **An "escaped escape" at line-end is deliberately *not* a
  continuation.** `foo\\` (two backslashes) at the end of a line does
  *not* continue to the next line — a real, documented quirk of the
  upstream regex having no negative-lookahead support, not an
  oversight replicated here by accident
  (`splice_lines_does_not_treat_an_escaped_escape_as_continuation`).
* **`EXPOSE`'s own port list is sorted, not kept in source order** —
  and that sort is lexicographic (byte-wise string comparison), not
  numeric, matching Go's own plain `slices.Sort([]string)` — this
  crate's own first version of the corresponding test had the wrong
  expected order (assumed numeric sorting) until actually running it
  against the implementation surfaced the mismatch.
* **`ADD` genuinely has no `--from` flag at all** (only `COPY` can
  reach across build stages) — enforced here as a real parse error,
  not merely undocumented.
* **`SHELL` must be JSON/exec-array form; shell form is a hard
  error** — the one `RUN`/`CMD`/`ENTRYPOINT`-shaped instruction that
  doesn't actually accept a plain shell-form argument.

## Instructions covered this increment, and what's deliberately deferred

Covered, with real per-instruction argument grammar (flags included
where the real syntax has long been stable): `FROM` (multi-stage `AS`,
`--platform`), `RUN`/`CMD`/`ENTRYPOINT` (shell vs. JSON/exec form),
`COPY` (`--from`/`--chown`/`--chmod`), `ADD` (`--chown`/`--chmod`),
`ENV`/`LABEL` (both the legacy two-word form and the modern
multi-assignment form), `ARG` (with an optional default), `WORKDIR`,
`USER`, `EXPOSE`, `VOLUME`, `SHELL`, `STOPSIGNAL`, `MAINTAINER`
(deprecated upstream but still valid syntax, not a parse error).

Deliberately not implemented yet, each a separate, later increment:

* `ARG`/`ENV` variable substitution/interpolation within other
  instructions' own argument text (e.g. `RUN echo $FOO`) — every
  `Instruction` here carries its arguments exactly as written; the
  small real Containerfile this crate's own test parses deliberately
  includes an unexpanded `${VERSION}` reference to make this explicit,
  not accidental.
* `ONBUILD` and `HEALTHCHECK` — both have a genuinely different, more
  involved grammar (recursive sub-instruction parsing for `ONBUILD`;
  a two-stage `NONE`-or-`CMD` parse with its own duration/retry flags
  for `HEALTHCHECK`) that didn't fit this increment's own scope;
  both fail to parse with a clear, explicit error rather than being
  silently dropped or misparsed.
* Heredocs (`<<EOF ... EOF`) and every BuildKit-only flag (`RUN
  --mount=`/`--network=`/`--security=`/`--device=`, `COPY --link`/
  `--parents`/`--exclude=`, `ADD --link`/`--keep-git-dir`/
  `--checksum=`/`--unpack`) — same treatment: a clear parse error, not
  a silent misparse.
* The build graph (stage DAG, dependency ordering, target selection)
  and build cache this crate's own module doc has always planned —
  `parse` only produces a flat instruction list; grouping instructions
  into stages by their own `FROM` boundaries is the natural next
  increment.
* A deliberate simplification within what *is* covered: flag *values*
  (`--chown=`, `--from=`, etc.) are parsed as plain whitespace-
  delimited tokens, not through the real parser's own considerably
  more intricate quote-aware flag tokenizer — every real-world
  Containerfile this project's own milestone actually needs to build
  only ever uses simple, unquoted flag values in practice.

## Real, automated tests

39 unit tests across `lexer`/`instruction`/the crate root: every
lexical rule above with its own dedicated test (several written only
*after* discovering the real behavior via the research above, not
guessed); a per-instruction test for each of the 15 covered
instructions' own grammar, including negative cases (`FROM` with the
wrong argument count, `COPY --frm=` — a typo'd flag — correctly
rejected, `ADD --from=` correctly rejected since `ADD` has no such
flag, `SHELL` in bare shell form correctly rejected); and one
integration-shaped test parsing a small but realistically-structured
Containerfile end to end (multi-stage `FROM`, `ARG`, continued `ENV`,
quoted `LABEL` values, `COPY --chown`, a multi-line `RUN`, sorted
`EXPOSE`, `ENTRYPOINT`/`CMD`) and checking the resulting instruction
list directly.

## Performance

Not wired into any binary yet, so zero runtime impact on any hot path
by construction — the same reasoning 0033's own "Performance" section
used for its own not-yet-wired-in primitive. The one new workspace
dependency edge this increment adds (`oci-dockerfile` now depends on
the already-present `serde_json`, for `RUN`/`CMD`/`COPY`/`ADD`/
`VOLUME`/`SHELL`'s own JSON-array argument form) doesn't affect any
existing binary's own build, since nothing links against
`oci-dockerfile` yet.

## What's still not here

* Everything listed under "deliberately deferred" above.
* No CLI surface at all yet — `ociman build` doesn't exist as a
  command; this increment is purely the parsing primitive underneath
  where it will eventually live.
* No build graph/stage-DAG/cache — the natural next increment once
  more of the instruction grammar (`ONBUILD`/`HEALTHCHECK`/variable
  interpolation) is either covered or deliberately still deferred with
  the caller aware of it.
