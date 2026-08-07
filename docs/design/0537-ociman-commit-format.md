# Design note 0537: `ociman commit --format`/`-f`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_commit.rs`.

## What this closes

`ociman commit`'s own doc comment (and `0523`'s design note) has
named `--config`/`--format`/`--include-volumes` as its own last
open gaps for a while. This closes `--format`.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/commit.go:64-65`:
  ```go
  formatFlagName := "format"
  flags.StringVarP(&commitOptions.Format, formatFlagName, "f", "oci", "`Format` of the image manifest and metadata")
  ```
- `~/git/podman/pkg/domain/infra/abi/containers.go:645-654`
  (`ContainerCommit`):
  ```go
  switch options.Format {
  case "oci":
      mimeType = buildah.OCIv1ImageManifest
      if len(options.Message) > 0 {
          return nil, fmt.Errorf("messages are only compatible with the docker image format (-f docker)")
      }
  case "docker":
      mimeType = manifest.DockerV2Schema2MediaType
  default:
      return nil, fmt.Errorf("unrecognized image format %q", options.Format)
  }
  ```
- **Verified live against a real installed `podman 4.9.3`** (not
  assumed from source alone), using a real running container:
  `commit --format bogus` → `Error: unrecognized image format
  "bogus"` (exit 125); `commit --message "hi"` (default, `oci`
  format) → `Error: messages are only compatible with the docker
  image format (-f docker)`; `commit --format docker --message "hi"`
  → succeeds; `commit --format oci` (explicit) → succeeds, identical
  to no flag at all.
- Confirmed this project has no Docker Schema2 manifest writer
  anywhere: `oci-store`'s own manifest/config-writing code only ever
  emits real OCI media types (`MEDIA_TYPE_IMAGE_MANIFEST`) — so
  `--format docker` really would need a clear, honest error, not
  silent mislabeling of an OCI manifest as Docker's.

## Implementation

`bin/ociman/src/main.rs`:
- New `Command::Commit::format: String`, `#[arg(short = 'f', long,
  default_value = "oci")]`, and the identical field on the nested
  `ContainerCommand::Commit` alias (whose own doc comment previously
  named `--format` alongside `--config`/`--include-volumes` as an
  open gap — updated to note it's now closed, matching `--quiet`'s
  own identical `0523` precedent for the same alias).
- `cmd_commit` gains a `format: &str` parameter, validated *first*,
  before any other work at all (matching the existing "`--change`
  validated up front, before any freeze/diff work" convention already
  in this same function): `"oci"` is a true no-op; `"docker"` is a
  real, honest, immediate error (this project has no Docker Schema2
  writer); anything else is real podman's own exact `"unrecognized
  image format %q"` wording.
- Deliberately preserves this project's own already-documented
  `--message` divergence (real podman's own `-f docker`-only
  restriction on `--message`, which this project's `--message`
  already ignores by writing to the OCI-native `history[].comment`
  field instead) — that interaction never actually arises here in
  practice anyway, since `--format docker` itself is rejected outright
  before any such check could ever matter, so no extra code was
  needed to avoid a regression.

## Tests

Four new integration tests in `tests/tests/ociman_commit.rs`:
- `commit_format_oci_is_the_default_and_a_true_no_op`
- `commit_format_docker_is_a_clear_error`
- `commit_format_unrecognized_value_is_a_clear_error`
- `container_commit_format_flag_works_through_the_alias`

A real bug in the *test itself* (not the feature) found and fixed
while verifying: the first version of `commit_format_oci_is_the_
default_and_a_true_no_op` asserted the digests of two *separate*
`commit` invocations (one with `--format oci`, one with no flag) were
byte-identical. This project's own `commit_inner` always stamps
`config.created` with the real wall-clock commit time
(`format_rfc3339_utc(SystemTime::now())`), so two genuinely separate
invocations — even for the exact same underlying diff — are never
byte-identical top-level digests, regardless of `--format`; the test
happened to pass when both invocations landed within the same
second and failed on a slower/differently-timed rerun, a real,
timing-dependent bug in the test's own assertion, not the feature.
Fixed by comparing the two committed images' own real layer content
(`rootfs.diff_ids`, unaffected by any timestamp) instead of their
top-level digests — the actually honest way to prove `--format oci`
changes nothing. Reran the fixed test 5 consecutive times (spanning
real second boundaries) to confirm the fix, not just once.

Manually exercised end to end beyond the automated tests: a real
image built via `ociman build`, a real container run to completion,
then `commit --format bogus` (error), `commit --format docker`
(error), `commit --format oci` and a bare `commit` (both succeeding
with byte-identical `rootfs.diff_ids`, confirmed via `ociman inspect
--json`), and the `container commit --format bogus` alias (identical
error).

Full workspace: `cargo build --workspace --locked` (clean — caught
and fixed one duplicated `#[allow(clippy::too_many_arguments)]`
attribute along the way, from not noticing `cmd_commit` already had
one before this change added a second), `cargo fmt --all` (clean
after two auto-fixes), `cargo clippy --workspace --all-targets
--locked -- -D warnings` (clean), the full `ociman_commit.rs` suite
(21/21), a full `cargo test --workspace --locked` run (this host had
several genuinely concurrent `opencode` sessions active throughout;
one run hit the already-documented transient `ociman_logs.rs` follow
test, confirmed transient by immediate isolated rerun; a fully clean
final run: 126 test-result blocks, 0 failures), `python3 ci/guards.py`
(clean), `cargo deny check` (clean), `bash ci/native-ci.sh` (clean on
the first attempt), `bash ci/build-deb.sh` (clean on the first
attempt, real `dpkg -i`/`--version`/`dpkg -r` round trip). Pure
CLI-parsing-and-validation addition — no hot path touched, no
`ci/bench.sh` rerun needed.

## Deliberately still out of scope

`--config` (an override-config JSON blob) and `--include-volumes`
remain `Command::Commit`'s own last two open gaps, unaffected by this
increment.
