# Design note 0281: `ociman ps --filter ancestor=`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_ps.rs`.

## Continuing the `ps --filter` family

`0272`/`0273`/`0275`/`0280` implemented `status=`/`id=`/`name=`/
`label=`/`before=`/`since=`. `ancestor=` (filter by the image a
container was created from) is the next real, well-scoped candidate.

## A real installed-vs-cloned-source version mismatch, caught by direct verification

Reading `~/git/podman`'s own vendored `pkg/domain/filters/
containers.go` first suggested `ancestor=` should substring-match the
container's own image *ID* too (`strings.Contains(rootfsImageID,
filterValue)`), matching a short hex prefix the same way `id=` does.
Direct testing against the real *installed* `podman` (version 4.9.3)
found otherwise: a 12-hex-char short image ID prefix **never**
matched, while the exact, full 64-hex-char ID did. Checking why:
`~/git/podman`'s own clone is `v5.4.0-rc1` — a materially newer major
version than the installed `4.9.3` — so the two genuinely have
different `ancestor=` implementations, and the installed behavior
(what any real user of this project's own `podman`-comparison would
actually see) is what matters, not the newer cloned source. This
project's own "checked directly against a real installed X" principle
paid off concretely here: relying on the source alone would have
shipped incorrect prefix-matching behavior.

## Scope: name/tag substring only, this turn

Given that version-dependent uncertainty, and that real docker/
podman's own documented `ancestor=` contract is actually broader still
("containers created from an image **or a descendant**" — full image-
lineage tracing, which this project's own content-addressed store has
no direct "parent image" graph for at all), this note deliberately
scopes down to the overwhelmingly common real case: does the
container's own recorded image reference match `<image>` by name/tag,
checked directly and confirmed against the installed `podman`:

- A substring match against the container's own full image reference
  (e.g. `docker.io/library/busybox:latest`).
- A bare, tagless value (e.g. `busybox`) also matches a `:latest`-
  tagged reference — checked directly (`ancestor=busybox` matched a
  real `busybox:latest` container; `ancestor=busybox:v1`, a real but
  wrong tag, did not).
- Multiple `ancestor=` values are OR'd together (same convention every
  other multi-value `ps --filter` key here already uses).
- Like `id=`/`name=`/`label=`/`before=`/`since=` and unlike `status=`,
  giving `ancestor=` does not override the default running-only/
  `--all` visibility rule on its own.

An exact full-manifest-digest match, and real docker/podman's own
broader image-lineage ("or a descendant") semantics, are both real,
deliberately deferred candidates — see "Still ahead".

## Verified

Integration (`tests/tests/ociman_ps.rs`, one new test):

- A full image reference matches.
- A bare, tagless value matches a real `:latest`-tagged container.
- A wrong tag doesn't match.
- `ancestor=` alone (no `-a`) doesn't override the default running-
  only visibility rule.

Regression: all 18 pre-existing `ociman_ps.rs` tests still pass (one,
`ps_filter_with_an_unrecognized_key_or_value_is_a_clear_error`,
updated to use a still-genuinely-unsupported key, `pod=`, since its
previous example, `ancestor=`, is now supported).

Full workspace: `cargo build`/`test --workspace`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`python3 ci/guards.py`, `cargo deny check`, `ci/native-ci.sh`,
`ci/build-deb.sh`.

## Still ahead

An exact full-manifest-digest `ancestor=` match (needs the image
store, not just the container's own recorded reference annotation);
real docker/podman's own broader "or a descendant" image-lineage
semantics (needs a real parent-image graph this project's own store
doesn't track); and the rest of real podman's own remaining `ps
--filter` family (`pod`, `network`, `restart-policy`, `command`,
`annotation`/`annotation!`, `exited`, `health`, `volume`, `until`)
all remain further, separately-scoped candidates.
</content>
</invoke>
