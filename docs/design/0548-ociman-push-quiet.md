# Design note 0548: `ociman push --quiet`/`-q`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_tls_verify.rs`.

## What this closes

Adds `--quiet`/`-q` to `ociman push` and its `ociman image push` alias
— the one sibling command in the `pull`/`save`/`load`/`push` family
that had never been given the flag, even though `cmd_push` already has
a real progress spinner to suppress (unlike `ociman commit --quiet`,
`0523`, which is a genuine no-op because `cmd_commit` has no progress
writer at all).

## Real, checked-directly confirmation

- Flag definition: `~/git/podman/cmd/podman/images/push.go:110` —
  `flags.BoolVarP(&pushOptions.Quiet, "quiet", "q", false, "Suppress
  output information when pushing images")`.
- Real consumption: `push.go:180-182` — `if !pushOptions.Quiet {
  pushOptions.Writer = os.Stderr }`; and again at the engine layer,
  `~/git/podman/pkg/domain/infra/abi/images.go:419-421` — `if
  !options.Quiet && pushOptions.Writer == nil { pushOptions.Writer =
  os.Stderr }`, gating the actual libimage copy-progress writer.
- **Checked directly whether this is one of real podman's own
  hidden/inert flags first** (this project's own established practice
  before porting *any* flag, especially one right next to a
  `MarkHidden` call in the same file): `push.go:136-144` shows
  `_ = flags.MarkHidden("quiet")`, but only `if registry.IsRemote()` —
  the `podman-remote`/API-socket client mode, a client/server split
  this project has no equivalent of at all (`ociman` is always
  "local"). In the normal mode this project actually emulates,
  `--quiet` is a real, plainly visible flag — confirmed live against a
  real installed `podman push --help` on this host too.

## Real, functional gap — not a no-op

`cmd_push` already draws a real `indicatif` spinner ("pushing `<ref>`
[elapsed]") via `oci_cli_common::progress::spinner`, unconditionally,
with no `quiet` parameter to gate it — the exact same class of
progress-writer this project's own `pull`/`save`/`load --quiet`
already correctly suppress via the shared `progress::
spinner_unless_quiet` helper (`0417`-era). This is a small, mechanical
gap: push was simply the one command in this family never given the
flag at all, not an inapplicable concept.

## Implementation

`bin/ociman/src/main.rs`: `quiet: bool` (`#[arg(short, long)]`) added
to `Command::Push` and `ImageCommand::Push`. `cmd_push` gained a
`quiet: bool` parameter, and its one `progress::spinner(...)` call
site was swapped for `progress::spinner_unless_quiet(quiet, ...)` —
the identical, already-proven-safe pattern this exact helper already
serves four other call sites with.

## Tests

`push_quiet_still_pushes_correctly_and_reports_the_same_digest`
(`tests/tests/ociman_tls_verify.rs`, reusing that file's own existing
`MockPushRegistry`/`start_mock_with_a_real_image` helpers already
built for `push --tls-verify=false`'s own end-to-end test): a real
pull, tag, and `push --quiet` against a real, local, plain-HTTP mock
registry, asserting the pushed digest is identical to what an
unflagged push would report — the same "accepted, still produces
correct output" test shape `ociman_save.rs`'s own
`save_quiet_still_writes_a_correct_archive` already established (the
spinner itself draws only to stderr, and is already automatically
hidden whenever stderr isn't a real terminal, same established
limitation every other spinner-backed command's own test already
notes).

Manually exercised beyond the automated tests: `ociman push --help`/
`ociman image push --help` render the new flag correctly; a real push
attempt against an unreachable host produces the identical error text
with and without `--quiet`.

## Verification

`cargo build --workspace --locked` (clean), `cargo fmt --all` (clean,
no changes needed for the test edit), `cargo clippy --workspace
--all-targets --locked -- -D warnings` (clean), targeted
`ociman_tls_verify.rs`/`ociman_push.rs`/`ociman_container.rs` runs
(12/12, 4/4, 12/12), a full `cargo test --workspace --locked` run
(clean), `python3 ci/guards.py` (clean), `cargo deny check` (clean),
`bash ci/native-ci.sh` (clean), `bash ci/build-deb.sh` (clean, real
`dpkg -i`/`--version`/`dpkg -r` round trip). Pure CLI-parsing plus
reuse of the already-tested `spinner_unless_quiet` helper — no new
hot path, no `ci/bench.sh` rerun needed.
