# Design note 0135: `ociman build --label`

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Build` gains `--label`);
`bin/ociman/src/build.rs` (`cmd_build` gains a `labels: &[String]`
parameter, new `parse_labels` helper); `tests/tests/ociman_build.rs`
(2 new tests).

## Why this, now

Continuing 0134's own survey of real `podman build`'s much larger flag
surface for the next well-scoped gap: `--label` is a small, common,
low-risk real-world flag (setting image metadata from the calling
script/CI pipeline without needing a templated Containerfile) with no
interaction with any of the still-out-of-scope, riskier flags
(`--squash`, `--platform`, `--secret`, etc.).

## Checked directly against real `podman build --label` first — a real, non-obvious finding

Ran a real, installed `podman build --label foo=from-cli --label
baz=only-from-cli` against a Containerfile with its own `LABEL
foo=from-dockerfile`/`LABEL bar=only-in-dockerfile`, and watched its
own step-by-step output closely: `--label` isn't applied as a simple
pre/post metadata patch — it shows up as its **own extra, real build
step** (`STEP 4/4: LABEL "foo"="from-cli" "baz"="only-from-cli"`),
appended *after* every real Containerfile instruction. This means:

* `--label` **overrides** a same-key `LABEL` already in the
  Containerfile (confirmed: the final `foo` was `from-cli`, not
  `from-dockerfile`) — not the other way around, since it's applied
  strictly last.
* A bare `--label bareword` (no `=` at all) becomes `bareword=""` — a
  real, tolerant parse (confirmed directly), never a CLI parse error.
* It's genuinely its own step in the build's own history (visible in
  `ociman history`, mirroring real podman/buildah's own extra `LABEL`
  step), not folded silently into whatever the last real instruction
  already recorded.

`ociman build`'s own implementation mirrors this exactly: `cmd_build`
applies `--label`'s own parsed pairs to the *target* stage's already-
fully-built `ImageConfig` (after the entire real build loop finishes,
overriding any same-key label the Containerfile itself set), and — only
when at least one `--label` was actually given, avoiding a spurious
empty history entry otherwise — records one synthetic `LABEL k=v ...`
history entry via the same `oci_dockerfile::record_empty_history` a
real Containerfile `LABEL` instruction already uses, formatted with
this codebase's own already-established `format_pairs` helper (`k=v`
space-separated, not real buildah's own quoted `"k"="v"` wire format —
an internal, already-consistent convention this project uses for every
metadata-only instruction's own history text, not a claim of byte-for-
byte compatibility with real buildah's own internal string).

`parse_labels` (new) is `parse_build_args`'s own sibling, deliberately
narrower in one specific way: a bare `KEY` (no `=`) never falls back to
this process's own environment the way `--build-arg`'s bare-`KEY` form
does — real `podman build --label` doesn't either (confirmed directly:
it becomes an empty-string value, not an environment lookup). Later
duplicate keys overwrite an earlier one's own value *in place*
(keeping its original position in the resulting ordered list), rather
than moving it to the end — a deliberate, reasonable choice for this
project's own synthetic history text's own ordering, not itself
dictated by anything real podman's own single-map internal
representation would even distinguish.

## Real, automated tests

Four new unit tests for `parse_labels` (verbatim `KEY=VALUE`, bare-key-
means-empty-value, later-duplicate-overwrites-in-place, empty input)
plus two new CLI-level integration tests in `tests/tests/
ociman_build.rs`: `--label` overriding a same-key Containerfile
`LABEL`, adding a brand new key, and parsing a bare key as an empty
value, all verified against the built image's own real, stored config;
and a dedicated test confirming *no* `--label` at all adds no extra,
spurious history entry (exactly the one real Containerfile `LABEL`
entry, nothing more). All pre-existing tests (the full 69-test base
`ociman_build.rs` suite, including 0130-0134's own dockerignore/
containerignore/ignorefile/iidfile tests) still pass unmodified. Full
`cargo build --workspace --locked`/`cargo test --workspace --locked`
(2 clean runs)/`cargo fmt --all --check`/`cargo clippy --workspace
--all-targets --locked -- -D warnings`/`python3 ci/guards.py`/`cargo
deny check` all clean.

## What this doesn't do yet

* `--annotation` (the OCI-manifest-level sibling of `--label`, which
  sets *config*-level metadata) — a separate, similarly small, well-
  scoped future increment.
* `--layer-label` (real podman's own flag for labeling an
  *intermediate* image, not the final one) — out of scope; this
  project's own build executor doesn't currently expose intermediate
  stage results as separately-taggable images at all.
* The much larger remaining `podman build` flag surface unchanged from
  0134's own list (`--platform`, `--squash`, `--secret`, `--ssh`,
  `--cache-from`/`--cache-to`, etc.).
