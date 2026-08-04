# Design note 0437: `ociman pause`/`unpause --latest`/`-l`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_pause.rs`,
`README.md`.

## What this closes

Neither `ociman pause` nor `ociman unpause` had a `--latest`/`-l`
flag. This is the fourth and fifth of the five sibling commands real
podman offers the identical flag on (`rm`/`stop`/`restart`/`pause`/
`unpause`), closing out the deliberately one-command-per-note rollout
`0434`/`0435`/`0436` already committed to.

## Real, checked-directly confirmation

`~/git/podman/cmd/podman/containers/pause.go:79,86` and
`unpause.go:77,84`: both call `validate.AddLatestFlag` — the exact
same flag/validation `ociman rm --latest` (`0434`) already ports,
reused verbatim (`GetLatestContainer`'s own semantics, `validate.
CheckAllLatestAndIDFile`'s own mutual-exclusivity matrix — see
`0434`'s own design note for the full citation, not repeated here).

A real, deliberate divergence carries over from `0422`
(`pause`/`unpause --filter`), and had to be re-checked directly for
`--latest` specifically before writing any code: real podman's own
`ContainerPause`/`ContainerUnpause` (`~/git/podman/pkg/domain/infra/
abi/containers.go`) only ever tolerate a not-actually-eligible match
(`ErrCtrStateInvalid`, silently skipped) when `options.All` is set —
that gate is `err != nil && options.All && errors.Is(err,
ErrCtrStateInvalid)`, checked directly, never conditioned on
`options.Latest`. So `--latest` must behave exactly like `--filter`
already does here: a resolved latest container that isn't currently
in the right state is a real, reported error, not a silent skip.

## Implementation

- `Command::Pause`/`Command::Unpause` each gain `latest: bool`
  (`#[arg(short = 'l', long)]`), with doc comments citing the exact
  divergence above.
- `cmd_pause`/`cmd_unpause` each gain a `latest: bool` parameter,
  threaded straight through to the shared `cmd_pause_or_unpause`.
- `cmd_pause_or_unpause` gains a mutual-exclusivity check for
  `--latest` against an explicit id, `--cidfile`, `--all`, and
  `--filter` (matching the exact shape its existing `--filter` check
  already has), then — unlike `stop`/`restart`'s own simpler "merge
  into the cidfile-merged `ids` and let the existing paths handle it"
  shape — merges the resolved single id into that same `ids: Vec<
  String>` right after the cidfile merge too. This still works out
  to the correct, non-tolerant semantics with no extra branching:
  the merged single-id list only ever flows into the pre-existing
  single-target (`[id] => ...`) path below, which was already a
  real, reported error on a wrong-state container before this
  change — the `--all`-only tolerant-skip loop is a separate branch
  entirely, never reached by a `--latest` call at all. This
  naturally reuses `resolve_latest_container` (introduced in `0434`
  explicitly as shared infrastructure for exactly this rollout)
  unchanged.

## Tests

Four new tests in `tests/tests/ociman_pause.rs` (where this project's
own existing `pause`/`unpause` test suite already lives):
`pause_and_unpause_latest_act_only_on_the_most_recently_created_container`
(two genuinely running containers with a real, distinguishable
creation-time gap; `pause --latest` freezes only the newer one, the
older stays running; a follow-up `unpause --latest` then thaws the
same newer one, still the latest by creation time), `pause_latest_
on_a_non_running_match_is_a_real_error_unlike_all` (a never-started
latest container is a real, reported error, proving no `--all`-style
tolerant skip — mirrors `0422`'s own `pause_filter_on_a_non_running_
match_is_a_real_error_unlike_all`), `pause_and_unpause_latest_on_an_
empty_store_is_a_clear_error`, and `pause_and_unpause_latest_
combined_with_anything_else_is_a_clear_error` (against `--all`, an
explicit id, and `--filter`, for both commands). All 11 prior tests
in `ociman_pause.rs` continue to pass unmodified (15/15 total).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean, no diff), `cargo clippy --workspace --all-targets --locked --
-D warnings` (clean), `cargo test --workspace --locked` (119
test-result blocks, 0 failures, clean on the first full run),
`python3 ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh`
(clean, 119/119), `bash ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip). Touches only `ociman pause`/`unpause`'s own
selection logic, not any hot path at all — no benchmark re-run
needed.

## Deliberately still out of scope

This closes the entire `--latest`/`-l` rollout across all five real
podman sibling commands (`rm`/`stop`/`restart`/`pause`/`unpause`) —
no further commands in this specific family remain. `ociman images
--sort` remains a separately documented, not-yet-started candidate
for a future increment.
