# Design note 0549: `ocibox create`/`ocibox ephemeral --absolutely-disable-root-password-i-am-really-positively-sure`

Status: implemented
Scope: `bin/ocibox/src/main.rs`, `tests/tests/ocibox_create.rs`,
`tests/tests/ocibox_ephemeral.rs`.

## What this closes

Adds `--absolutely-disable-root-password-i-am-really-positively-sure`
(no short alias, matching real distrobox — it has none either) to
`ocibox create` and `ocibox ephemeral` — a real, previously-unnoticed
gap: `grep -rn "absolutely-disable-root-password\|nopasswd" docs/
design/*.md README.md bin/ocibox/src/main.rs` found zero hits
anywhere before this increment.

## Real, checked-directly confirmation

- Flag declaration: `~/git/distrobox/internal/cli/create.go:170-173`
  — `cli.BoolFlag{Name: "absolutely-disable-root-password-i-am-really-
  positively-sure", Usage: "⚠️ ⚠️ when setting up a rootful distrobox,
  this will skip user password setup, leaving it blank. ⚠️ ⚠️"}`.
- Wired into `CreateOptions.Nopasswd`: `create.go:216`. Inherited by
  `ephemeral` too — checked directly, `~/git/distrobox/internal/cli/
  ephemeral.go:22-24`'s own `ignoredFlags` list only ever strips
  `"compatibility"`/`"no-entry"`, never this one, and `ephemeral.go:94`
  wires it into `Nopasswd` again.
- Real, live-consumed effect: `~/git/distrobox/pkg/containermanager/
  providers/podman.go:379-381` (and the identical `docker.go:391-393`)
  — when true, appends `--volume /dev/null:/run/.nopasswd:ro` to the
  generated container-create command.
- The marker's own one real consumer: `~/git/distrobox/internal/
  inside-distrobox/assets/distrobox-init:234-246` — checked only as
  part of a rootful-vs-rootless detection heuristic (`stat`-ing a
  bind-mounted *real host* `/run/host/etc/shadow` as uid 0, i.e.
  genuine host root access), itself only reachable at all in real
  distrobox's own genuinely rootful mode.

## Real, faithful no-op (checked directly, not assumed by analogy)

The only thing this flag ever does upstream is mount a marker file
that's read by a heuristic which is itself only reachable when the
container can read the real host's own `/etc/shadow` as uid 0 — i.e.
only in rootful mode. This project's own `ocibox` has already
established (`0540`, `--root`) that it has no rootful/rootless
distinction of any kind at all: every box is always the rootless case.
Checked directly (not merely inferred from the `--root` precedent):
`ocibox` never bind-mounts anything resembling `/run/host/etc/shadow`
and never runs any `distrobox-init`-equivalent script inside its own
containers at all — there is no code path here that could ever
consume a `/run/.nopasswd` marker in the first place, even
independently of the rootful/rootless framing. Accepting-and-ignoring
this flag is therefore a genuinely faithful port of upstream's own
real, checked-directly-narrow effect, not a shortcut.

## Implementation

`bin/ocibox/src/main.rs`: `absolutely_disable_root_password_i_am_
really_positively_sure: bool`
(`#[arg(long = "absolutely-disable-root-password-i-am-really-
positively-sure")]`) added to `Command::Create` (full doc comment
citing the above) and `Command::Ephemeral` (cross-referencing it,
matching the `--root`/`0540` cross-referencing convention). Accepted
and immediately discarded at both dispatch sites — `cmd_create`/
`cmd_ephemeral`'s own signatures are untouched, the same "nothing to
skip" convention `--root` and `ociman commit --quiet` (`0523`) already
established for a genuine, total no-op.

## Tests

Two new tests, one per command, following the exact same
"accepted and behaves identically" shape `--root`'s own tests already
established:

- `create_nopasswd_flag_is_accepted_and_behaves_identically`
  (`tests/tests/ocibox_create.rs`)
- `ephemeral_nopasswd_flag_is_accepted_and_behaves_identically`
  (`tests/tests/ocibox_ephemeral.rs`)

Manually exercised beyond the automated tests: `ocibox create --image
... --name nopwbox --absolutely-disable-root-password-i-am-really-
positively-sure` (succeeds, box created normally), `ocibox enter
nopwbox /bin/echo hi` (still works normally afterward), and `ocibox
ephemeral --image ... --absolutely-disable-root-password-i-am-really-
positively-sure /bin/echo hi-ephemeral` (succeeds identically);
confirmed `--help` on both commands renders the new flag correctly.

## Verification

`cargo build --workspace --locked` (clean), `cargo fmt --all` (clean,
no changes needed for the new tests), `cargo clippy --workspace
--all-targets --locked -- -D warnings` (clean), targeted
`ocibox_create.rs`/`ocibox_ephemeral.rs` runs (15/15, 12/12), a full
`cargo test --workspace --locked` run (clean), `python3 ci/guards.py`
(clean), `cargo deny check` (clean), `bash ci/native-ci.sh` (clean),
`bash ci/build-deb.sh` (clean, real `dpkg -i`/`--version`/`dpkg -r`
round trip). Pure CLI-parsing-and-discard addition — no hot path
touched, no `ci/bench.sh` rerun needed.
