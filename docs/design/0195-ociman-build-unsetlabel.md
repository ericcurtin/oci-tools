# Design note 0195: `ociman build --unsetlabel`

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Build`'s new `--unsetlabel`
flag); `bin/ociman/src/build.rs` (`target_base_labels` capture,
`--unsetlabel` application); `tests/tests/ociman_build.rs`.

## Continuing milestone 4

0194's own "what this doesn't do yet" named `--unsetlabel` (real
podman's own separate flag, distinct from `--unsetenv`) as a small,
natural next companion. Checked real `docker build --help`/`podman
build --help` directly first: podman's own help text phrases it
distinctly from `--unsetenv` — "unset label **when inheriting labels
from base image**" — a real, deliberate hint this isn't simply
`--unsetenv`'s own logic ported over to labels.

## Real, checked-directly semantics — genuinely different from
`--unsetenv`

Verified directly, three separate real scenarios against an actually-
installed `podman build --unsetlabel`, each isolated with a fresh
base image to avoid any cross-contamination between scenarios:

1. A label the *base image* declares (`LABEL inherited=frombase` in a
   separate base build) — `--unsetlabel inherited` removes it.
2. A label only ever declared by *this* Containerfile's own `LABEL`
   instruction (never present in the base at all) — `--unsetlabel`
   naming it leaves it **completely untouched**, unlike `--unsetenv`,
   which removes an analogous env var regardless of origin.
3. A label the base declares, which a *later* `LABEL` in this same
   Containerfile also redeclares with a different value — `--unsetlabel`
   naming it **still removes it entirely**, even the redeclared value;
   the redeclaration does not save it.

So the real rule: `--unsetlabel <key>` removes `<key>` from the final
image if and only if the *target stage's own base image* declared that
key at all, independent of whatever this stage's own `LABEL`
instructions later do with the same key. Confirmed via real podman
that it adds no history entry of its own either, matching `--unsetenv`.

## The fix

* `Command::Build` gains `--unsetlabel <KEY>` (repeatable, bare keys).
* A new `target_base_labels: BTreeMap<String, String>`, captured
  inside the existing stage loop right when `is_target` is true and
  the target's own base config has just been resolved (before that
  stage's own `LABEL`/other instructions run) — the one real base
  snapshot the later filtering step needs, since `base_config` itself
  is moved into `build_stage` immediately afterward.
* After the whole build finishes, for each `--unsetlabel` name present
  in `target_base_labels`, removed from the *final* `config.config.
  labels` — regardless of whatever value ended up there after this
  stage's own instructions ran. A name never in `target_base_labels`
  is left completely alone.

## Tests

Four new integration tests in `tests/tests/ociman_build.rs`, each
isolating one of the three real scenarios above plus the no-history-
entry proof, using `seed_image`'s own `ContainerConfig.labels` field to
represent "the base image already declares this label" without
needing a nested two-step build. All 99 pre-existing `ociman build`
tests continue to pass unchanged. Full `cargo build --workspace
--locked`/`cargo test --workspace --locked` (2 clean runs, 83/83
result blocks)/`cargo fmt --all --check`/`cargo clippy --workspace
--all-targets --locked -- -D warnings`/`python3 ci/guards.py`/`cargo
deny check`/`bash ci/native-ci.sh` all clean. No performance
regression (`ociman build --no-cache`, one `RUN` step, ~24.8ms,
consistent with prior measurements for the same scenario).

## What this doesn't do yet

Nothing new — this closes 0194's own last-named small companion gap.
Real `until=`/`dangling=`-style prune filters, `--timestamp`, `-q`/
`--quiet`, and the larger `RUN --mount=`/heredoc/multi-platform gaps
named by the earlier milestone-4 survey remain open, each its own
well-scoped future increment.
