# Design note 0479: `ociman tag` multi-target support

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_tag.rs`.

## What this closes

`0478`'s own "still out of scope" section flagged this directly:
`ociman tag`/`ociman image tag` only ever accepted a single target,
while real `docker`/`podman tag` accept `TARGET_NAME [TARGET_NAME...]`
— one or more targets in a single call.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/images/tag.go:14-21`: `Use: "tag IMAGE
  TARGET_NAME [TARGET_NAME...]"`, `Args: cobra.MinimumNArgs(2)` —
  `IMAGE` plus at least one target.
- `~/git/podman/pkg/domain/infra/abi/images.go:445-457`
  (`ImageEngine.Tag`): a real, plain sequential loop — `for _, tag :=
  range tags { if err := image.Tag(tag); err != nil { return err } }`
  — applying each target one at a time, in the order given. Critically,
  a *later* target failing leaves every *earlier*, already-successful
  one in place rather than rolling the whole call back to a clean
  slate.
- The top-level CLI wrapper (`tag.go:46-48`) prints nothing on
  success at all — this project's own existing, deliberate divergence
  (printing each tagged target) predates this increment and is
  preserved unchanged, just extended to print once per target instead
  of once total.

## Implementation

- `Command::Tag::target: String` → `targets: Vec<String>`
  (`#[arg(required = true)]`, matching real podman's own
  `MinimumNArgs(2)` requirement of at least one target beyond
  `IMAGE`); `ImageCommand::Tag` (`0478`'s own alias) mirrored
  identically.
- `cmd_tag(source_str: &str, target_strs: &[String], json: bool)`:
  resolves `source` once, then loops over `target_strs` in order,
  parsing and `store.put_image`-ing each — matching real podman's own
  exact sequential, stop-at-first-failure, never-roll-back semantics.
  `--json` follows this project's own already-established single/
  array convention (`cmd_rmi`'s own `RmiOutcome`/`RmiResult` pair,
  `0102`-era): a lone target still prints its own bare `TagResult`
  object, two or more print a JSON array of them.

## Tests

Three new integration tests in `tests/tests/ociman_tag.rs`:
`tag_accepts_multiple_targets_in_one_call` (three targets in one
call, each resolving to the exact same manifest digest, one line
printed per target in order), `tag_with_a_later_invalid_target_
leaves_earlier_ones_tagged` (a deliberately-malformed second target
fails the whole call, but the first, valid one is still genuinely
tagged on disk afterward — proving the real "no rollback" semantics,
not just the CLI's own exit code), `tag_with_no_target_at_all_is_a_
clear_error`. All 15 tests in the file pass (12 prior + 3 new,
verified unmodified: the single-target case is just the one-element
edge of the same new code path).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (121 test-result
blocks, 0 failures on the first attempt), `python3 ci/guards.py`
(clean), `cargo deny check` (clean), `bash ci/native-ci.sh` (clean,
121/121 on the first attempt), `bash ci/build-deb.sh` (clean, real
`dpkg -i`/`--version`/`dpkg -r` round trip on the first attempt). No
benchmark re-run needed: `ociman tag` is not exercised by `ci/
bench.sh` at all, and tagging remains an infrequent, offline metadata
operation, not any startup/destroy-time hot path.

## Deliberately still out of scope

Real `podman untag`'s own equivalent shape (`IMAGE [IMAGE...]`, the
first argument resolving the target image, every later one an
explicit tag to remove) was already correctly ported with its own
full multi-reference support from the start (`0283`) — no equivalent
gap exists there to close.
