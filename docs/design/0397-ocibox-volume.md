# Design note 0397: `ocibox create/ephemeral --volume`

Status: implemented
Scope: `bin/ocibox/src/main.rs`, `tests/tests/ocibox_enter.rs`, `README.md`.

## What this closes

Real `distrobox create --volume`/`distrobox ephemeral --volume` — an
extra host directory bind-mounted into the box, given once at create
time and applied on every later `enter` — had no `ocibox` counterpart
at all. This is a real, previously-missing feature (not just a gap
this project already avoided on purpose), matching the same class of
work as `ocibox --home`/`--hostname` (`0344`/`0381`).

## Real, checked-directly confirmation

- `~/git/distrobox/internal/cli/create.go`'s own `AdditionalVolumes`
  (`--volume`, a repeatable string slice) and `~/git/distrobox/
  internal/cli/ephemeral.go`'s own identical inherited flag.
- `~/git/distrobox/pkg/containermanager/providers/podman.go:394-397`:
  real distrobox passes each value straight through, verbatim, as a
  real `podman create --volume <val>` — no distrobox-side parsing or
  validation of its own at all; `podman` itself parses the string.

## Implementation

- `ocibox create`/`ephemeral` gain `--volume`/`-v HOST-DIR:CONTAINER-
  DIR[:ro]` (repeatable) — a new, `ocibox`-private `parse_box_volume`
  parses and validates the syntax at `create` time, deliberately
  **narrower** than `ociman run --volume`'s own `parse_volume`: only
  an already-absolute host path is accepted, with no named-volume
  shorthand fallback at all (`ocibox` has no `oci_store::volume::
  VolumeStore` concept to resolve one against, unlike `ociman`) — a
  real, deliberate scope narrowing, not a half-implemented feature.
  Reusing `ociman`'s own full `ParsedVolume`/`resolve_volume_host`
  machinery instead was considered and rejected: that code's own
  named-volume resolution path has no meaning here at all, so pulling
  it in would only add unused complexity for a feature this binary
  can't support.
- `BoxRecord` gains `volumes: Vec<BoxVolume>` (`#[serde(default)]`,
  the same forward-compatible-record convention `hostname`/
  `custom_home` already established), persisted once at `create` time.
- `enter_spec` appends a real bind `Mount` for each persisted volume,
  right after the existing `$HOME` mount — the identical `Mount{Type:
  "bind", Options: ["rbind"]}` shape (plus `"ro"` when read-only)
  `ociman run -v`'s own `synthesize_spec` already builds.

## Tests

Seven new unit tests for `parse_box_volume` (a plain absolute bind
mount, explicit `ro`/`rw`, a non-absolute host or container path
rejected, a named-volume-shaped value rejected outright — the
deliberate narrowing above — a missing colon, and an unsupported
option). Three new real, end-to-end integration tests in `tests/
tests/ocibox_enter.rs`: `enter_bind_mounts_a_real_extra_volume_given_
at_create_time` (a real write from inside the box lands on the real
host directory), `enter_read_only_volume_rejects_a_write_from_inside_
the_box`, and `create_rejects_an_invalid_volume_value` (a real,
immediate CLI error, no half-created box left behind). All existing
tests across `ocibox_create.rs` (5 pre-existing), `ocibox_enter.rs`
(8 pre-existing), and `ocibox_ephemeral.rs` (6 pre-existing) continue
to pass unmodified.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures), `python3
ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh`, `bash
ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg -r` round trip).
This change touches only `bin/ocibox`, which `ci/bench.sh` doesn't
measure at all — no benchmark re-run needed.

## Deliberately still out of scope

Real `distrobox`'s own additional `--volume`-adjacent behaviors (SELinux
relabeling suffixes `Z`/`z`, propagation modes) remain unsupported,
matching `ociman run --volume`'s own identical narrow option set. Real
distrobox's own remaining `create`/`ephemeral` flags (`--unshare-*`,
`--init`, `--nvidia`, `--clone`) remain out of scope, each needing real
architectural additions this project has explicitly, repeatedly
deferred (README's own milestone-7 row).
