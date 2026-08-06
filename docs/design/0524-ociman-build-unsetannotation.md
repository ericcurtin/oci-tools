# Design note 0524: `ociman build --unsetannotation`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_build.rs`.

## What this closes

Real `docker build --unsetannotation`/`podman build --unsetannotation`
had no `ociman build` equivalent at all -- a real CLI flag `ociman`
would reject as unrecognized, unlike `--unsetlabel` (already
accepted).

## Real, checked-directly confirmation -- and a real, separate, pre-existing gap found along the way

- `~/git/podman/vendor/go.podman.io/buildah/pkg/cli/common.go:130,347`:
  `UnsetAnnotations []string`, `fs.StringSliceVar(&flags.
  UnsetAnnotations, "unsetannotation", nil, "unset annotation when
  inheriting annotations from base image")` -- the exact same
  bare-`KEY`-only shape `--unsetlabel` already established.
- `~/git/podman/vendor/go.podman.io/buildah/config.go:88-99`
  (`Builder.initConfig`): real buildah unconditionally copies a real
  OCI base manifest's own `annotations` into `b.ImageAnnotations` the
  moment any base image with a real OCI manifest is set -- a genuine
  default inheritance this project's own build path has never had
  any equivalent of at all (traced `cmd_build`'s own `manifest_
  annotations` construction end to end: it's built *solely* from the
  explicit `--annotation` CLI flag, never from a base image's own
  manifest).
- `~/git/podman/vendor/go.podman.io/buildah/image.go:645-646`:
  `--unsetannotation`'s only real job -- `for _, k := range i.
  unsetAnnotations { delete(annotations, k) }` -- removing one of
  *those inherited* entries right before the final manifest is built.

Since this project structurally never inherits base-manifest
annotations in the first place, `--unsetannotation` has nothing to
ever act on here -- a genuine, faithful no-op, the same "nothing to
skip" reasoning class `0512`-`0523` already established, this time
masking a real, separate, bigger gap (no base-manifest-annotation
inheritance at all) rather than a simpler "no interactive prompt"
one, honestly named in `Command::Build`'s own doc comment rather than
papered over (the same convention `0522` established for `ocibox
enter --yes`).

Checked one more edge case directly rather than assuming: combining
`--unsetannotation KEY` with an explicit `--annotation KEY=value` for
the exact same key in the same call. Real buildah's own apply order
(`image.go:645-650`) always deletes *before* setting, so the explicit
`--annotation` wins regardless -- the identical outcome accepting-
and-discarding this flag entirely already produces. There is no
reachable case in this project where a genuine no-op diverges from
real buildah's own actual result.

## Implementation

`unsetannotation: Vec<String>` (`#[arg(long = "unsetannotation",
value_name = "KEY")]`) added to `Command::Build`, right next to the
already-existing `unsetlabel`, accepted and immediately discarded
(`unsetannotation: _`) at the one dispatch site -- never even reaches
`build::cmd_build`'s own function signature at all.

## Tests

One new integration test in `tests/tests/ociman_build.rs`:
`build_unsetannotation_flag_is_accepted_and_behaves_identically` --
proven two ways: alone (manifest annotations stay empty, same as
before this flag existed), and combined with `--annotation` for the
exact same key in the same call (the explicit `--annotation` still
wins).

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (123
test-result blocks -- no new test file added, so the block count is
unchanged from `0523`; the documented transient `ocicri_container.rs`
flakiness under this host's own persistent CPU contention (plus a
second, genuinely concurrent process observed this session) showed
up once, confirmed transient by rerunning the specific failing test
in isolation -- passed -- then a clean full-suite rerun), `python3
ci/guards.py` (clean), `cargo deny check` (clean), `bash
ci/native-ci.sh` (clean on the first attempt with
`RUST_TEST_THREADS=2`), `bash ci/build-deb.sh` (clean on the first
attempt, real `dpkg -i`/`--version`/`dpkg -r` round trip). Pure
CLI-parsing addition, never reaching `cmd_build`'s own body at all --
no hot path touched, no `ci/bench.sh` rerun needed.

## Deliberately still out of scope

Real base-manifest-annotation inheritance itself (see above) remains
a genuinely separate, bigger gap -- closing it would mean this
project's build path starting to copy a base image's own manifest
annotations into every image it builds by default, a real default-
behavior change in the same risk class as the already-deferred
`ocibox create` auto-entry-generation gap, not a small increment.
`ocibox export --sudo` (real and tractable, but needs genuine
priority-detection logic, not a pure no-op) remains an open candidate
for a future increment.
</content>
