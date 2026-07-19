# Design note 0042: applying `$VAR`/`${VAR}` expansion to each stage's own instructions

Status: implemented
Scope: `crates/oci-dockerfile/src/expand_stage.rs`.

0040 shipped the expansion engine; 0041 shipped grouping instructions
into stages by `FROM` boundaries — both deliberately left unwired,
each waiting on the other. This increment finally combines them:
walking each stage's own instructions in order, threading an
accumulating `ARG`/`ENV` environment through, and applying expansion
to exactly the instruction fields real BuildKit itself expands.

## Grounded directly against the real dispatch driver

Checked directly against BuildKit's own `convert.go`
(`~/git/moby/vendor/github.com/moby/buildkit/frontend/dockerfile/
dockerfile2llb/convert.go`), not re-derived from documentation:

* **`FROM`'s own `base_name`/`platform` only ever see the meta-`ARG`
  environment** (`ARG`s declared before the very first `FROM`) — never
  a stage's own local `ARG`/`ENV`, even a stage appearing later in the
  same file, since a `FROM` always starts a fresh stage before any of
  that stage's own instructions have run. `expand_meta_args` builds
  this environment once (each meta-`ARG`'s own default expanded
  against the meta environment accumulated *so far* from earlier
  meta-`ARG`s, matching real `buildMetaArgs`), and `expand_stage` uses
  it only for the `FROM` header, never for the stage body.
* **Each stage starts with a completely fresh environment** — no
  carry-over from an earlier stage's own `ENV`/`ARG` at all (each
  stage builds from its own, independent base image).
* **A meta-`ARG` is not automatically inherited by a stage.** It must
  be re-declared, bare (`ARG NAME`, no `=value`), inside the stage to
  become usable there — the stage's own bare re-declaration is what
  pulls the meta-arg's value in; an un-re-declared meta-arg is simply
  absent from that stage's own environment.
* **Every substitution within *one* `ENV` instruction sees the same
  environment snapshot.** `ENV a=hello b=$a` does **not** make `b` see
  `a`'s own brand-new value — both pairs expand against the
  environment as it was *before* this instruction ran. Implemented by
  cloning the environment once, before processing an `ENV`
  instruction's own pairs, rather than updating it pair by pair.
* **`RUN`/`CMD`/`ENTRYPOINT`/`SHELL`'s own command-line text is never
  expanded, at any point** — the shell running inside the container
  does its own `$VAR` expansion at container-build time, using the
  `RUN` step's own environment, not this crate's. Every other
  instruction this crate covers (`ENV`, `ARG`'s own default, `LABEL`,
  `COPY`/`ADD`'s flags and sources/dest, `WORKDIR`, `USER`,
  `STOPSIGNAL`, `MAINTAINER`, `EXPOSE`, `VOLUME`) does expand.

## A deliberate simplification, documented rather than silently accepted

`EXPOSE`'s own port list is sorted by the parser (0039) *before*
expansion ever runs, on the raw, unexpanded strings — matching real
`parseExpose`'s own behavior of sorting at parse time. This increment
does *not* re-sort after expanding a port list that happens to contain
a variable (e.g. `EXPOSE $PORT`), since a real-world `EXPOSE` using a
variable at all is rare enough that preserving the parser's own
already-committed order, rather than risking a second, subtly
different sort based on expanded values, is the more conservative
choice.

## API

`expand_meta_args(meta_args) -> Result<HashMap<String, String>, String>`
and `expand_stage(global_args, stage) -> Result<Stage, String>` — the
latter returns a new `Stage` with the same shape, every relevant
field's string content expanded in place. `Instruction::Arg`'s own
`default` field, once it comes back out of `expand_stage`, holds the
*resolved* value (from its own inline default, or from `global_args`
if bare and re-declared) rather than the original literal text — the
one field whose meaning deliberately shifts from "as written" to "as
resolved" once expansion has run, since a later build-execution
increment will need exactly that resolved value to set as a real
environment variable.

## Real, automated tests

8 new unit tests, each built around one of the real rules above rather
than an arbitrary example: `FROM` expanding only against meta-args;
`ENV`/`WORKDIR` correctly seeing the environment accumulated so far;
one `ENV` instruction's own pairs all seeing the *same* starting
snapshot (not each other's new values); a meta-arg correctly *not*
leaking into a stage unless re-declared, and correctly available once
it is; `RUN`/`CMD`/`ENTRYPOINT` provably left untouched even when their
own text contains a variable reference that *is* declared; `COPY`
expanding its flags, sources, and destination; and each stage starting
completely fresh (a variable set in an earlier stage not leaking into
a later one).

## Performance

Not called from anywhere yet (no build-execution increment exists to
call it), so zero runtime impact on any hot path by construction, same
reasoning 0039/0040/0041's own "Performance" sections used.

## What's still not here

* `ONBUILD`, `HEALTHCHECK`, heredocs, and the BuildKit-only flags
  0039 already deferred remain deferred.
* `--build-arg` (an external override for a meta-`ARG`'s own value)
  has no representation at all yet — `expand_meta_args` only ever sees
  each `ARG`'s own inline default; there's no way yet to supply a
  value from outside the Dockerfile itself.
* The glob-pattern shell operators (`#`/`##`, `%`/`%%`, `/pattern/
  repl`) 0040 already deferred remain deferred.
* Dependency-ordered execution, target-stage selection, the build
  cache, and actual build execution (`RUN` steps via
  `oci-runtime-core`, layer commits via `oci-store`) — still all
  future work, exactly as 0039's own module doc has always scoped.
