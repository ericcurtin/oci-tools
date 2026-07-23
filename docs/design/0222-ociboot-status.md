# Design note 0222: `ociboot status`

Status: implemented
Scope: `bin/ociboot/src/main.rs` (`Command::Status`, `cmd_status`,
`format_built_at`); `bin/ociboot/src/origin.rs` (`read` now always
compiled, not `#[cfg(test)]`-only).

## `origin::write`'s own real, first reader

0220 wrote a real `<output>.origin.json` sidecar every time `ociboot
build-image` runs, but nothing read it back — a deliberate, explicitly
flagged gap ("nothing reads this record back yet"). This closes that
gap directly: `ociboot status <IMAGE>` reads `<IMAGE>.origin.json` and
reports exactly what's in it (`image_reference`/`image_digest`/
`image_version`/`built_at`), in both human-readable and `--json` form.

`origin::read` existed since 0220 but was `#[cfg(test)]`-only (nothing
outside its own unit tests called it) — now a real, always-compiled
public function, `ociboot status`'s own first genuine caller.

## Deliberately not a bootc-style booted/staged/rollback report

A direct research pass against real `bootc`'s own `status`
(`bootc_composefs::status`, done before 0220 even started, see 0220's
own doc comment) already identified exactly what's missing for that:
a real BLS entry actually pointing at a specific deployment image (this
project's own real partitioning/bootloader/BLS-entry-writing is still
ahead — milestone 5's own `install to-disk`), and a real boot-time
digest-in-`/proc/cmdline` convention (`ociboot-init`'s own still-ahead
job). Reporting "booted"/"staged"/"rollback" today, with neither piece
of real provenance to back it, would mean fabricating data — this
project's own repeatedly-applied standard (see 0220's own `image_
version: None` reasoning, and 0213/0214/0215's own "an honest empty
response, never an invented one" pattern for `ocicri`) says no. This
increment is scoped down to exactly `origin::write`'s own read-side
counterpart, nothing claimed beyond what's actually recorded on disk.

## `--json` finally wired up for `ociboot`

A direct research pass (done while scoping 0220) found `ociboot`'s own
`--json` global flag parsed but completely dead: no subcommand ever
read `cli.global.json` at all. `cmd_status` is the first to actually
branch on it, following the exact same pattern already established
elsewhere (`ocibox list`'s own `cmd_list(json: bool)`): a plain
`#[derive(Serialize)]` struct (`DeploymentOrigin` already had one from
0220), `oci_cli_common::output::print_json` for the JSON path, a plain
`println!` table for the human one.

## `built_at`'s own display: honest about the one real sentinel value

`built_at: 0` is a real, documented sentinel from 0220 (the image's own
`created` field was missing or unparseable) — displaying it as a
literal RFC 3339 `1970-01-01T00:00:00Z` would be technically accurate
but actively misleading (implying a real, known build time that just
happens to be the Unix epoch). `format_built_at` shows `"unknown"`
instead for exactly that one value, everything else through the same
`oci_spec_types::time::format_rfc3339_utc` this workspace already uses
everywhere else a Unix timestamp needs a human-readable form.

## Verified

- Two new integration tests in `tests/tests/ociboot_status.rs`: a real
  build-image + status round trip (both human and `--json` output,
  checked against the same `oci_store::Store` independently, not just
  re-reading `cmd_build_image`'s own output) and a clear, real error
  for a path with no matching origin record — naming the exact path,
  not a vague or lower-level I/O message.
- Manually verified both output modes end to end against a real
  `mkfs.erofs`-built image before writing the automated tests (human:
  `Image reference:`/`Image digest:`/`Image version: <none>`/`Built
  at: 2026-05-13T02:21:49Z`; `--json`: a real, valid JSON document with
  `image_version: null`).
- Full workspace: `cargo build`, `cargo test --workspace` (95/95
  result blocks — one new test binary, `ociboot_status`, all others
  unchanged — 0 failures), `cargo fmt --check`, `cargo clippy
  --all-targets -- -D warnings`, `python3 ci/guards.py` (18 capability
  groups, unaffected), `cargo deny check` (only the pre-existing
  benign warning), `bash ci/native-ci.sh`, hyperfine perf sanity on
  `ociman run --rm` (no regression — this change is entirely within
  `ociboot`, nowhere near `ociman`/`ocirun`'s own hot path).

## What this doesn't do yet

A real `status` reading BLS entries too (`ociboot list`'s own data,
cross-referenced against an origin record once a real deployment
image is actually linked to one via a BLS entry) — still blocked on
the same two missing pieces named above, both real, separate,
still-ahead milestone-5/6 work. `upgrade`/`switch`/`rollback`/`gc`,
`/etc` merge, the `grubenv`-based boot-counting protocol, and layered
mode are all still ahead too.
