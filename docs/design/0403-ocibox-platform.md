# Design note 0403: `ocibox create/ephemeral --platform`

Status: implemented
Scope: `crates/oci-spec-types/src/image.rs`, `bin/ociman/src/build.rs`,
`bin/ocibox/src/main.rs`, `tests/tests/ocibox_create.rs`, `README.md`.

## What this closes

Real `distrobox create --platform`/`distrobox ephemeral --platform`
had no `ocibox` equivalent: `create_box` hardcoded
`oci_spec_types::image::Platform::host()` at both of its own real
image-resolution call sites (`resolve_or_pull`'s own platform
argument, and the `pull_unconditionally` closure's), with no override
possible at all.

## Real, checked-directly confirmation

`~/git/distrobox/internal/cli/create.go`'s own `--platform` — a plain,
unvalidated-beyond-parsing `StringFlag`, checked directly — confirms
this is a real, simple flag, not a bigger feature needing new
architecture (distrobox itself does nothing more than forward the
string straight to `podman create --platform` underneath).

## A shared primitive moved out of `ociman`-private code

`ociman pull`/`run`/`create --platform` (`0307`) and `ociman build
--platform`/`FROM --platform=` already parse the exact same
`os/arch[/variant]` grammar via a single, `ociman`-private
`parse_platform_spec` in `build.rs`. Since `bin/*` crates must never
depend on each other (`ci/guards.py`'s own `bin-deps` guard), `ocibox`
could not reuse that function directly — so it moved to
`oci_spec_types::image` (where `Platform` itself already lives), the
same "move a shared primitive out of one crate's own private code
into a crate every real caller already depends on the moment a
second, genuinely unrelated one needs it" move `glob` (`0295`) and
`resolve_by_reference_or_id` (`oci_store`, `0122`/`0213`) already went
through.

The move is a genuine, verified-zero-behavior-change one: the parsing
logic itself is untouched, only its error type changed from an ad hoc
`anyhow::anyhow!`/`anyhow::ensure!` call site to a real, structured
`PlatformParseError` (`thiserror`, matching this crate's own
established `DigestParseError`/`ReferenceParseError` convention) with
the identical three error messages verbatim. `ociman`'s own
`build::parse_platform_spec` is now a thin, `anyhow`-returning wrapper
around the shared function, so none of its three existing call sites
(`ociman pull`/`run`/`create`/`build --platform`) needed any changes
of their own at all.

## Implementation

- `oci_spec_types::image::parse_platform_spec(command, value) ->
  Result<Platform, PlatformParseError>`, plus the new
  `PlatformParseError` enum, right after `Platform`'s own `impl`
  block.
- `ocibox`'s `Command::Create`/`Command::Ephemeral` gain `--platform`
  (an `Option<String>`, matching real distrobox's own optional flag),
  threaded through `cmd_create`/`cmd_ephemeral` into the single shared
  `create_box` (both commands already funnel through it) — a bare
  `--platform` value is parsed via the shared function and falls back
  to `Platform::host()` when not given, replacing both hardcoded
  `Platform::host()` call sites at once.
- `cmd_create`/`create_box`/`cmd_ephemeral` needed
  `#[allow(clippy::too_many_arguments)]` once their parameter counts
  crossed clippy's default threshold, matching the same attribute
  several multi-flag functions elsewhere in this project already
  carry.

## Tests

Six new unit tests for `parse_platform_spec`/`PlatformParseError` in
`oci_spec_types::image` (valid `os/arch[/variant]`, valid `os/arch`
with no variant, an empty `os`, a missing architecture, too many
components, and that the given `command` name appears in the error
message) — the identical validation `build.rs`'s own prior copy had,
now with real coverage of its own (the prior `ociman`-private version
had none). Two new end-to-end integration tests in
`tests/tests/ocibox_create.rs`: a real `--platform` value matching
this test host's own actual platform still resolves and extracts a
real rootfs exactly as before this flag existed (confirming the CLI
plumbing is genuinely wired through, without re-proving the
underlying platform-selection logic itself — already verified end to
end against a real multi-platform index by `ociman_platform.rs`'s own
tests, reused here completely unchanged), and a malformed value is a
real, immediate CLI error leaving no half-created box directory
behind, the same guarantee `create_of_an_unresolvable_reference_
leaves_no_box_directory_behind` already establishes for a failed
pull. All existing tests continue to pass unmodified.

Full workspace: `cargo build --workspace --locked`, `cargo build
--workspace --locked --tests`, `cargo fmt --all --check`, `cargo
clippy --workspace --all-targets --locked -- -D warnings`, `cargo
test --workspace --locked` (0 failures), `python3 ci/guards.py`
(confirms the crate move didn't introduce a `bin`-to-`bin` dependency
or a capability-group duplicate), `cargo deny check`, `bash
ci/native-ci.sh`, `bash ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip). This touches only `ociman`/`ocibox`'s own
image-resolution CLI plumbing, not `launch.rs`'s hot path at all — no
benchmark re-run needed.

## Deliberately still out of scope

Every other unported real `distrobox create`/`ephemeral` flag
(`--unshare-*`, `--init`, `--nvidia`, `--clone`, `--additional-
packages`/`--init-hooks`/`--pre-init-hooks`, `--additional-flags`,
`--no-tty`) remains explicitly out of scope, each needing real new
architecture this project has deliberately deferred (per the
README's own milestone-7 row and `0397`'s own "still out of scope"
section) — `--platform` was the one small, mechanical gap among them.
