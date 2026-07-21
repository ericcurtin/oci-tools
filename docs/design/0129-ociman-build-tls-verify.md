# Design note 0129: `ociman build --tls-verify`

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Build` gains `--tls-verify`,
`cmd_run`'s own `--tls-verify` wiring left as-is from 0128);
`bin/ociman/src/build.rs` (`cmd_build`, `build_stage`,
`apply_instruction`, `copy_instruction`, `external_image_source_root`
all gain a threaded `tls_verify: bool` parameter down to their own
`resolve_or_pull` call sites); `tests/tests/ociman_tls_verify.rs` (4
new tests).

## Closing the gap 0128 explicitly flagged

0128's own "what this doesn't do yet" section named this directly:
"`ociman build`'s own `FROM`/`COPY --from=<external-image>` pulls
(`resolve_or_pull`) still always assume HTTPS... wiring the same flag
into `build` is a small, separate, well-scoped future increment."
Picked back up here.

## Two separate pull call sites inside `build.rs`, both threaded

`resolve_or_pull` (shared helper in `main.rs`, already `tls_verify`-
aware since 0128) is called from two independent places inside
`build.rs`:

* The `FROM <external-image>` case in `cmd_build`'s own per-stage
  setup — resolves (or pulls) the stage's base image before any of its
  instructions run.
* `external_image_source_root` — `COPY --from=<external-image>`'s own
  source-image resolution (as opposed to `COPY --from=<earlier-stage>`,
  which never touches a registry at all; `stage_ctx.rootfs_for` is
  checked first and short-circuits this path entirely for that case).

Both needed `tls_verify: bool` threaded all the way from `cmd_build`'s
own new parameter down through `build_stage` → `apply_instruction` →
`copy_instruction` → `external_image_source_root`'s own call — one
`bool` added at each level, following each function's existing
parameter-threading style (`context`, `stage_ctx`, `cache_candidates`
already thread the same way).

## Matches 0128's own `--tls-verify` CLI idiom exactly

`Command::Build` gained the identical clap flag shape 0128 already
established for `pull`/`push`/`run` (`num_args = 0..=1` +
`default_missing_value = "true"` + `ArgAction::Set`) — `--tls-verify`,
`--tls-verify=false`, `--tls-verify false` all work identically across
every `ociman` subcommand that ever talks to a registry, matching real
podman's own per-subcommand `--tls-verify` flag consistency.

## Real, automated tests

Four new CLI-level integration tests added to `tests/tests/
ociman_tls_verify.rs` (same file 0128 established), extending its
existing mock-HTTP-registry pattern:

* `build_from_with_tls_verify_false_pulls_the_base_image_over_plain_http`
  / its `_without_` counterpart — a metadata-only Containerfile (no
  `RUN`/`COPY`, just `LABEL`) referencing the mock registry directly in
  `FROM`, proving the flag reaches the `FROM` pull path. Metadata-only
  deliberately: no rootfs is ever extracted for a stage with no `RUN`/
  `COPY` of its own, so the mock's placeholder (non-tar) layer bytes
  don't need to be a real archive here, matching `start_mock_with_a_
  real_image`'s existing shape used by the `pull`/`push` tests.
* `build_copy_from_with_tls_verify_false_pulls_and_extracts_over_plain_http`
  / its `_without_` counterpart — a `COPY --from=<mock-registry-image>`
  from a *different*, new mock (`start_mock_with_a_real_extractable_
  image`, new in this increment) whose one layer is a genuine tar+gzip
  archive (built with the same `tar`/`flate2` crates `oci-tools-tests`'
  own `seed_image_with_files_and_compression` already uses), since this
  path *does* extract the pulled layer into a real rootfs cache
  directory (`ensure_cached`) before the `COPY` can read from it — a
  placeholder blob would fail to untar and mask what's actually being
  tested.

All pre-existing tests (including the 53-test `ociman_build.rs` suite
and the 4 pre-existing `ociman_tls_verify.rs` tests) still pass
unmodified. Full `cargo build --workspace --locked`/`cargo test
--workspace --locked` (2 clean runs)/`cargo fmt --all --check`/`cargo
clippy --workspace --all-targets --locked -- -D warnings`/`python3
ci/guards.py`/`cargo deny check` all clean.

## What this doesn't do yet

* No `--cert-dir` for `build`, same as 0128's own pull/push scope —
  plain HTTP or default HTTPS only, no custom private-CA support.
* `ARG`-expanded registry hosts in `FROM`/`COPY --from=` (e.g. `FROM
  ${REGISTRY}/image:tag`) are unaffected by this change either way —
  `tls_verify` is a single build-wide flag, not resolved per stage or
  per `ARG` value; a Containerfile mixing a plain-HTTP internal
  registry with a real HTTPS public one in the same build still isn't
  supported (same limitation `pull`/`push`'s single-registry-per-
  invocation shape already has).
