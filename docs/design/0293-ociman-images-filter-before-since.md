# Design note 0293: `ociman images --filter before=`/`since=`/`after=`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_images.rs`.

## A different comparison than `ociman ps --filter before=`/`since=`

`ociman ps --filter before=`/`since=` (`0280`) compares a **container's**
own creation time against another container's. Real `podman images
--filter before=`/`since=`/`after=` is a structurally similar but
genuinely distinct filter: it compares an **image's** own declared
creation time (`ImageConfig.created`) against another *image's*.
Checked directly against `~/git/container-libs/common/libimage/
filters.go`:

```go
case "after", "since":
    img, err := r.time(key, value)
    key = "since"
    filter = filterAfter(img.Created())
case "before":
    img, err := r.time(key, value)
    filter = filterBefore(img.Created())
```

`after`/`since` are real, checked-directly synonyms for the identical
filter (the switch statement literally shares one `case` arm for
both).

## A real, checked-directly distinction from `ps`'s own multi-value rule

`applyFilters`'s own doc comment states the generic rule plainly: "All
filters of each key must apply" — every `--filter before=X` given
appends a *separate* filter function under the `"before"` key, and
*all* of them must match (a real AND, not an OR). Mathematically, `created
< X && created < Y` is equivalent to `created < min(X, Y)` — the
**earliest** reference wins for `before=`. For `since=`/`after=`,
`created > X && created > Y` is equivalent to `created > max(X, Y)` —
the **latest** reference wins.

This is a real, checked-directly *difference* from `ociman ps
--filter before=`/`since=`'s own already-implemented container
version, which (matching real podman's own separate, apparently
unintentional quirk in `pkg/domain/filters/containers.go`, where both
`before` and `since` keep whichever candidate is *earlier*) uses the
earliest reference for *both* keys — verified directly in `0280` and
re-confirmed here by reading the source rather than assumed to
generalize. `images --filter before=`/`since=` implements the
mathematically-correct AND composition instead (earliest for
`before=`, latest for `since=`/`after=`), since `libimage/filters.go`
has no equivalent quirk — checked line by line, not inferred from the
`ps`-side precedent.

## Implementation

`resolve_image_created` resolves a reference via the same
`resolve_by_reference_or_id` every other image-by-name command here
already shares, then reads `store.image_config(...).created` (the
same field `ociman system df -v`, `0285`, already reads), parsed with
the same `oci_spec_types::time::parse_rfc3339_utc` used throughout.
`earliest_image_creation`/`latest_image_creation` fold over the given
references the same way `cmd_ps`'s own `earliest_referenced_creation`
does, just with the comparison direction genuinely flipped for the
`since=`/`after=` case per the reasoning above. An image with no
recorded `created` at all (every `seed_image`-only test fixture, and
any real image whose config genuinely omits it) is silently excluded
from a `before=`/`since=` filtered listing rather than erroring the
whole command — matching this project's own established "absence over
fabrication" convention rather than a hard failure over one
unrelated image's own missing field.

## Verified

Integration (`tests/tests/ociman_images.rs`, one new test, one
pre-existing test's own filter value corrected since `before=` is now
a real, recognized key):

- `before=img-3` lists exactly the images created strictly earlier
  (the base image, with no recorded `created` at all, is silently
  excluded rather than erroring).
- `since=img-1` lists exactly the images created strictly later;
  `after=img-1` produces the identical result, confirming the real
  synonym.
- Multiple `before=` values use the earliest reference; multiple
  `since=` values use the latest — both verified against a real,
  independently-computed expected result, not merely "doesn't crash".
- An unresolvable reference image is a clear error.

Full workspace: `cargo build`/`test --workspace` (111 test binaries),
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets --
-D warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh`.

## Still ahead

Real podman's own remaining `images --filter` keys (`reference=` —
a real glob match across several normalized forms of each image's own
tag, checked directly against `imageMatchesReferenceFilter`,
meaningfully more logic than this slice — `readonly=`, `intermediate=`,
`containers=`) remain further, separately-scoped candidates.
