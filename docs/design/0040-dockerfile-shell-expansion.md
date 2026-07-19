# Design note 0040: `$VAR`/`${VAR}` shell expansion engine (milestone 4)

Status: implemented (the expansion engine itself; not yet wired into
`Instruction` dispatch — see "What's still not here")
Scope: `crates/oci-dockerfile/src/shell_expand.rs`.

0039 shipped the Dockerfile parser itself, explicitly deferring
`ARG`/`ENV` variable substitution within other instructions' own
argument text as "a separate, later increment." This increment is
that engine — matching this project's own established pattern of
shipping and thoroughly testing a foundational primitive on its own
before wiring it into anything that needs surrounding machinery this
crate doesn't have yet (build-stage grouping, in this case — see
"What's still not here").

## Grounded directly against the real implementation, with real surprises found along the way

Checked directly against BuildKit's own lexer
(`~/git/moby/vendor/github.com/moby/buildkit/frontend/dockerfile/
shell/lex.go`) and its own golden test fixture data
(`frontend/dockerfile/shell/envVarTest` — the Go test file that drives
these rows isn't vendored, but the fixture rows themselves are real,
exact expected input/output pairs). Several real behaviors here are
genuinely surprising and would have been easy to get wrong from
documentation prose or intuition alone:

* **`$$` is not a literal-dollar escape.** Real shells (and BuildKit's
  own lexer, deliberately matching them) treat `$` immediately after
  another `$` as the shell "special parameter" named `$` (the PID
  variable, in a real shell) — since this crate never declares
  anything by that name, `$$` simply expands to an empty string. A
  literal dollar sign needs an actual backslash (`\$`), not doubling.
  This crate's own `double_dollar_is_not_a_literal_dollar_escape` test
  exists specifically because this is the opposite of what a
  reasonable first guess would assume.
* **Variable-name matching is greedy**, exactly like a real shell:
  `$PWDx` looks up a variable literally named `PWDx`, not `PWD`
  followed by a literal `x` — `${PWD}x` is how a real Dockerfile
  disambiguates this. This crate's own first version of the
  `bare_variable` test got this wrong (assumed `$PWDx` meant `PWD` +
  literal `x`) until actually running it against the implementation
  surfaced the mismatch, which is exactly the kind of thing worth
  writing down here rather than silently fixing the test and moving
  on.
* **A never-declared variable silently expands to an empty string**,
  not an error and not left as a literal `$VAR` — confirmed directly
  against the fixture (`he${hi}xx` → `hexx`, `hi` never declared).
  Real BuildKit does separately emit a *lint warning* for this case,
  which this crate doesn't implement any linting for.
* **A name starting with a digit only ever consumes further digits**
  (bash's positional-parameter convention, e.g. `$1`), never letters
  after that — unlike an ordinary name, which keeps consuming letters/
  digits/underscore.

## What's implemented

`expand(word, env) -> Result<String, String>`:

* `$VAR` / `${VAR}` — basic lookup.
* `${VAR:-word}` / `${VAR-word}` — default if unset (`:`-form: also if
  set-but-empty).
* `${VAR:+word}` / `${VAR+word}` — alternate if set (`:`-form: also
  requires non-empty).
* `${VAR:?message}` / `${VAR?message}` — a hard error (not a silent
  empty-string) if unset (`:`-form: also if empty), the one construct
  where a missing variable deliberately fails rather than degrading
  gracefully — matches real bash/BuildKit semantics exactly.
* Nested expansion within any of the three modifiers' own "word"
  portion (`${A:-${B:-c}}`), implemented for free by simple recursion:
  a nested `${...}` consumes its own matching closing `}` before
  returning, so the outer scan never mistakes an inner closing brace
  for its own.
* Backslash-escaping of a literal `$` (`\$FOO`, `\${FOO}`) — and
  *only* of `$`; a backslash not immediately followed by `$` is left
  completely untouched (this crate's own lexer, `0039`, already
  consumes the file's own line-continuation escape token separately;
  word-level backslash-escaping is a distinct, narrower mechanism that
  only ever applies to `$`).

## Deliberately not implemented yet

The glob-pattern operators (`${VAR#pattern}`/`${VAR##pattern}`
prefix-strip, `${VAR%pattern}`/`${VAR%%pattern}` suffix-strip,
`${VAR/pattern/repl}`/`${VAR//pattern/repl}` substitution) — each
needs its own glob-to-regex conversion, meaningfully more machinery
than everything else in this module, and rare enough in practice to
defer; a Dockerfile using one of them is a clear parse error here, not
a silent misparse.

## Real, automated tests

20 new unit tests, nearly all directly mirroring a real row from
BuildKit's own `envVarTest` fixture data rather than an invented case:
every modifier (`-`/`+`/`?`, both `:`-and non-`:`-forms), nested
defaults, the greedy-name-matching and leading-digit rules, the
`$$`/backslash-escape surprises documented above, and every
"bad substitution" error case (`${}`, `${:xx}`, `${XXX:YYY}`, an
unterminated `${XXX`).

## Performance

Not called from anywhere yet (see below), so zero runtime impact on
any hot path by construction — same reasoning 0039's own "Performance"
section used for the parser itself.

## What's still not here

* **Not wired into `Instruction` dispatch at all yet.** Real expansion
  needs to know the accumulated `ARG`/`ENV` environment *at the point
  each instruction appears*, which resets at each `FROM` (a new build
  stage starts with a mostly-fresh environment; only meta-`ARG`s
  declared before the very first `FROM`, and only if re-declared
  inside the stage, carry over — a real, checked-directly rule, not
  assumed) — a scoping question that only makes real sense once
  instructions are grouped into stages by their own `FROM` boundaries,
  which is 0039's own already-documented next increment (the build
  graph). Wiring expansion in properly is a natural companion to that
  increment, not a separate loose end.
* The glob-pattern operators listed above.
* `RUN`/`CMD`/`ENTRYPOINT`/`SHELL`'s own command-line text is
  deliberately *never* meant to go through this engine at all, even
  once wiring lands — confirmed directly against real BuildKit (`Run
  Command.Expand` only expands `--mount=` flag values, never the
  actual shell command line) — the shell running inside the container
  does its own `$VAR` expansion at container-build time, using the
  `RUN` step's own environment. This crate's future wiring increment
  needs to preserve that distinction, not accidentally expand
  everything uniformly.
