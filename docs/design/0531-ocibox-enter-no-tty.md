# Design note 0531: `ocibox enter --no-tty`/`-T`/`-H`

Status: implemented
Scope: `bin/ocibox/src/main.rs`, `tests/tests/ocibox_enter.rs`.

## What this closes

Real `distrobox enter` supports `--no-tty` (aliases `-T`, `-H`),
"do not instantiate a tty". `ocibox enter` had no equivalent at all —
a real CLI flag it would reject as unrecognized.

## Real, checked-directly confirmation

- `~/git/distrobox/internal/cli/enter.go:58-63`:
  ```go
  &cli.BoolFlag{
      Name:    "no-tty",
      Aliases: []string{"T", "H"},
      Usage:   "do not instantiate a tty",
  },
  ```
- `~/git/distrobox/pkg/containermanager/providers/podman.go:849-853`
  (`generateEnterCommand`, `docker.go` has the identical shape at its
  own equivalent call site):
  ```go
  if !noTTY && containermanager.IsTTY() {
      cmd = append(cmd, "--tty")
  }
  ```
  This is the *entire* real effect for this project's own purposes:
  suppressing a real `--tty` real distrobox would otherwise append to
  the generated `podman exec`/`docker exec` invocation, and only in
  combination with its own `IsTTY()` auto-detection.
- `~/git/distrobox/pkg/containermanager/containermanager.go:355-374`
  (`BuildCommandArgs`): `noTTY`'s only *other* real effect is dropping
  `su`'s own `--pty` flag under `--unshare-groups` mode — checked
  directly, confirmed this project's `ocibox` has no `--unshare-*`
  concept of any kind (`grep -n "unshare_groups\|UnshareGroups"
  bin/ocibox/src/main.rs` — no matches), so this second effect is
  moot here too.
- This project's own `ocibox enter` never allocates a PTY at all
  regardless of any flag — a real, already-documented, project-wide
  gap (`docs/design/0207`, the same one `ociman run`'s own missing
  `-t`/`--tty` already has). Since the thing `--no-tty` disables never
  happens here in the first place, this is a genuine, faithful no-op
  — the same reasoning class `Command::Enter::yes` (`0522`) and
  `Command::Rm`/`Command::Stop`'s own `--yes` already established.

## Implementation

`bin/ocibox/src/main.rs`: new `Command::Enter::no_tty: bool`,
`#[arg(long = "no-tty", short = 'T', short_alias = 'H')]` — clap's
`short_alias` builder attribute (confirmed to work via `#[arg(...)]`
derive; not previously used anywhere else in this codebase) accepts
real distrobox's own *second* short alias directly, unlike the
"single short alias only" limitation previously documented for other
flags — no compatibility gap here after all. Accepted and immediately
discarded (`no_tty: _`) at the one dispatch site; `cmd_enter`'s own
signature is untouched.

## Tests

One new integration test in `tests/tests/ocibox_enter.rs`:
`enter_no_tty_flag_and_both_real_aliases_are_accepted_and_behave_identically`
— proves `--no-tty`, `-T`, and `-H` all parse and produce byte-
identical real output (a real command's own real stdout) versus the
same invocation with no flag at all.

Manually exercised end to end beyond the automated test: a real image
built via `ociman build` from a `FROM scratch` + bundled busybox
Containerfile, `ocibox create`, then `ocibox enter` with `--no-tty`,
`-T`, `-H`, and no flag at all, each running the identical real
command and producing identical real output.

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean after one auto-fix), `cargo clippy --workspace
--all-targets --locked -- -D warnings` (clean), a full `cargo test
--workspace --locked` run (124 test-result blocks, 0 failures, 0
`FAILED` lines — fully clean on the first attempt), `python3
ci/guards.py` (clean), `cargo deny check` (clean), `bash
ci/native-ci.sh` (failed twice on its own internal `cargo test`,
both times a single, different `ocicri_container.rs` test —
`create_container_masked_paths_genuinely_masks_a_real_file_inside_the_
running_container` then
`create_container_bind_mount_follows_a_symlinked_host_path` —
confirmed transient by isolated rerun both times, then a fully clean
run with `RUST_TEST_THREADS=2` on the third attempt, the same
concurrent-`opencode`-session-driven environmental contention
`0527`-`0530` already documented, this host currently had 3-4 other
genuinely concurrent sessions active), `bash ci/build-deb.sh` (clean
on the first attempt, real `dpkg -i`/`--version`/`dpkg -r` round trip
for every binary). Pure CLI-parsing-and-discard addition — no hot
path touched, no `ci/bench.sh` rerun needed.

## Deliberately still out of scope

Real PTY allocation for `ocibox enter` (and every other command in
this project) remains the pre-existing, project-wide gap this flag's
own real effect would otherwise interact with — unrelated to and
unaffected by this increment.
