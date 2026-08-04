# Design note 0449: `ociman inspect --latest`/`-l`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_inspect.rs`.

## What this closes

`ociman inspect` had no `--latest`/`-l` flag at all — continuing the
same rollout `0434`-`0437`/`0443`-`0448` already established.

## Real, checked-directly confirmation

`~/git/podman/cmd/podman/inspect/inspect.go:36`: `validate.
AddLatestFlag` (registered by `AddInspectFlagSet`, shared by *both*
the top-level generic `podman inspect` and `podman container
inspect`) — the exact same flag/validation `ociman rm --latest`
(`0434`) already ports. Its own `inspect` function (lines ~76-95) has
a real, well-defined three-part shape, a genuine mix of the two
extremes this rollout has seen so far (`inspect --latest` behaves
like `exec`/`wait`/`top`/`logs`'s own real mutual-exclusivity checks,
*not* like `diff`'s own complete lack of one):

```go
if len(namesOrIDs) == 0 {
    if !i.options.Latest && !i.options.All {
        return errors.New("no names or ids specified")
    }
}
tmpType := i.options.Type
if i.options.Latest {
    if len(namesOrIDs) > 0 {
        return errors.New("--latest and arguments cannot be used together")
    }
    if i.options.Type == common.AllType {
        tmpType = common.ContainerType // -l works with --type=all, defaults to containertype
    }
}
```

And, in `newInspector` (line ~58): `if options.Type == common.
ImageType { if options.Latest { return ...("latest is not supported
for type %q", ...) } }`. Three real, checked-directly rules: (1)
`--latest` + an explicit reference is a real, immediate error; (2)
`--latest` + `--type image` is a real, immediate error (an image has
no "most recently created" concept `--latest` could mean); (3)
`--latest` with `--type` left at its own default (`all`) resolves as
if `--type container` had been given instead, never falling back to
an image the way a plain, latest-less `all` lookup otherwise would.
(`options.All`, referenced in the first check, is dead code in
practice — grepped the whole `inspect`/`container inspect`/top-level
`inspect` command tree: no CLI flag anywhere ever sets it, so it's
always `false`, and that check always reduces to just checking
`!Latest`.)

## Implementation

- `Command::Inspect::reference` widens from `String` to
  `Option<String>` (omittable when using `--latest`); new `latest:
  bool` (`#[arg(short = 'l', long)]`).
- The dispatch arm ports all three real rules above, in real podman's
  own exact wording: `--latest` + `--type image` errors first; then
  `--latest` + an explicit reference errors; then, with `--latest`
  given, resolves via `resolve_latest_container` and forces the
  effective type to `InspectType::Container` regardless of whatever
  `--type` itself was left at (matching real podman's own "only ever
  matters when `--type` genuinely is `image`, which the first check
  above already rejected" outcome for every other value); without
  `--latest`, the given `reference`/`inspect_type` are used exactly
  as before, with a real, immediate error if neither `--latest` nor a
  reference was given at all. `cmd_inspect`'s own signature is
  completely unchanged.

## Tests

Five new tests in `tests/tests/ociman_inspect.rs`: `inspect_latest_
shows_the_most_recently_created_container` (two containers with a
real, distinguishable creation-time gap; `inspect --latest` reports
only the newer one's own name/id, never the older one's),
`inspect_latest_combined_with_an_explicit_reference_is_a_clear_error`
(a real, deliberate contrast with `ociman diff --latest`, `0448`,
which has no such check at all), `inspect_latest_combined_with_type_
image_is_a_clear_error`, `inspect_with_no_reference_and_no_latest_is_
a_clear_error`, and `inspect_latest_on_an_empty_store_is_a_clear_
error`. All 26 prior tests in the file pass unmodified (31/31 total).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (120 test-result
blocks, 0 failures, clean on the first full run), `python3 ci/
guards.py`, `cargo deny check`, `bash ci/native-ci.sh` (clean,
120/120, clean on the first run too), `bash ci/build-deb.sh` (real
`dpkg -i`/`--version`/`dpkg -r` round trip). `ociman inspect` isn't
on any `ci/bench.sh`-measured hot path (confirmed by grep, same
finding `0442` already established) — no benchmark re-run needed.

## Deliberately still out of scope

Continuing this same rollout: `attach`, `stats`, `start`, `port`, and
`checkpoint`/`restore` (the last two CRIU-based, a much larger,
separately-scoped gap) still don't have `--latest` here at all — each
a natural, separate future increment.
