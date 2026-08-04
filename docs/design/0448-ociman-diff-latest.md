# Design note 0448: `ociman diff --latest`/`-l`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_diff.rs`.

## What this closes

`ociman diff` had no `--latest`/`-l` flag at all — continuing the
same rollout `0434`-`0437`/`0443`-`0447` already established.

## Real, checked-directly confirmation

`~/git/podman/cmd/podman/containers/diff.go:43`: `validate.
AddLatestFlag` — the exact same flag/validation `ociman rm --latest`
(`0434`) already ports. A real, checked-directly divergence from
*every other sibling* in this rollout so far, worth citing verbatim
(`diffRun`, line ~47, and the shared `Diff` abi function, `~/git/
podman/pkg/domain/infra/abi/containers.go:1159-1177`):

```go
// diffRun:
if len(args) == 0 && !diffOpts.Latest {
    return errors.New("container must be specified: podman container diff [options [...]] ID-NAME")
}

// abi Diff:
if opts.Latest {
    ctnr, err := ic.Libpod.GetLatestContainer()
    base = ctnr.ID()
}
if len(namesOrIDs) > 0 {
    base = namesOrIDs[0]  // unconditionally overwrites the latest-resolved value
    ...
}
```

There is **no mutual-exclusivity check at all** between `--latest`
and an explicit `ID` — an explicit one always silently wins outright
(the latest-resolved value is simply overwritten, never even
compared or rejected). Real podman's own `Diff` also accepts an
optional second positional (`PARENT`, compared against instead of
the container's own base image) — a separate, genuinely richer
feature `ociman diff` has never implemented at all (it always
compares against its own recorded base snapshot); deliberately not
added here either, kept as a clearly separate future increment rather
than folded into this one.

## Implementation

- `Command::Diff::id` widens from `String` to `Option<String>`
  (omittable when using `--latest`); new `latest: bool`
  (`#[arg(short = 'l', long)]`).
- The dispatch arm mirrors real podman's own exact precedence: an
  explicit `id`, when given, is always used verbatim, regardless of
  `--latest` (ported faithfully — no stricter mutual-exclusivity
  check added that real podman itself doesn't have); with no `id`,
  `--latest` resolves via `resolve_latest_container`; with neither,
  a real, immediate error (`"container must be specified: ociman diff
  [options] ID-NAME"`, adapted from real podman's own exact wording
  to this project's own binary/subcommand name). `cmd_diff`'s own
  signature is completely unchanged.

## Tests

Four new tests in `tests/tests/ociman_diff.rs`: `diff_latest_shows_
the_most_recently_created_containers_own_diff` (two independent
stopped containers with a real, distinguishable creation-time gap,
each with a genuinely different file change; `diff --latest` shows
only the newer one's own change, never the older one's — plus a
sanity check that the older one's own explicit id still resolves to
its own, genuinely different diff, proving the two containers are
truly independent, not accidentally merged), `diff_explicit_id_
silently_wins_over_latest_when_both_given` (a direct, convincing
proof of the real "no mutual exclusivity, explicit always wins" quirk
above — `--latest` would resolve to the newer container, but the
explicit older id given alongside it wins instead), `diff_with_no_
id_and_no_latest_is_a_clear_error`, and `diff_latest_on_an_empty_
store_is_a_clear_error`. All 8 prior tests in the file pass unmodified
(12/12 total).

A real, if minor, test-authoring pitfall caught while writing the
first new test: `seed_and_run_stopped_container`'s own internal id
lookup (`ps -a -q`) assumes exactly one container exists in the
store at call time — calling it a *second* time against the same
`storage_dir` (needed here, to get two independent containers) means
its own returned "id" for that second call is actually two ids
concatenated by a literal embedded newline. Worked around by simply
not using that second call's own return value at all (the test never
actually needed it).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (120 test-result
blocks, 0 failures, clean on the first full run), `python3 ci/
guards.py`, `cargo deny check`, `bash ci/native-ci.sh` (clean,
120/120, clean on the first run too), `bash ci/build-deb.sh` (real
`dpkg -i`/`--version`/`dpkg -r` round trip). Touches only `ociman
diff`'s own selection logic, not any hot path at all — no benchmark
re-run needed.

## Deliberately still out of scope

Real podman's own optional second `PARENT` positional (comparing a
container against an arbitrary reference instead of its own recorded
base image) — a separate, genuinely richer feature, not something
this increment's own narrower `--latest` addition needed to also
solve. Continuing this same rollout otherwise: `attach`, `inspect`,
`stats`, `start`, `port`, and `checkpoint`/`restore` still don't have
`--latest` here at all — each a natural, separate future increment.
