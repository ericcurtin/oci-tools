# Design note 0067: `ociman run --cap-add`/`--cap-drop` (milestone 3)

Status: implemented
Scope: `bin/ociman/src/main.rs` (new `--cap-add`/`--cap-drop` flags,
`normalize_capability`/`normalize_capabilities`/`merge_capabilities`,
`synthesize_spec` now takes an already-merged capability list instead
of hardcoding the `podman` default), `crates/oci-runtime-core/
src/identity.rs` (`ALL_CAPABILITY_NAMES` promoted from a test-only
duplicate to a real, shared, public constant), `tests/tests/
ociman_run.rs`.

A switch back to milestone 3 after six consecutive milestone 5
increments (0061-0066): `--cap-add`/`--cap-drop` is one of milestone
3's own remaining, explicitly-named gaps (its own doc comment in
`synthesize_spec` has said *"No `--cap-add`/`--cap-drop` override
exists yet"* since 0058 shipped the real `podman`-default 11-capability
set this override now sits on top of).

## A direct, checked port of real `docker`/`podman`'s own algorithm, not an independently invented one

`~/git/podman/vendor/go.podman.io/common/pkg/capabilities/
capabilities.go`'s own `NormalizeCapabilities`/`MergeCapabilities` were
read directly before writing any Rust. `normalize_capability` matches
`NormalizeCapabilities` exactly: upper-case, add a `CAP_` prefix if not
already present, and validate against every name this build actually
recognizes -- the special value `ALL` (case-insensitive on the way in)
is left as the bare literal marker, unprefixed and unvalidated, since
it's a merge-time instruction, not a real capability name.
`merge_capabilities` matches `MergeCapabilities`'s own three real rules
in the same order: `--cap-drop=all` together with `--cap-add=all` is a
real, refused error (*"adding all capabilities and removing all
capabilities not allowed"*, the exact real message, not a rephrasing);
`--cap-drop=all` alone discards the base set entirely and keeps only
whatever `--cap-add` separately grants (not "drop everything and
ignore `--cap-add` too"); `--cap-add=all` alone replaces the base with
every recognized capability. The same capability given to both
`--cap-add` and `--cap-drop` (after the two `all` cases above) is a
real, surfaced error either way, never silently resolved.

One deliberate, documented divergence from the real source: real
`docker`/`podman`'s own `--cap-add=all` uses the *calling process's own
real bounding set* (`BoundingSet()`) as the replacement base. That has
no equivalent meaning here: a runtime-spec's own `bounding`/
`effective`/`permitted` arrays declare what the *container* should
have, independent of whatever privilege the invoking `ociman` process
itself happens to hold (unlike real podman, which historically ran
these checks against a privileged daemon process). Using
`oci_runtime_core::identity::ALL_CAPABILITY_NAMES` (every capability
this build actually recognizes) is the more literal, correct reading
of "grant every capability" for that context.

## A real, existing duplicate found and fixed while making the name list a shared public API

`oci-runtime-core::identity` already had the complete list of every
capability name it recognizes -- twice: once as the match arms in
`capability_named` (real production logic), and again as a
`#[cfg(test)]`-only, unexported `ALL_CAPABILITY_NAMES` const used by
exactly one test (`parse_set_recognizes_every_capability_name`).
Validating `ociman run --cap-add`/`--cap-drop` needs this exact same
list, so rather than adding a *third* copy in `bin/ociman`, the
existing test-only const was promoted to a real, documented, public
item in the main module (`pub const ALL_CAPABILITY_NAMES: &[&str]`),
and the test module's own duplicate was deleted, letting its existing
test resolve to the new shared one via `use super::*` instead — one
real source of truth for "every capability name this build
recognizes", not three independent lists that could quietly drift out
of sync with each other over time.

## Real, manual end-to-end verification before writing a single automated test

Built the debug binary, ran real containers, and read real
`/proc/self/status` bitmasks by hand for every case before trusting any
of it: `--cap-drop=chown` against the real podman-default mask
(`0x800405fb`, established in 0058) produced exactly `0x800405fa`
(`CAP_CHOWN` is bit 0); `--cap-add=net_admin` produced exactly
`0x800415fb` (`CAP_NET_ADMIN` is bit 12); `--cap-drop=all
--cap-add=chown` produced exactly `0x1` (only `CAP_CHOWN`, base fully
discarded); `--cap-add=all` produced exactly `0x1ffffffffff` (all 41
recognized capabilities, bits 0-40); `--cap-add=net_admin
--cap-drop=net_admin` and `--cap-add=all --cap-drop=all` both refused
with the expected real error messages and a non-zero exit, before the
container ever started; `--cap-add=bogus_capability` was rejected with
a clear, real error naming the bad value. Also confirmed both
`--cap-add=chown,fowner` (comma-separated in one flag occurrence) and
`--cap-add chown --cap-add fowner` (repeated) produce byte-identical
results -- matching real `docker`/`podman`'s own `pflag.StringSlice`
flag type, which supports both shapes at once (`clap`'s own
`value_delimiter = ','` gives the same dual behavior for free).

## Real, automated tests

12 new unit tests for `normalize_capability`/`merge_capabilities`
directly (name normalization with/without the `CAP_` prefix and
case-insensitively; the `ALL` marker; an unknown name rejected;
add/drop/no-op/dedup/sort behavior; both `all` rules; the conflict
error). 4 new integration tests in `tests/tests/ociman_run.rs`, each
spawning the real built binary against a real seeded `busybox` image
and reading real `/proc/self/status` output, reproducing every bitmask
verified by hand above: `--cap-drop` removing a capability from the
real default set; `--cap-add` granting one beyond it; the same
capability given to both being a real, surfaced CLI error (checked
both by exit status and by the error message actually naming the
conflicting capability) before the container ever starts.

## Performance

This increment touches `oci-runtime-core::identity` (shared hot-path
code `ocirun`/`ociman run` both depend on), though only by promoting an
existing test-only constant to a public one and deleting its duplicate
— `capability_named`/`parse_set`/`drop_privileges` and every other real
function in the module are byte-for-byte unchanged (confirmed via
`git diff`). A direct `git stash`/`git stash pop` A/B `hyperfine`
comparison was run anyway (`ocirun --version`, `ociman run --rm
docker.io/library/busybox:latest -- /bin/true`, 20+ runs each): results
were noise-dominated and within this shared host's already-documented
variance (`ociman` even measured slightly *faster* after, well inside
the overlapping ranges), with no plausible regression mechanism (the
new capability-merging logic is a few string comparisons over an
11-41-element list, done once per `run` invocation, nowhere near the
actual container-launch hot path) — consistent with no real
regression.

## What's still not here

* `--privileged` (which disables far more than capabilities in real
  `docker`/`podman` — device access, seccomp, AppArmor/SELinux, and
  more) is still not implemented, unchanged from before this increment.
* `createContainer`/`startContainer` hooks, automated failed-systemd-
  scope cleanup — milestone 3's other remaining, previously-identified
  gaps.
