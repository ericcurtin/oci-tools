# Design note 0131: `.containerignore` support for `ociman build`

Status: implemented
Scope: `bin/ociman/src/build.rs` (new `read_ignore_patterns` helper,
called from `cmd_build` in place of its own inline `.dockerignore`-
only read); `tests/tests/ociman_build.rs` (2 new tests).

## Closing the gap 0130 explicitly flagged

0130's own "what this doesn't do yet" section named this directly:
"No `.containerignore` fallback name (real podman/buildah also accept
`.containerignore` as an alternate file name, preferring it over
`.dockerignore` when both exist) ... a small, well-scoped follow-up
if it turns out to matter in practice." Picked back up here.

## Verified against real buildah's own current source, not assumed

`~/git/podman/vendor/go.podman.io/buildah/pkg/parse/parse.go`'s own
`ContainerIgnoreFile` — confirmed, by tracing `executor.go`'s own
build setup, to be exactly what real `podman build`/`buildah build`
(non-remote) actually calls — is unambiguous for the plain, root-level
case this increment implements: `.containerignore` is tried first;
`.dockerignore` is only ever consulted as a fallback if
`.containerignore` doesn't exist at all. Independently confirmed with
two real `podman build` runs against a real local context: one with
only `.containerignore` present (worked exactly like `.dockerignore`
does); one with *both* present at once, each excluding a different
file — only `.containerignore`'s own pattern took effect, proving
`.dockerignore` genuinely isn't even read once `.containerignore`
exists, not merely "checked and then overridden".

## A genuine upstream inconsistency, deliberately not replicated yet

The same real source file has a *second* precedence rule, for a
per-Containerfile-named ignore file (`<dockerfile-name>.containerignore`/
`<dockerfile-name>.dockerignore`, e.g. a `Containerfile.dev`'s own
`Containerfile.dev.dockerignore`) — and that second rule is
*inconsistent* with the first: reading the real Go source line by
line, `.dockerignore` silently overwrites an already-found
`.containerignore` there, the opposite of the plain, root-level rule
above. Rather than replicate a real upstream self-inconsistency
without any real justification for *why* it differs (a Dockerfile
using a non-default name at all is itself a comparatively rare case),
this increment only implements the plain, root-level, internally-
consistent rule — `read_ignore_patterns`'s own doc comment records
this scope limit directly, for whoever picks up the per-Containerfile-
named case later.

## Implementation

A single new `read_ignore_patterns(context: &Path) -> anyhow::Result<
Vec<String>>` helper replaces `cmd_build`'s own previous inline
`.dockerignore`-only read: try `context.join(".containerignore")`
first; on `ErrorKind::NotFound`, fall back to `context.join(
".dockerignore")`; on `ErrorKind::NotFound` there too, no patterns at
all (nothing excluded, same as before this increment). Any *other* I/O
error at either stage still propagates as a real, clear error, exactly
like the read this replaces already did.

## Real, automated tests

Two new CLI-level integration tests in `tests/tests/ociman_build.rs`,
following the exact same real-build-plus-run-round-trip pattern 0130's
own tests already established: `.containerignore` alone excluding a
named file (same shape as `.dockerignore`'s own equivalent test); and
`.containerignore`/`.dockerignore` both present at once, each naming a
*different* file to exclude, confirming only `.containerignore`'s own
pattern ever takes effect. All pre-existing tests (including 0130's
own 8 `.dockerignore` tests and the full 53-test base `ociman_build.rs`
suite) still pass unmodified. Full `cargo build --workspace --locked`/
`cargo test --workspace --locked` (2 clean runs)/`cargo fmt --all
--check`/`cargo clippy --workspace --all-targets --locked -- -D
warnings`/`python3 ci/guards.py`/`cargo deny check` all clean.

## What this doesn't do yet

* The per-Containerfile-named ignore file case above (`<dockerfile-
  name>.containerignore`/`.dockerignore`) — deliberately deferred, see
  above.
* No `--ignorefile`/equivalent explicit-path override flag (real
  `podman build --ignorefile <path>` skips this whole resolution
  entirely) — not yet wired into `ociman build`'s own CLI at all.
