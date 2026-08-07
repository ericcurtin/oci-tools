# Design note 0558: `ocibox export --verbose`/`-v`

Status: implemented
Scope: `bin/ocibox/src/main.rs`, `tests/tests/ocibox_export.rs`.

## What this closes

Real `distrobox export` (a shell script, genuinely separate from the
newer Go-CLI rewrite `0536`/`0557` already traced for `rm`/`enter`/
`ephemeral`) has its own real, root-level `--verbose`/`-v` flag.
`ocibox export` had no equivalent at all — a real CLI flag it would
reject as unrecognized.

## Real, checked-directly confirmation

- `~/git/distrobox/internal/inside-distrobox/assets/distrobox-
  export:107`: help text, `--verbose/-v: show more verbosity`.
- `~/git/distrobox/internal/inside-distrobox/assets/distrobox-
  export:120-122`: real flag registration in the script's own
  arg-parse loop: `-v | --verbose) shift; verbose=1 ;;`.
- `~/git/distrobox/internal/inside-distrobox/assets/distrobox-
  export:203-204`: the live consumer — `if [ "${verbose}" -ne 0 ];
  then set -o xtrace; fi`, unconditionally applied right after arg
  parsing, for the rest of the script's own execution. `set -o
  xtrace` is a real bash builtin (echoes every subsequent command the
  script itself runs, expanded, to stderr) — genuinely live, not
  dead/vestigial code.
- Confirmed `export` has **no Go-CLI counterpart at all**: `ls
  ~/git/distrobox/internal/cli/` and `~/git/distrobox/pkg/commands/`
  show `create.go`/`enter.go`/`ephemeral.go`/`rm.go`/`stop.go`/etc.,
  but no `export.go` anywhere in either directory — this is a
  genuinely separate, previously-unexamined chain, not something
  `0557`'s own "still out of scope" note (which named `export` only
  because it hadn't traced *this* file's own separate flag at all)
  already looked at and decided against.

## Why this is a real, faithful no-op (unlike `0557`'s `enter`/
`ephemeral --verbose`)

Real distrobox's own effect here (`set -o xtrace`) is inherently
specific to a shell script tracing its own subsequent statements as
bash executes them — this project's `cmd_export` is ordinary,
already-compiled Rust with no equivalent "echo each step as it runs"
facility, and (checked directly: grepped the whole file) contains
**zero** `tracing::debug!`/`trace!` calls of its own anywhere in
`cmd_export`/`cmd_export_bin`/`cmd_export_app`/`cmd_export_list_apps`/
`cmd_export_list_binaries`. Unlike `0557`'s `enter`/`ephemeral
--verbose` — which genuinely surfaces this project's own pre-existing
`tracing::debug!` instrumentation on their real `oci_runtime_core::
launch`/`systemd_cgroup` hot path when forced to `debug` level —
forcing `--log-level debug` here would only ever print the one
universal `"ocibox starting"` line already common to every command
regardless of this flag, not a genuine `xtrace`-equivalent behavior
change. Accepted for real CLI compatibility, changes nothing — the
same faithful-no-op class as `0536`'s `rm --verbose` (a different,
separately-traced consumer chain) and `0556`'s `ps --sync`.

## Why this is narrow

Entirely contained to one CLI struct (`Command::Export`) and its one
dispatch site — `cmd_export`'s own function signature is untouched
(accept-and-discard `verbose: _`, the same convention `sync`/`sudo`
adjacent no-op flags already use). No persisted state, no lifecycle
reload sites.

## Implementation

`Command::Export` gains `verbose: bool` (`#[arg(long, short = 'v')]`
— no short-flag collision within `Export`, unlike `Create`/
`Ephemeral`'s own `-v`/`--volume` collision `0536`/`0557` had to work
around). Accepted and immediately discarded (`verbose: _`) at the one
dispatch site.

## Tests

One new integration test in `tests/tests/ocibox_export.rs`:
`export_bin_verbose_flag_and_its_short_alias_are_accepted_and_behave_
identically` — proves both `--verbose` and `-v` parse and produce a
byte-identical generated wrapper compared to a plain `export`.

Manually verified end to end beyond the automated test: a real image
built via `ociman build`, a real box created from it, `ocibox export
--bin /bin/echo --verbose` and `-v` both confirmed to succeed and
produce the identical wrapper script.

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (128
test-result blocks, all passing — no new test file added, so the
block count is unchanged from `0557`), `python3 ci/guards.py`
(clean), `cargo deny check` (clean), `bash ci/native-ci.sh` (a single
isolated `ociman_run.rs` cgroup-conf test flaked once under this
host's own concurrent-session CPU contention, confirmed transient by
an immediate isolated rerun, then a fully clean full run on the
second attempt), `bash ci/build-deb.sh` (clean on the first attempt,
real `dpkg -i`/`--version`/`dpkg -r` round trip). A pure
CLI-parsing-and-discard addition — no hot path touched, no
`ci/bench.sh` rerun needed.

## Deliberately still out of scope

Pairing this with adding real `tracing::debug!` instrumentation to
`cmd_export`'s own actual steps (wrapper path chosen, sudo/extra-
flags applied, icon resolution, etc.) to make `--verbose` a genuine,
`xtrace`-equivalent behavior change remains a legitimate, separately-
scoped future candidate — deliberately not bundled into this same
increment, which stays a pure, narrow CLI-compatibility fix.
