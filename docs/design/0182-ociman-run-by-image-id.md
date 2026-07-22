# Design note 0182: `ociman run`/`ociman create` by image ID

Status: implemented
Scope: `bin/ociman/src/main.rs` (`resolve_image_by_reference_or_id`
refactored to share its own ID-matching logic via a new
`resolve_image_by_id_only`; `prepare_container`'s own image resolution
now tries ID first); `tests/tests/ociman_run.rs`,
`tests/tests/ociman_create.rs`.

## Closing a gap named three separate times without ever being fixed

0179, 0180, and 0181 each independently flagged the exact same real
inconsistency in their own "what this doesn't do yet" sections:
`ociman inspect`/`rmi`/`tag`/`push`/`save` (0122) all resolve an image
by its own real or short ID, but `ociman run`/`ociman create` never
did — confirmed directly each time by testing a *tagged* image's own
short ID identically failing, ruling out any connection to the
untagged-image work those three increments were actually about. This
increment closes it.

## Why ID resolution has to run *before* tag resolution here — the
opposite ordering from 0122's own precedent, and why that's still
consistent

0122's own `resolve_image_by_reference_or_id` tries a tag reference
first, falling back to ID only if that fails — completely safe there,
since `inspect`/`rmi`/`tag`/`push`/`save` never touch the network
either way, so the order costs nothing.

`ociman run`/`ociman create` are different: they carry a real pull
policy (`--pull never/missing/always/newer`) that decides whether to
consult a registry at all. A real image ID (e.g. `e35b5c7e4a54`)
almost always *also* parses successfully as some syntactically valid
but nonsense tag reference — checked directly by tracing through
`Reference::parse`'s own grammar: a bare hex string with no `/` just
becomes repository `docker.io/library/e35b5c7e4a54`, tag `latest`.
Trying that as a tag *first* would mean `--pull missing`/`--pull
always` dutifully attempting a real, wasted network round trip against
that nonsense reference before ever falling back to ID resolution —
a real, measurable, entirely avoidable cost this project's own
performance goals argue against paying on every single "run by ID"
invocation.

So `prepare_container` tries ID resolution *first*, falling back to
the existing tag-and-pull-policy path only if that returns `None`.
This costs nothing extra for the overwhelmingly common "run a real
tag" case: `resolve_image_by_id_only`'s own hex-prefix filter rejects
a real tag string (which almost never happens to be all-hex) in a
single cheap string scan, with no store access at all, before ever
reaching the existing path. Verified directly: `ociman run --pull
always <short-id>` against a real, offline test image completes
immediately rather than hanging/failing on a real network attempt.

## A small refactor, not new logic

`resolve_image_by_reference_or_id`'s own ID-matching body (the hex-
prefix filter, the by-digest dedup with 0179's own "a real tag always
wins over the sentinel" tie-break, and the "ambiguous prefix" error)
was extracted verbatim into a new `resolve_image_by_id_only`, with
`resolve_image_by_reference_or_id` itself now just calling it as its
own fallback — behavior-preserving (confirmed: every existing
`inspect`/`rmi`/`tag`/`push`/`save` test still passes unchanged).
`prepare_container` calls the exact same extracted function directly,
in the opposite order, for the reason above.

## `ANNOTATION_IMAGE` now records the resolved record's own actual
reference, not necessarily what the user typed

Previously: `annotations.insert(ANNOTATION_IMAGE, reference.to_string())`
using the *parsed input* reference. Now: `record.reference.clone()` —
the record's own real reference, always identical to the input for a
tag resolution (both are the same normalized string `store.resolve_
image` is keyed by), but correctly capturing whichever real tag (or
0179's own untagged sentinel) an ID-resolved image actually has. This
keeps `commit`/`rmi --force`'s own existing "does any container's
`ANNOTATION_IMAGE` reference this exact image" lookups working
correctly regardless of which form started the container — verified
directly: a container started by ID from an image that's *also* been
given a real tag correctly records that real tag, not the raw ID
string typed at the command line, matching 0179's own already-
established "a real tag always wins over the sentinel" tie-break.

## Tests

`tests/tests/ociman_run.rs` gained 4 integration tests: running by
both the short (12 hex char) and full `sha256:<hex>` ID forms; `--pull
always` against a real ID never touching the network (verified by
succeeding immediately in this fully offline test environment, which
would otherwise hang/fail on a real registry attempt); an unknown
image ID being a clear error, same as an unknown tag; and the started
container's own `ANNOTATION_IMAGE` correctly recording the image's
real tag, not the bare ID it was actually started with.
`tests/tests/ociman_create.rs` gained one test confirming `ociman
create` (which shares `prepare_container` entirely) needed no separate
fix at all. Full `cargo build --workspace --locked`/`cargo test
--workspace --locked` (2 clean runs)/`cargo fmt --all --check`/`cargo
clippy --workspace --all-targets --locked -- -D warnings`/`python3
ci/guards.py`/`cargo deny check`/`bash ci/native-ci.sh` all clean.

## What this doesn't do yet

Nothing new — this closes the exact gap 0179/0180/0181 each already
named; no further "what this doesn't do yet" items were introduced.
