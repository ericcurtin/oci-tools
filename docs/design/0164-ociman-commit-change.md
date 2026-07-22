# Design note 0164: `ociman commit --change`

Status: implemented
Scope: `crates/oci-dockerfile/src/lib.rs` (new `parse_change`, 2 new
unit tests); `bin/ociman/src/build.rs` (`args_for`/`format_pairs`/
`resolve_workdir` bumped from private to `pub(crate)` so `main.rs` can
reuse them — no behavior change of their own); `bin/ociman/src/main.rs`
(`Command::Commit`'s new `change` field; `cmd_commit`/`commit_inner`
updated; new `apply_change_instruction`); `tests/tests/
ociman_commit.rs` (3 new integration tests).

## Closing 0155's own deferred gap

0155's own "what this doesn't do yet" named `--change` directly (a
real, valid `podman commit --change`/`podman import --change`
combination: apply Dockerfile-instruction-style config overrides as
part of the same commit). This increment closes it.

## The exact real, checked-directly-allowed instruction list

`~/git/podman/cmd/podman/common/completion.go`'s own `ChangeCmds`:
`CMD`, `ENTRYPOINT`, `ENV`, `EXPOSE`, `LABEL`, `ONBUILD`, `STOPSIGNAL`,
`USER`, `VOLUME`, `WORKDIR` — every one of these already has a real,
working parser (`oci_dockerfile::Instruction`) and a real, working
"apply this to an `ImageConfig`" implementation (`ociman build`'s own
`apply_instruction`, `bin/ociman/src/build.rs`) already established
for actual Dockerfile builds. Anything else (`RUN`/`COPY`/`ADD`/
`FROM`/`ARG`/`SHELL`/`HEALTHCHECK`/`MAINTAINER` — anything that only
makes sense as part of an actual, multi-step *build*) is a real, clear,
immediate error, not silently ignored or misapplied.

## Reusing real, existing infrastructure end to end — no new parsing/application logic duplicated

* **Parsing**: a new `oci_dockerfile::parse_change(text: &str) ->
  Result<Instruction, String>` parses one standalone instruction line
  — the exact same grammar `parse` (a whole Dockerfile) already uses
  for each of its own lines, just applied to one line with no
  surrounding file (no line-continuation splicing/parser-directive
  scanning needed for a single, already-complete line).
* **Application**: `args_for`/`format_pairs`/`resolve_workdir` (`ociman
  build`'s own existing, already-tested helpers for shell-form
  wrapping, `key=value` formatting, and relative/absolute `WORKDIR`
  resolution) were bumped from private to `pub(crate)` so a new
  `apply_change_instruction` in `main.rs` can call the *exact same*
  logic `ociman build` already uses for the identical instruction —
  the two can never silently drift apart on what, say, a relative
  `WORKDIR` or a shell-form `CMD` actually resolves to, because they
  share the real computation, not just the general shape of it.

## One real, deliberate difference from `ociman build`'s own identical instructions: no extra history entry

`ociman build`'s own `apply_instruction` calls `oci_dockerfile::
record_empty_history` after each of these same instructions, since
`ociman build`'s own history is a real, step-by-step provenance record
of the whole build. A `commit` isn't a multi-step build at all — real
buildah's own `Commit` applies `--change` as plain `ImportBuilder`
config setters, never a build step of its own (checked directly,
`~/git/podman/libpod/container_commit.go`) — so `apply_change_
instruction` deliberately never calls `record_empty_history`: the
*only* new history entry a `--change`d commit ever gets is the one
real diff layer's own (already added by `record_layer`, before `--change`
is ever applied), exactly like a commit with no `--change` at all.
Verified directly, not just reasoned about (see tests below).

## `--change` is parsed and validated before anything else

Every `--change` value is parsed (and, for an unsupported instruction,
rejected) in `cmd_commit` itself, before ever resolving the container
or pausing anything — a bad `--change` value fails fast, with no
pointless freeze/thaw or wasted diff/layer-commit work first, matching
this project's own established "fail fast on bad input" convention
elsewhere (e.g. `ociman build`'s own upfront Containerfile parse before
any real build work starts).

## Real, automated tests

Two new unit tests for `oci_dockerfile::parse_change` itself (parses a
real instruction line correctly; rejects an invalid one the same way a
whole file's own parse would, with the same error text). Three new
integration tests in `tests/tests/ociman_commit.rs`:
`commit_change_applies_every_real_supported_instruction_and_adds_no_
extra_history` (all 10 instructions in one commit, each field verified
against the resulting image's own real `inspect --json` output, plus
confirming exactly one new history entry, not eleven); `commit_change_
rejects_a_build_only_instruction` (`RUN`/`COPY`/`ADD`/`FROM`/`ARG`, each
a real, clear error); `commit_change_rejects_an_unparseable_
instruction`.

Full `cargo build --workspace --locked`/`cargo test --workspace
--locked` (2 clean runs, plus the full existing `ociman build` test
suite — 73 tests — re-run to confirm the `args_for`/`format_pairs`/
`resolve_workdir` visibility bump changed nothing observable there)/
`cargo fmt --all --check`/`cargo clippy --workspace --all-targets
--locked -- -D warnings`/`python3 ci/guards.py`/`cargo deny check`/
`bash ci/native-ci.sh` all clean.

## What this doesn't do yet

Everything 0155/0156 already named beyond `--change` itself remains
unchanged: `--config` (merge an arbitrary container-config JSON file),
`--squash`, `--include-volumes`, and `image` as an optional (untagged)
argument.
