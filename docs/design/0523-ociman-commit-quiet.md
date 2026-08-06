# Design note 0523: `ociman commit --quiet`/`-q`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_commit.rs`.

## What this closes

`ContainerCommand::Commit`'s own doc comment already flagged
`--quiet` by name as the next open gap on this command ("no
`--config`/`--format`/`--quiet`/`--include-volumes`, matching the
top-level command's own identical gap") -- this note closes exactly
that one, on both the top-level `ociman commit` and its `container
commit` alias together.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/commit.go:81`: `flags.
  BoolVarP(&commitOptions.Quiet, "quiet", "q", false, "Suppress
  output")`.
- `~/git/podman/cmd/podman/containers/commit.go:104-106`: `if
  !commitOptions.Quiet { commitOptions.Writer = os.Stderr }` --
  `--quiet`'s only real effect is suppressing a real buildah
  progress-writer.
- `~/git/podman/cmd/podman/containers/commit.go:120`: `fmt.Println
  (response.Id)` happens completely unconditionally, quiet or not.

Traced this project's own entire commit path (`cmd_commit`/
`commit_inner`) end to end: there is no progress-writer/spinner
anywhere in it at all -- only two `println!` calls total, both
unconditional (the final digest, and an optional "tagged: ..." line
when a real tag was also given). A genuine, faithful no-op, the same
"nothing to skip" reasoning class `0512`-`0522` already established,
checked directly rather than assumed.

## Implementation

`quiet: bool` (`#[arg(short, long)]`) added to `Command::Commit` and
its byte-identical `ContainerCommand::Commit` alias, each accepted
and immediately discarded (`quiet: _`) at its own dispatch site.
`cmd_commit`'s own function signature is untouched.

## Tests

Two new integration tests in `tests/tests/ociman_commit.rs`:
`commit_quiet_flag_is_accepted_and_behaves_identically` and
`container_commit_quiet_flag_works_through_the_alias` -- each proving
a real commit's own digest is still printed identically with the
flag given.

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (123
test-result blocks -- no new test file added, so the block count is
unchanged from `0522`; the documented transient `ocicri_container.rs`
flakiness under this host's own persistent CPU contention (plus a
second, genuinely concurrent process observed this session) showed
up once during `native-ci.sh`, confirmed transient by rerunning the
specific failing test in isolation -- passed -- then a clean
full-suite rerun), `python3 ci/guards.py` (clean), `cargo deny check`
(clean), `bash ci/native-ci.sh` (clean on the second attempt with
`RUST_TEST_THREADS=2`), `bash ci/build-deb.sh` (clean on the first
attempt, real `dpkg -i`/`--version`/`dpkg -r` round trip). Pure
CLI-parsing addition -- no hot path touched, no `ci/bench.sh` rerun
needed.

## Deliberately still out of scope

`--config`/`--format`/`--include-volumes` remain `Command::Commit`'s
own last three open gaps. `ociman build --unsetannotation` (a likely
faithful no-op, same class as this note, masking a real, separate,
bigger pre-existing gap: this project never inherits base-manifest
annotations at all) and `ocibox export --sudo` (real and tractable,
but needs genuine priority-detection logic, not a pure no-op) remain
open candidates for future increments.
</content>
