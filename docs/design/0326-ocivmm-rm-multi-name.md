# Design note 0326: `ocivmm rm` accepts multiple explicit names

Status: implemented
Scope: `bin/ocivmm/src/main.rs`, `tests/tests/ocivmm_create_list_rm.rs`.

## What this closes

`ocivmm rm` previously accepted exactly one positional `name: Option<
String>`, or `--all` — no way to remove several named VMs in one call.
`ocibox rm` already got this exact widening in `0321` (`name: Option<
String>` → `names: Vec<String>`); `ocivmm rm` was the one sibling
command left with the older, narrower shape. A small, mechanical port
of an already-solved, already-tested pattern to a sibling binary — no
new architecture, no new primitive.

## Semantics chosen

`ocivmm` is this workspace's own design (not a drop-in replacement for
any single existing tool the way `ociman`/`ocirun`/`ocibox` are), so
there's no single real tool's own multi-name convention to match here.
Rather than invent a third convention, this reuses the one this
project's own `ociman rm`/`kill`/`stop` multi-target support already
established (`docs/design/0310`-`0318`): every given name is resolved
(checked to exist) *before* anything is actually removed — one
unresolvable name aborts the whole call, leaving every VM (including
ones that would have resolved) untouched — but once every name
resolves, each is still genuinely attempted regardless of an earlier
one's own removal failure. This is the direct, minimal generalization
of `ocivmm rm`'s own pre-existing single-name behavior (already a hard,
immediate error for an unknown name) rather than `ocibox rm`'s own
different, more tolerant "unresolvable name is just a warning" choice
— that choice was made there specifically to match real distrobox's
own checked-directly behavior, which doesn't apply to `ocivmm` at all.

## Implementation

`Command::Rm.name: Option<String>` → `names: Vec<String>` (a plain
positional `Vec`, the same clap shape `ocibox rm`'s own `names` field
already uses). `cmd_rm(names: &[String], all: bool)` keeps its
existing four-way `--all`/no-name dispatch shape, with the two
single-name arms replaced by one new `remove_named_vms(names)`: a
first pass validates every name's charset (`validate_vm_name`, the
existing path-traversal guard, unchanged) and existence
(`vms_root().join(name).is_dir()`, the same check `remove_one_vm`
already made, just pulled out to a pre-pass); a second pass then calls
the existing, unmodified `remove_one_vm` for each, collecting (not
aborting on) the first real removal failure — matching `--all`'s own
already-established "attempt every one, report the first failure"
resilience immediately above it in the same function.

## Verified

`cargo build -p ocivmm --locked`; manual: `ocivmm rm --help` shows the
new `[NAMES]...` positional and updated doc text. Two new integration
tests in `tests/tests/ocivmm_create_list_rm.rs` (15 total, 13
pre-existing, all pass unchanged): `rm_accepts_multiple_explicit_
names_in_one_call` (three seeded VMs, two named explicitly, both
removed in the given order, the third untouched) and `rm_with_one_
unresolvable_name_among_several_removes_nothing` (a real VM plus an
unknown name in the same call: the whole call fails, and the real VM
survives completely untouched, not partially removed).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (113 test result blocks,
0 failures), `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).

Performance: `ocivmm rm` is a one-shot, offline command, not part of
any hot-path benchmark tracked in `docs/benchmarks.md`; the single-
name/`--all` cases are unchanged in shape and cost. No re-benchmark
needed.

## Still ahead

Nothing new opened by this note. `ocibox`'s remaining gaps (icon
handling for `export --app`, `stop`/`upgrade`/`generate-entry`/
`assemble`) and `ocivmm`'s own remaining gaps (a lighter-weight offline
`create` success-path fixture, the HVF/macOS phase-4 blocker) remain
the same separately-scoped future candidates `0323`/`0324`/`0325`
already listed.
