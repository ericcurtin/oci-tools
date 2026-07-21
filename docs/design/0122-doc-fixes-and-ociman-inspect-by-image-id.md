# Design note 0122: stale doc-comment fixes, and `ociman inspect` resolves by image ID too

Status: implemented
Scope: `crates/oci-spec-types/src/runtime.rs`, `crates/oci-dockerfile/
src/commit.rs`, `crates/oci-dockerfile/src/shell_expand.rs`,
`crates/oci-runtime-core/src/launch.rs`, `bin/ociman/src/build_cache.rs`
(doc comments only, no functional change); `bin/ociman/src/main.rs`
(`resolve_image_by_reference_or_id` new, `cmd_inspect` wired to use it);
`tests/tests/ociman_inspect.rs` (2 new tests).

## Six stale doc comments, found by a targeted survey, fixed

A dedicated survey (grepping for "not yet"/"not implemented"/"still not
here"/etc. across every crate, then cross-checking each hit against
what the codebase actually does today) found six real, confirmed-stale
claims — each describing a gap that a *later* increment already closed
without the original comment ever being updated:

* `oci-spec-types/runtime.rs`: `cpuset.cpus`/`cpuset.mems` claimed "not
  yet translated to a cgroup write" — actually written by `oci_runtime_
  core::cgroups::plan_cpu` since 0056.
* `bin/ociman/src/build_cache.rs`: "`ociman rmi`, not implemented yet
  either" — shipped since 0102.
* `oci-runtime-core/src/systemd_cgroup.rs`: a "known, not-yet-handled
  edge case" (a scope left in `failed` state) — fixed since 0096
  (`reset_failed_unit`, three real call sites in `ociman`'s own
  `cmd_run`/`cmd_stop`/`cmd_rm`).
* `oci-dockerfile/src/commit.rs`: "a future build executor's own job,
  still not implemented" — `ociman build` (`bin/ociman/src/build.rs`)
  has been that executor since 0050.
* `oci-dockerfile/src/shell_expand.rs`: "the engine only, not yet wired
  into any `Instruction`" — wired in by `expand_stage`/`expand_meta_
  args` since 0042.
* `oci-runtime-core/src/launch.rs`: the `create`+`start` two-phase
  lifecycle "needs a persistent background process... and is not
  implemented yet" — implemented (`create`, `crate::exec_fifo`) since
  0017.

Each was checked against the actual current code (not just the design
note claiming it was fixed) before editing — e.g. confirming `plan_cpu`
really does write `cpuset.cpus`/`cpuset.mems`, and that `launch::create`
really exists and is wired into `ocirun create`/`start`. No functional
change; pure documentation accuracy, but a real one: a stale "not
implemented" comment is actively misleading to anyone (human or agent)
reading the code to decide what's safe to build on next.

## `ociman inspect` resolves by image ID, not just by tag

A real, checked-directly parity gap noticed while fixing the above:
`ociman images`' own `DIGEST` column already prints a real, 12-character
short image ID (matching real `docker images`' own `IMAGE ID` column
convention) — but nothing let a user actually *use* that ID with any
other command; only a full tag reference ever resolved. Real `docker
inspect <id>`/`podman inspect <id>` both resolve by a real or short
image ID (a hex prefix of the image's own digest) as well as by tag.

`resolve_image_by_reference_or_id` (new): tries an ordinary tag
reference first (the existing, common-case behavior, completely
unchanged), then, only if that fails, treats the given string as a
possible digest prefix (an optional `sha256:` prefix stripped, hex-
validated) and scans every stored image for a manifest digest starting
with it. Deduplicated by the *real* underlying digest, not by tag
count — checked directly with a dedicated test: two tags pointing at
the exact same image (`ociman tag`) must never make that image's own
ID ambiguous; only two genuinely *different* images sharing a digest
prefix should be (and are refused with a clear "is ambiguous" error,
matching real docker's own behavior for a too-short/colliding prefix,
rather than silently guessing one).

Wired into `cmd_inspect` only, this increment — `ociman rmi`/`ociman
tag` resolving by ID too would need real, extra design care for the
"multiple tags share this exact ID" case specifically for *removal*
(which tag does `rmi <id>` actually remove when more than one points
at it? — real docker's own answer here is more involved than inspect's
own "just show me the image" case, which has no such ambiguity at
all), left for a future, separately-scoped increment rather than
rushed into this one.

## Real, automated tests

Two new `ociman_inspect` tests: a real image resolved by its own short
ID, full hex digest, and `sha256:`-prefixed full digest, all three
working; and the digest-vs-tag-count dedup behavior, proving two tags
to the same image don't trigger a false "ambiguous" error. All 4
pre-existing `ociman inspect` tests still pass unmodified (none of them
exercise the new fallback path, so nothing about the existing tag-based
resolution changed). Full `cargo build --workspace --locked`/`cargo
test --workspace --locked` (2 clean runs)/`cargo fmt --all --check`/
`cargo clippy --workspace --all-targets --locked -- -D warnings` all
clean.
