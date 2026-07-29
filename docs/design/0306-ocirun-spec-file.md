# Design note 0306: `ocirun spec -f/--file`

Status: implemented
Scope: `bin/ocirun/src/main.rs`, `tests/tests/ocirun_spec.rs`.

## The gap

Checked directly against an installed `crun 1.14.1`: `crun spec`
supports `-f/--file <PATH>`, writing the generated spec to `PATH`
instead of `config.json` under the bundle directory. `runc spec` has
no equivalent flag at all — this project's own `ocirun` (its primary
reference implementation for `spec`/`state`/`list`/`update` is `runc`/
`crun` both) had neither.

## A real, checked-directly quirk ported faithfully

Read `~/git/crun/src/spec.c` directly rather than assuming symmetric
behavior with the default case: crun's own "already exists" refusal
(`access(where, F_OK)`) only runs when `fname == NULL` — i.e. only for
the plain, no-`-f`-given `config.json` default. When `-f` **is**
given, crun skips that check entirely and opens the file with
`fopen(where, "w+e")`, which truncates unconditionally — a real,
silent overwrite, not a bug. Verified directly against the installed
binary before porting: `crun spec -f custom.json` twice in a row (the
second overwriting a placeholder file) succeeds both times with no
complaint, while a bare `crun spec` a second time in the same
directory refuses with `` `config.json` already exists``.

Also matched crun's own path-resolution behavior: `crun`'s own
`chdir(bundle)`-then-relative-`fname` sequence means a relative
`--file` value is resolved against `--bundle` when both are given.
`Path::join` already produces the identical result here (and, like
`chdir`+`fopen`, leaves an absolute `--file` value untouched
regardless of `--bundle`) — no extra logic needed.

## Implementation

One new `-f/--file <PATH>` flag on `Command::Spec`. `cmd_spec`'s
existing `path.exists()` guard is now gated on `file.is_none()` —
unconditionally skipped whenever an explicit `--file` was given,
matching the exact real crun rule above.

## Verified

Manual, cross-checked directly against the installed `crun 1.14.1`
before and after implementing: both tools refuse a bare `spec` a
second time against an already-existing `config.json`
(`` `config.json` already exists`` / `` file ./config.json exists;
remove it first``); both silently overwrite an explicit `--file`/`-f`
target that already held unrelated placeholder content, in both
cases producing a real, freshly-generated spec afterward, not an
error. A relative `--file` combined with `--bundle` lands in the
bundle directory, not the caller's own CWD.

Integration (`tests/tests/ocirun_spec.rs`, 3 new tests):
`spec_file_writes_to_the_given_path_instead_of_config_json` — the
named file is written, `config.json` is not created at all;
`spec_file_is_resolved_relative_to_an_explicit_bundle_directory` — a
relative `--file` lands inside `--bundle`, not the caller's own CWD;
`spec_file_silently_overwrites_an_existing_file_unlike_the_default` —
an explicit `--file` target holding unrelated content is silently
overwritten with a real, valid spec, unlike the default case (already
covered by the pre-existing `spec_refuses_to_overwrite_existing_
config` test, left unmodified).

Regression: all 7 `ocirun_spec.rs` tests pass (4 pre-existing + 3
new); full `cargo test --workspace --locked` (111 test result blocks,
0 failures).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).

Performance: `ocirun spec` is a one-shot, offline utility command, not
part of any hot-path benchmark tracked in `docs/benchmarks.md` — no
re-benchmark needed.

## Still ahead

No further `ocirun spec` gap is known against either real reference
runtime.
