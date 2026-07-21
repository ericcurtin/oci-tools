# Design note 0132: `ociman build --ignorefile`

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Build` gains `--ignorefile`);
`bin/ociman/src/build.rs` (`cmd_build`/`read_ignore_patterns` gain an
`ignorefile: Option<&Path>` parameter); `tests/tests/ociman_build.rs`
(2 new tests).

## Closing the gap 0131 explicitly flagged

0131's own "what this doesn't do yet" section named this directly:
"No `--ignorefile`/equivalent explicit-path override flag (real
`podman build --ignorefile <path>` skips this whole resolution
entirely) — not yet wired into `ociman build`'s own CLI at all."
Picked back up here.

## Matches real `podman build --ignorefile` exactly, checked directly

`podman build --help` (real, installed binary): `--ignorefile
string` — "path to an alternate .dockerignore file". Real buildah's
own `ContainerIgnoreFile` (`~/git/podman/vendor/go.podman.io/buildah/
pkg/parse/parse.go`) short-circuits its own `.containerignore`/
`.dockerignore` search entirely whenever an explicit path is given:
`if path != "" { excludes, err := imagebuilder.ParseIgnore(path);
return excludes, path, err }` — no fallback to any other name or
location, and any read error (including "does not exist") propagates
as a real build failure. Confirmed directly with two real `podman
build` runs: `--ignorefile /does/not/exist` fails outright (not
silently proceeding as if no ignore file existed at all); `--ignorefile
<arbitrary path, arbitrary name>` reads and applies it correctly, with
no `.dockerignore`/`.containerignore` at the context root involved at
all.

`read_ignore_patterns` (`bin/ociman/src/build.rs`) now takes this
exact short-circuit shape: an `Some(path)` `ignorefile` reads `path`
directly and returns (any I/O error propagating via the same
`.with_context` a missing `.dockerignore`/`.containerignore` already
used); `None` falls through to 0131's own `.containerignore`-then-
`.dockerignore` context-root search, unchanged.

## Real, automated tests

Two new CLI-level integration tests in `tests/tests/ociman_build.rs`:
`--ignorefile` reading an arbitrarily-named file at an arbitrary path
outside the context entirely (proving the context-root search is
truly bypassed, not merely renamed); and `--ignorefile` pointing at a
nonexistent path failing the build outright. All pre-existing tests
(0130/0131's own 10 `.dockerignore`/`.containerignore` tests and the
full 53-test base `ociman_build.rs` suite) still pass unmodified. Full
`cargo build --workspace --locked`/`cargo test --workspace --locked`
(2 clean runs)/`cargo fmt --all --check`/`cargo clippy --workspace
--all-targets --locked -- -D warnings`/`python3 ci/guards.py`/`cargo
deny check` all clean.

## What this doesn't do yet

* The per-Containerfile-named ignore file case 0131 already deferred
  (`<dockerfile>.containerignore`/`.dockerignore`) — still deliberately
  out of scope, for the same reason (a real, internally-inconsistent
  upstream precedence rule, not worth replicating without its own
  justification).
