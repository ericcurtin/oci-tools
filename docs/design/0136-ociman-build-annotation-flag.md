# Design note 0136: `ociman build --annotation`

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Build` gains `--annotation`);
`bin/ociman/src/build.rs` (`cmd_build` gains an `annotations: &[String]`
parameter; `parse_labels` renamed to the more general
`parse_key_value_pairs`, now shared by both `--label` and
`--annotation`); `tests/tests/ociman_build.rs` (2 new tests).

## Closing the gap 0135 explicitly flagged

0135's own "what this doesn't do yet" section named this directly:
"`--annotation` (the OCI-manifest-level sibling of `--label`, which
sets *config*-level metadata) — a separate, similarly small, well-
scoped future increment." Picked back up here.

## Checked directly against real `podman build --annotation` first — confirmed a different target than `--label`

Ran a real, installed `podman build --annotation foo=bar --annotation
bareword`, then inspected the real, pushed manifest's own raw JSON
(via a real local `registry:2` container, the same verification method
this project's own registry-facing increments already established) —
confirmed `--annotation` writes the built **manifest's own top-level
`annotations` field** (`{"annotations": {"foo": "bar", "bareword": "",
...}}`), not `Config.Labels` the way `--label` does. `podman inspect
--format '{{json .Annotations}}'` also confirmed the same thing at the
CLI level. The CLI-level parsing rules themselves — `KEY=VALUE`, bare
`KEY` meaning an empty value (never an environment-variable fallback),
a later duplicate key overwriting an earlier one — are, confirmed
directly, byte-for-byte identical to `--label`'s own.

## Sharing the parser, not duplicating it

Since the parsing rules are genuinely identical, 0135's own
`parse_labels` was renamed to the more general `parse_key_value_pairs`
and is now called from both places `cmd_build` needs it — matching this
project's own "one implementation per function" pillar rather than
carrying two near-identical private helpers. Only what the resulting
pairs get applied *to* differs: `--label`'s own pairs still merge into
`ImageConfig.config.labels` and record a synthetic `LABEL` history
entry (0135's own behavior, unchanged); `--annotation`'s own pairs
become the built `ImageManifest`'s own top-level `annotations`
`BTreeMap` directly (manifest annotations have no history/instruction
concept at all in the real OCI image spec, so there's no equivalent
synthetic history entry to record for this one).

## Real, automated tests

Two new CLI-level integration tests in `tests/tests/ociman_build.rs`:
`--annotation` setting the built manifest's own `annotations` (bare
key included) while leaving a same-named `LABEL` in the Containerfile
itself completely untouched (proving the two flags really do target
different places, not just that `--annotation` works in isolation);
and confirming no `--annotation` at all leaves the manifest's own
`annotations` empty, same as before this flag existed. All pre-existing
tests (the full 71-test base `ociman_build.rs` suite, including 0130-
0135's own dockerignore/containerignore/ignorefile/iidfile/label
tests) still pass unmodified. Full `cargo build --workspace --locked`/
`cargo test --workspace --locked` (2 clean runs)/`cargo fmt --all
--check`/`cargo clippy --workspace --all-targets --locked -- -D
warnings`/`python3 ci/guards.py`/`cargo deny check` all clean.

## What this doesn't do yet

* `--layer-label` (real podman's own flag for labeling an
  *intermediate* image) — same reason 0135 already gave: this
  project's own build executor doesn't expose intermediate stage
  results as separately-taggable images at all.
* The much larger remaining `podman build` flag surface unchanged from
  0134/0135's own lists (`--platform`, `--squash`, `--secret`, `--ssh`,
  `--cache-from`/`--cache-to`, etc.).
