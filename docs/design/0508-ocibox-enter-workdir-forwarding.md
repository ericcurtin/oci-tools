# Design note 0508: `ocibox enter` real host-cwd forwarding, `--no-workdir`

Status: implemented
Scope: `bin/ocibox/src/main.rs`, `tests/tests/ocibox_enter.rs`.

## What this closes

Diversifying away from the just-completed, now-heavily-mined `ociman
container <verb>` alias series (`0488`-`0507`, 20 consecutive
increments) into a fresh area: `ocibox enter`'s own working-directory
resolution never read the real host's own current working directory
at all — every session always started at the box's own `$HOME` (or
the box's own declared `working_dir`, or `/`), regardless of which
real host directory the user actually ran `ocibox enter` from. Real
`distrobox enter` forwards the real host's own cwd by default
whenever it resolves to somewhere inside the box's own home
directory, with `--no-workdir`/`-nw` to opt back into the old,
home-only behavior.

## Real, checked-directly confirmation

- `~/git/distrobox/internal/cli/enter.go:58-62`: the real
  `--no-workdir`/`"nw"` alias flag, `Usage: "always start the
  container from container's home directory"`, default `false`.
- `~/git/distrobox/pkg/containermanager/containermanager.go:330-353`
  (`GetWorkDir`): the exact real resolution logic — `noWorkDir` wins
  outright (`return containerHome`); otherwise reads `os.Getwd()`; a
  host cwd genuinely inside `containerHome` is used verbatim; a host
  cwd *outside* it gets prefixed with `/run/host` instead (real
  distrobox's own whole-host bind mount, which this project has no
  equivalent of).
- `~/git/distrobox/pkg/containermanager/providers/podman.go:855-861`
  (`generateEnterCommand`): confirms the resolved `workdir` becomes
  both `--workdir=<dir>` *and* `--env=PWD=<dir>` unconditionally.
- `~/git/distrobox/pkg/commands/ephemeral.go:91`: confirms `ephemeral`
  never sets `NoWorkDir` on its own `EnterOptions{...}` construction
  at all — it gets the identical forwarding-by-default behavior for
  free, with no CLI flag of its own.

## Implementation

- `Command::Enter` gains a new `no_workdir: bool` field
  (`--no-workdir`), mirroring `clean_path`'s existing shape exactly.
- A new `resolve_workdir(no_workdir, cwd, home, fallback) -> String`
  function implements the real resolution logic directly: `cwd` and
  `home` are passed in as `Option<&Path>` (rather than calling
  `std::env::current_dir()` internally) purely so the branching logic
  itself is unit-testable without mutating this whole process's own
  actual working directory — the one real caller (`enter_spec`) passes
  `std::env::current_dir().ok()`.
- The case where the real host cwd is inside the box's own already-
  bind-mounted `$HOME` needs no new mount at all: `enter_spec` already
  unconditionally bind-mounts the whole resolved `home` directory
  (`rbind`), so any subdirectory of it is already visible inside the
  rootfs under the identical path. The `/run/host`-prefixed
  outside-`$HOME` case real distrobox handles via a *separate*,
  unconditional whole-host bind mount is deliberately not replicated
  here — an honestly narrower first slice, the same established
  precedent every other first-slice design note in this project
  already sets (falls back to the pre-existing home/`working_dir`/`/`
  chain instead in that case).
- `process.env`'s own `PWD=` entry is now always set to match the
  resolved `cwd`, matching real distrobox's own identical
  unconditional `--env=PWD=<workdir>` — a real, previously-missing
  environment variable `ocibox enter` never set at all before now
  (the secondary candidate this research turn's own recommendation
  flagged, folded into the same increment since it's a one-line
  addition reusing the exact same resolved value).
- `cmd_ephemeral`'s own call into `enter_and_get_exit_code` keeps
  passing `false` for the new `no_workdir` parameter (alongside the
  pre-existing `false` for `clean_path`), matching real `distrobox
  ephemeral`'s own checked-directly `EnterOptions{...}` construction
  exactly — no CLI flag of its own, always the default forwarding
  behavior.

## Tests

Six new unit tests for `resolve_workdir` (table-style, matching
`build_container_path`'s own existing pattern): `--no-workdir` always
wins; a real cwd inside home is forwarded verbatim; a cwd exactly
equal to home is forwarded; a cwd outside home falls back; no home at
all falls back; an unreadable cwd (`None`) falls back.

Two new integration tests in `tests/tests/ocibox_enter.rs`:
- `enter_forwards_the_real_hosts_own_current_working_directory_when_inside_home`
  — spawns `ocibox enter` from a real subdirectory of a fake `$HOME`
  and confirms both `pwd` and `$PWD` report that exact subdirectory
  inside the box.
- `enter_no_workdir_flag_starts_from_home_instead_of_the_real_cwd` —
  the identical setup, but with `--no-workdir` given, confirming the
  box starts at bare `$HOME` instead, restoring the pre-existing
  behavior exactly.

All pre-existing `ocibox` tests (66 across `ocibox_enter.rs`/
`ocibox_create.rs`/`ocibox_ephemeral.rs`/`ocibox_export.rs`/
`ocibox_generate_entry.rs`/`ocibox_list_rm.rs`) still pass unchanged
— none of them asserted the exact starting `cwd` before, so none
needed updating for the new default behavior.

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (122
test-result blocks, 0 failures — no new test file added, so the block
count is unchanged from `0507`; clean on the first attempt with
`RUST_TEST_THREADS=2`), `python3 ci/guards.py` (clean), `cargo deny
check` (clean), `bash ci/native-ci.sh` (clean on the first attempt,
also with `RUST_TEST_THREADS=2`), `bash ci/build-deb.sh` (clean on
the first attempt, real `dpkg -i`/`--version`/`dpkg -r` round trip).
No benchmark re-run needed: this change is confined to `ocibox
enter`'s own spec-construction path, never exercised by `ci/
bench.sh` (which only benchmarks `ocirun`/`ociman`).

## Deliberately still out of scope

- The `/run/host`-prefixed case for a host cwd genuinely outside the
  box's own `$HOME` — needs a whole *separate*, unconditional bind
  mount of the entire host filesystem this project's own `ocibox` has
  no equivalent of at all; a real, architecturally bigger gap than
  this increment's own narrower scope.
- `distrobox-host-exec` — needs a separately-downloaded `host-spawn`
  helper binary and runs *inside* the container, a different concern
  than `ocibox`'s own host-CLI surface addresses today.
</content>
