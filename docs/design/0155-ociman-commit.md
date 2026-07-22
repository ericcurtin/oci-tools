# Design note 0155: `ociman commit`

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Commit` CLI variant and
dispatch; new `cmd_commit`; new private `CommitResult` for `--json`
output); `tests/tests/ociman_commit.rs` (5 new integration tests).

## What this does

`ociman commit <container> <image>`: create a new image from a
container's own changes relative to the image it was created from —
matching real `docker commit`/`podman commit`'s own core effect
exactly: one new layer, containing everything the container's own
filesystem gained/lost/changed since it started, stacked on top of the
exact same base layers/history its own source image already had.

`--author`: sets the resulting image's own top-level `author` field
(matches real `podman commit --author`/buildah's own `SetMaintainer`
exactly — checked directly, `~/git/podman/vendor/go.podman.io/buildah/
config.go`: sets `OCIv1.Author`, this project's own `ImageConfig.author`
being the exact same OCI field). `--message`: since this project only
ever produces OCI-format images, and the OCI image spec has no
top-level "Comment" field at all (checked directly: real buildah's own
`SetComment` sets a Docker-format-only field, explicitly warning and
discarding it for OCI format — `~/git/podman/vendor/go.podman.io/
buildah/config.go`), `--message` instead sets the new layer's own
per-entry `history[].comment` — a real field the OCI spec itself
defines, and the closest real equivalent this project's OCI-only image
config actually has.

## Reuses existing, already-tested infrastructure end to end — no new diffing/layer logic at all

This is genuinely the same operation `ociman build`'s own `RUN`/
`COPY`/`ADD` steps already perform (diff a live rootfs against some
"before" state, turn the diff into one new stored layer, append it to
an `ImageConfig`'s own layer list/history) — just with a running (or
stopped) container's own current state standing in for a build stage's:

* The "before" reference is the container's own persisted
  `BASE_SNAPSHOT_FILENAME` (0149), exactly like `cmd_diff` already
  uses — never a second, independent extraction of the base image
  (see `cmd_diff`'s own doc comment for the real false-positive bug
  that alternative was found to produce).
* `oci_dockerfile::commit_layer`/`record_layer` (already shared by
  `ociman build`'s three commit sites) turn the diff into a real
  stored layer and append it to a cloned copy of the base image's own
  `ImageConfig`/manifest layer list.
* The new manifest/config/tagging sequence at the end mirrors
  `ociman build`'s own final assembly step exactly (same
  `MEDIA_TYPE_IMAGE_MANIFEST`/`MEDIA_TYPE_IMAGE_CONFIG` descriptors,
  same `store.ingest`-then-`put_image` sequence).

The one genuinely new piece of code is resolving the container's own
recorded base image (`ANNOTATION_IMAGE`, already written by `cmd_run`)
back into a real `ImageRecord` via `store.resolve_image` — the exact
same lookup `cmd_rmi`'s own "is any container still using this image"
check already performs for the same annotation.

## `image` is currently required

Real `podman commit`'s own `IMAGE` argument is optional (an omitted
one produces a real, but untagged, image — `podman images` shows it as
`<none>:<none>`, retrievable only by its own digest/ID afterward).
This project's `oci_store::Store` has no established "an image can
exist with no tag at all" storage convention anywhere yet — `ociman
build --tag` has this exact same, already-documented narrowing for the
identical reason. Requiring `image` here keeps this increment's own
new code confined to `commit`'s own real, new logic (diff-into-a-new-
layer/manifest), rather than also being the first place in the
codebase to invent untagged-image storage.

## Real, automated tests

Five new integration tests in `tests/tests/ociman_commit.rs`, matching
`tests/tests/ociman_diff.rs`'s own established fully-offline pattern
(same `.rootless-overlay-supported` forcing, same "one real test
exercises whichever mode this host's own default actually picks"
convention for the rootless-overlay-rootfs gap):

* `commit_round_trips_an_added_file_and_a_deleted_one_into_a_real_
  runnable_image` — the real end-to-end proof: a container adds a
  file and deletes `/bin/sh`, gets committed, and a brand-new
  container run *from the committed image* (deliberately using
  `/bin/busybox <applet>` directly rather than a shell, since `/bin/sh`
  was deleted) reads the added file's real content back and confirms
  the deleted file is genuinely gone — not just "diff reported it",
  the whiteout actually propagated into a real, separately runnable
  image.
* `commit_sets_author_and_message_and_grows_history_by_exactly_one_
  real_layer` — `--author`/`--message` land in the right fields
  (`ociman inspect`'s own `author`, `ociman history`'s own newest
  entry's `comment`), and the resulting image's own history is
  exactly the base image's own history plus one new entry (never
  more, never fewer, and correctly ordered newest-first).
* `commit_requires_the_image_argument_to_parse_as_a_reference`,
  `commit_of_an_unknown_container_is_a_clear_error`,
  `commit_is_a_clear_error_for_a_rootless_overlay_rootfs_container` —
  the same real error-handling coverage `cp`/`diff` already established
  for their own identical gaps, extended here.

Full `cargo build --workspace --locked`/`cargo test --workspace
--locked` (2 clean full-workspace runs, plus repeated standalone runs
of the new `ociman_commit` tests specifically)/`cargo fmt --all
--check`/`cargo clippy --workspace --all-targets --locked -- -D
warnings`/`python3 ci/guards.py`/`cargo deny check`/`bash
ci/native-ci.sh` all clean.

## What this doesn't do yet

* **`image` as optional** (an untagged/`<none>` result) — see above; a
  real, deliberate narrowing, not a bug.
* **`--pause`** (real podman's own default: freeze the container via
  its own cgroup freezer while diffing/committing, for a consistent
  snapshot of a container that's still actively writing) — this
  project already has real cgroup-v2-freezer pause/resume support
  (0142/0143) that a future increment could wire in here; deferred for
  now to keep this increment's own new code confined to the actual
  commit/layer-assembly logic.
* **`--change`** (apply Dockerfile-instruction-style overrides —
  `CMD`/`ENTRYPOINT`/`ENV`/`EXPOSE`/`LABEL`/`ONBUILD`/`USER`/
  `VOLUME`/`WORKDIR`/`STOPSIGNAL` — as part of the same commit) and
  **`--config`** (merge an arbitrary container-config JSON file) —
  real podman flags, not implemented; a future increment's own real
  gap to close, not attempted here.
* **`--squash`** (flatten the new layer(s) into the base image's own
  single combined layer) and **`--include-volumes`** — real podman
  flags, deliberately out of scope for this first increment.
