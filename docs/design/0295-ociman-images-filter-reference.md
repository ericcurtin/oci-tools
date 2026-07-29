# Design note 0295: `ociman images --filter reference=`/`reference!=`

Status: implemented
Scope: `crates/oci-spec-types/src/glob.rs` (moved from
`crates/oci-dockerfile/src/glob.rs`), `crates/oci-spec-types/src/lib.rs`,
`crates/oci-dockerfile/src/lib.rs`, `crates/oci-dockerfile/src/dockerignore.rs`,
`bin/ociman/src/main.rs`, `tests/tests/ociman_images.rs`.

## Closing `0293`'s own "still ahead"

Real `podman images --filter reference=<pattern>` (checked directly
against `~/git/container-libs/common/libimage/filters.go`'s own
`imageMatchesReferenceFilter`) is a real shell-glob match — Go's own
`path.Match` — against several normalized forms of each image's own
reference, plus a real shortcut: if `<pattern>` itself resolves
directly (by tag or ID) to a real stored image, that's an immediate
match regardless of glob syntax.

## Reusing an already-verified matcher instead of writing a new one

This project already has a complete, Go-toolchain-verified
`path/filepath.Match` port: `oci_dockerfile::glob` (built for
Dockerfile `COPY`/`ADD` wildcards). Rather than writing (and
separately re-verifying) a second copy for this genuinely unrelated
purpose, `glob.rs` moved into `oci-spec-types` — the same "move a
shared primitive out of one crate's own private code into a crate
every real caller already depends on, the moment a second, unrelated
one needs it" move this project's own `resolve_by_reference_or_id`
(`oci_store`, `0122`/`0213`) and `time` (this same crate) already went
through. `oci-dockerfile` re-exports the identical public names from
their new home so no other existing caller needed to change at all.
Verified a real, zero-behavior-change move: the module's own complete
test suite (including Go's official `matchTests` table) moved
unmodified and still passes byte-for-byte identically, and
`oci-dockerfile`'s own 150 pre-existing tests (many exercising
`.dockerignore` matching, which uses this same matcher internally)
pass unmodified too.

## Building the real candidate set

Ported `imageMatchesReferenceFilter`'s own candidate-building loop
directly, using this project's own already-existing `Reference` type
(`registry()`/`repository()`/`tag()`/`Display`) rather than needing a
new reference-splitting primitive: for
`docker.io/library/busybox:latest`, the six real candidates checked
are the full reference, the repository path without domain or tag
(`library/busybox`), the bare name with tag (`busybox:latest`), the
full reference without tag (`docker.io/library/busybox`), the
repository path with tag (`library/busybox:latest`), and the bare name
without tag (`busybox`) — `value` matches if any candidate does.

## The exact-identity shortcut, and a real consequence of this project's own per-tag listing

Real podman's own shortcut compares by real image ID (content
digest), not by name. Combined with this project's own already-
established `ociman images` convention (one row per *tag*, not
deduplicated by digest, `0263`), this produces a real, faithful — not
buggy — consequence: three separately-named tags that happen to share
the exact same underlying image content (e.g., re-tagging the same
image three times with no rebuild) all "match" a `reference=` filter
naming any *one* of their shared tags, since real podman's own
algorithm is fundamentally about image identity, not name identity,
and would show the identical single row as a match for the same
reason (it just wouldn't have three separate rows to distinguish in
the first place). Found by hand while testing this at first with three
merely-retagged images and getting a "surprising" all-three-match
result — re-verified with genuinely distinct images (via `ociman
build`, so each has its own real digest) to confirm the glob-matching
logic itself is correct, then documented this real interaction rather
than "fixing" it away from real podman's own actual behavior.

## Combination rule: a real, checked-directly exception

`filterReferences`'s own comment states it plainly: "reference filters
is a special case as it does an OR for positive matches and an AND
logic for negative matches" — genuinely different from `before=`/
`since=`'s own generic per-key-AND rule (`0293`). Implemented exactly:
multiple `reference=` values are OR'd (any one suffices); any
`reference!=` match excludes, regardless of how many are given.

## Verified

Integration (`tests/tests/ociman_images.rs`, one new test, one
pre-existing test's own "unrecognized filter" example changed since
`reference=` is now real):

- A glob (`*myimage*`) matches only the one image whose name contains
  it, using genuinely distinct (different-digest) images so the glob
  logic itself is unambiguously exercised.
- A bare-name candidate match (`reference-filter-base`, no wildcards
  at all) matches via the "name without tag" candidate.
- `reference!=` excludes the matching image, keeps the rest.
- Multiple `reference=` values are OR'd.
- The exact-resolve shortcut matches a fully-qualified literal
  reference outright.
- A pattern matching nothing is a real, silent empty result (`no
  images`), never an error — `reference=` never requires resolving
  anything, unlike `before=`/`since=`.

Full workspace: `cargo build`/`test --workspace` (111 test binaries),
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets --
-D warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh`.

## Still ahead

Real podman's own remaining `images --filter` keys (`readonly=`,
`intermediate=`, `containers=`) remain further, separately-scoped
candidates.
