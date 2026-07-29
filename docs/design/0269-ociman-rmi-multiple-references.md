# Design note 0269: `ociman rmi <ref1> <ref2> ...`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_rmi.rs`.

## Closing another real gap, and correcting a speculative one

`ociman rmi` only ever removed exactly one, explicitly named image
reference. Real `podman rmi img1 img2 img3` (checked directly against
a real installed `podman rmi --help`, whose own worked example is
literally `podman rmi c4dfb1609ee2 93fd78260bd1 c0ed59d05ff7`) accepts
any number of references in one call. This closes that gap.

`0268`'s own "still ahead" note had speculated `ociman rmi` might gain
a `--filter`-driven bulk-removal shape matching `ociman prune`/
`images`. Checked directly against a real installed `podman rmi
--help`: real `podman rmi` has no `--filter` at all (only `-a/--all`,
`-f/--force`, `-i/--ignore`, `--no-prune` — `--filter` belongs to
`image prune`, a genuinely different command). That speculative note
was inaccurate and is corrected here rather than acted on; `-a/--all`,
`-i/--ignore`, and `--no-prune` remain real, separately-scoped
candidates for a future increment.

## A genuinely different multi-target policy than `ociman rm`

Real `podman rmi` was tested directly (tagging two real images,
removing them together with a bogus third name in between) rather
than assumed to share `ociman rm`'s own recently-added (`0267`)
all-or-nothing preflight-resolution policy for multiple container
IDs. It does not:

```
$ podman rmi rmitest1 bogus-image-xyz rmitest2
Untagged: localhost/rmitest1:latest
Untagged: localhost/rmitest2:latest
Error: bogus-image-xyz: image not known
```

Both real, valid images were removed even though `bogus-image-xyz`
never resolved at all — a genuinely *different* policy than `ociman
rm`'s own preflight-resolve-everything-first rule for container IDs.
Repeating with the bogus name in different positions confirmed order
doesn't matter: every reference is processed independently, any
failure (unresolvable name, sibling-tag ambiguity, in-use-by-container
gate) never blocks the others, and every error is reported once every
reference has had its own attempt.

`ociman rmi` now matches this exactly: each reference goes through the
*same*, unchanged, full per-reference `rmi` logic (`rmi_one`, factored
out of the previous single-reference `cmd_rmi` body verbatim — a pure,
verified-unchanged move for the single-reference case, confirmed by
all 10 pre-existing tests passing unmodified), in a loop that
continues past any individual failure, collecting every error and
reporting it, then exiting non-zero if at least one reference failed.

## `--json` output shape

Real `podman rmi` has no `--json` flag at all — this project's own
`--json` global flag is a pre-existing addition (0102). To preserve
exact backward compatibility for the pre-existing, overwhelmingly
common single-reference case (every existing test locks into a single
JSON *object*, not an array), the shape is arg-count-dependent:
exactly one reference still prints the identical single `RmiResult`
object as before; more than one reference prints a JSON *array* of
one `RmiResult` per reference actually removed (a reference that
failed to resolve/remove is simply absent from the array, matching
the plain-text mode's identical "print nothing for a failure, only
the id for a success" behavior).

## Verified

Integration (`tests/tests/ociman_rmi.rs`, three new tests):

- `ociman rmi ref1 ref2` removes both, printing each removed
  reference.
- One unresolvable reference among otherwise-valid ones still removes
  the others (regardless of its position in the list), matching real
  podman's own checked-directly behavior above — a real, deliberate
  contrast with `ociman rm`'s own all-or-nothing preflight noted
  directly in the new test's own doc comment.
- `--json rmi ref1 ref2` (more than one reference) prints a JSON
  array, one object per removed reference, in the order given.

Regression: all 10 pre-existing `ociman_rmi.rs` tests (single
reference, by-ID sibling-tag ambiguity, dependent-container gate,
`--json` single-object shape, `<none>` sentinel display) still pass
completely unmodified after the `rmi_one` extraction.

Full workspace: `cargo build`/`test --workspace`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`python3 ci/guards.py`, `cargo deny check`, `ci/native-ci.sh`,
`ci/build-deb.sh`.

## Still ahead

`ociman rmi -a/--all`, `-i/--ignore`, and `--no-prune` (real podman's
own remaining `rmi`-specific flags, checked directly above) remain
real, similarly-scoped next candidates. (`-i/--ignore` is implemented
in `0270`, which also found `--no-prune` doesn't actually apply to
this project's own content-addressed store at all.)
</content>
</invoke>
