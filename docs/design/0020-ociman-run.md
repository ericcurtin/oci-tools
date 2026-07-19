# Design note 0020: `ociman run`

Status: implemented (foreground/ephemeral only — see "What's still not
here")
Scope: `bin/ociman`'s `run` subcommand, wiring `oci-layer` (0019)
together with `oci-store`/`oci-registry` (milestone 2) and
`oci-runtime-core` (milestone 3) for the first time.

## What this closes

The last piece of `ociman run`'s own pipeline that nothing in the
workspace provided yet: pull-if-missing, apply every layer in manifest
order into a fresh rootfs, synthesize a runtime-spec `config.json` from
the image's own `ContainerConfig`, then hand it to `oci_runtime_core::
launch::run` — the exact same function `ocirun run` itself calls.
`ociman` never execs `ocirun`; it links `oci-runtime-core` directly, per
the project's own design pillars.

## A real bug this caught: `ContainerConfig`'s wire casing was never right

Running this against a real pulled image for the first time (`docker.io/
library/busybox`) produced an empty command — `ociman run busybox`
failed with "no command to run", even though the image plainly has
`CMD ["sh"]`. The raw downloaded config blob confirmed why:
`"config": {"Cmd": ["sh"], "Env": [...]}` — the image-spec's own
`config` object uses **`PascalCase`** field names (a historical quirk
inherited from Docker's original Go struct field names, serialized with
no `json` tag override, that the OCI image-spec adopted verbatim for
compatibility), but `oci_spec_types::image::ContainerConfig` had no
`#[serde(rename_all = ...)]` at all. Every field — `Cmd`, `Entrypoint`,
`Env`, `WorkingDir`, `User` — had silently deserialized to its default
for *every image ever pulled* since this type was written in milestone
2, and nothing had ever noticed because no existing test actually
checked a `ContainerConfig`'s fields came out non-default. Fixed with
`#[serde(rename_all = "PascalCase")]`; a real fixture
(`crates/oci-spec-types/tests/fixtures/busybox-image-config.json`,
captured verbatim from that same real pull) and a new test lock this
in at the type level, and `tests/tests/ociman_run.rs` locks it in from
`ociman run`'s own perspective too (`run_actually_uses_cmd_from_a_
pascal_case_wire_config`, seeding a raw hand-written `PascalCase` JSON
config rather than going through the — now-fixed — `ContainerConfig`
struct, so a future regression in the type couldn't accidentally hide
behind using the same fixed struct on both sides of the test).

This is exactly the value of this project's established "verify
against the real thing" discipline: a spec-shaped Rust type that
*compiles* and *parses without error* (empty objects are valid JSON)
gave zero signal that it was silently wrong for over a thousand lines
of prior work, until an actual image was actually run.

## Ephemeral by design (for now)

Each `ociman run` extracts into a fresh `tempfile::TempDir`, explicitly
closed right before the process exits with the container's real exit
code (`std::process::exit` skips destructors, so relying on `Drop`
here would leak every successful run's rootfs). There is no persistent
container record — nothing survives to `ps`/`inspect`/`rm` later, since
none of those exist yet. This is a deliberate, narrower scope than real
`podman run` (which keeps the container around unless `--rm`), chosen
because keeping an unlistable, un-removable rootfs directory around
provides no value yet and only spends disk space — matching this
project's repeated "narrow, honest, useful scope over a half-finished
wider one" pattern.

## `ENTRYPOINT`/`CMD`/`USER` handling: the common case, honest gaps for the rest

* Command resolution matches real `docker run`/`podman run`:
  `ENTRYPOINT` is always kept; explicit CLI args replace `CMD`,
  otherwise the image's own default `CMD` is used; an error if neither
  ends up set.
* `USER`: only `""`/`"0"`/`"0:0"` are accepted. A **named** user (e.g.
  `USER app`) needs resolving against `/etc/passwd` *inside the
  extracted rootfs*, not implemented yet. A **non-root numeric** user
  (e.g. `USER 1000`) is rejected with a clear, specific error rather
  than attempted and failing confusingly deep inside the forked child:
  this project's rootless containers currently map only container uid
  `0` (to the host's own euid, one entry, no subordinate range) — see
  `oci_runtime_core::namespaces` — so `setresuid(1000)` inside the
  container would simply fail `EINVAL` (that uid was never mapped).
  Both limits are real, both are loud rather than silent, and both are
  exactly the same wall this project's rootless model has already hit
  and documented elsewhere (0013's `identity` design note).

## Verified against a real kernel and a real image, then covered offline

Manually verified end to end against `docker.io/library/busybox` (a
real registry pull, real network, deleted after): default `CMD`,
explicit-args override, exit-code propagation, and the `ContainerConfig`
bug fix above, all confirmed by hand before writing any automated test.

The automated tests (`tests/tests/ociman_run.rs`, five cases) then
reproduce every one of those scenarios **without any network
dependency**: a helper hand-seeds a local `oci_store::Store` with a
synthetic-but-structurally-real image (a real `busybox` binary,
gzip-tarred exactly like a real layer blob, `oci_spec_types`' own
`ImageConfig`/`ImageManifest` types ingested exactly the way a real
`oci_registry::pull` leaves them) — `ociman`'s own `resolve_or_pull`
finds it already present and never touches the network, so this
exercises the identical extraction/synthesis/launch code path a real
pull would, deterministically, in CI (which has no reliable internet
access — the reason nothing in this workspace has attempted a
network-dependent automated test before now).

## What's still not here

* `ps`/`inspect`/`rm`/`stop` for containers — no persistent record
  exists yet (see "Ephemeral by design" above).
* Named `USER` resolution (`/etc/passwd` parsing inside the rootfs) and
  non-root numeric users (needs a subordinate uid/gid range).
* Volumes, port mapping (`-p`), `--name`, environment overrides
  (`-e`/`--env`), interactive/`-it` mode — none of `docker run`'s wider
  flag surface is implemented, only the image-pull-and-execute core.
* `zstd`-compressed layers (already a documented `oci-layer` gap, 0019).
