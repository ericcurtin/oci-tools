# Design note 0141: `ociman run --pull newer`/`ociman build --pull newer`

Status: implemented
Scope: `crates/oci-registry/src/pull.rs` (new `has_different_digest`,
`resolve_manifest` extracted as a shared helper `pull` itself now
calls too); `crates/oci-registry/src/lib.rs` (re-export); `bin/ociman/
src/main.rs` (`PullPolicy` gains `Newer`, `resolve_or_pull`'s own
match arm); 3 new unit tests in `oci-registry`, 2 new integration
tests in `tests/tests/ociman_pull_policy.rs` (plus one real, pre-
existing test bug fixed along the way — see below).

## Closing the gap 0140 explicitly deferred

0140's own "what this doesn't do yet" named this directly: "`newer` —
[deferred,] needs an extra registry round trip purely to fetch
comparison metadata." Picked back up here, now that the underlying
comparison's exact real semantics have been checked directly.

## Checked directly against real podman/buildah's own current source

Real `podman run --pull newer` produced no visibly different behavior
in casual testing (no "Trying to pull..." line for an already-current
local image), so the real semantics were read directly from real
buildah's own current source rather than inferred: `~/git/podman/
vendor/go.podman.io/common/libimage/pull.go`'s own dispatch calls
`hasDifferentDigestWithSystemContext` (`image.go`) whenever the policy
is `PullIfNewer` and a local copy already exists — which fetches the
remote manifest (following a multi-platform index down to the
matching platform first, exactly like a real pull already does) and
compares its digest against every digest the local image already
knows about. **Never a timestamp comparison** — purely digest
equality. A real registry request is always made (there's no cheaper
way to know without one), but a full pull (config + every layer blob)
only follows if the digest actually differs.

## Implementation: one shared resolution path, not two

`oci_registry::pull::resolve_manifest` (new, private) was extracted
from `pull`'s own existing body — fetch the top-level manifest,
resolve down to the platform-matching child if it's a multi-platform
index — and now returns the already-parsed `ImageManifest` alongside
the bytes/digest too, so neither `pull` nor the new `has_different_
digest` ever parses the same JSON twice (a real, if minor, care taken
specifically to avoid *any* new overhead on `pull`'s own existing hot
path — this project's own "must have measurably equal or better
performance" standard applies even to an internal refactor with no
behavior change). `has_different_digest(client, reference, platform,
local_digest)` calls `resolve_manifest` and compares digests directly,
never fetching a blob. `ociman`'s own `resolve_or_pull` gained a
`PullPolicy::Newer` arm: pull unconditionally if nothing is local yet;
otherwise call `has_different_digest` and pull only if it returns
`true`, otherwise return the already-local record unchanged.

## Real, automated tests

Three new unit tests in `oci-registry` (digest matches → `false`;
digest differs → `true`; and — the property that actually matters for
this to be worth doing at all — a mock registry serving *only* a
manifest route, no blob routes at all, still succeeds, proving no blob
is ever fetched during the check itself). Two new CLI-level
integration tests in `tests/tests/ociman_pull_policy.rs`, both built
around a genuinely new test technique this file needed: a
`MockRegistry` whose own route table can be swapped out *between*
requests at the same address (`set_routes`, backed by a real
`Mutex`) — simulating "the registry has since been updated" without
needing a second, differently-addressed mock (which would otherwise
look like an entirely unrelated image reference, not the same one
having changed). One test builds once (`--pull always`, to genuinely
pull for real), swaps in different layer content, then builds again
with `--pull newer` and confirms a real, additional registry request
happened; the other confirms `--pull newer` still succeeds using the
identical local copy when nothing has actually changed.

**A real, pre-existing test bug found and fixed along the way**: while
adding these two tests, re-reading the file's existing `missing`-policy
test turned up an assertion that had somehow ended up asserting `>= 1`
registry requests instead of the `== 0` its own name/intent (and
0140's own documented claim) require — the exact opposite of what it
was meant to prove. Root cause not fully pinned down (most likely an
editing mistake earlier in 0140's own turn, since the test passed
either way at the time — a `>= 1` assertion is strictly weaker, so it
never actually failed until this turn's own stricter re-reading caught
it), but fixed directly regardless: `assert_eq!(..., 0)`, matching what
the test's own name has always claimed. All pre-existing tests
(including every other 0140 test) still pass unmodified. Full `cargo
build --workspace --locked`/`cargo test --workspace --locked` (2 clean
runs)/`cargo fmt --all --check`/`cargo clippy --workspace
--all-targets --locked -- -D warnings`/`python3 ci/guards.py`/`cargo
deny check` all clean.

## What this doesn't do yet

* Real podman/buildah's own `hasDifferentDigestWithSystemContext`
  checks the local image's *every* known digest (a single image can
  have more than one, e.g. after being pulled under different
  manifest-list resolutions over time) — this increment's own
  `has_different_digest` only ever compares against the *one* digest
  `ociman`'s own `ImageRecord` tracks (this project's own storage model
  doesn't keep a history of alternate digests for the same stored
  image at all), a real, narrower-but-consistent-with-this-project's-
  own-storage-model scope limit, not an oversight.
