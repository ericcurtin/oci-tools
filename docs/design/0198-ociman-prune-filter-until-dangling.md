# Design note 0198: `ociman prune --filter until=`/`dangling=`

Status: implemented
Scope: `bin/ociman/src/main.rs` (`PruneFilters`, `parse_simple_duration`,
`parse_prune_filters`'s new `until=`/`dangling=` branches, `cmd_prune`'s
new `dangling_only`/`until` checks); `tests/tests/ociman_prune.rs`.

## Continuing milestone 4

0197's own "what this doesn't do yet" named `ociman prune --filter
until=`/`dangling=` as the natural next step, now unblocked by that
turn's own fix (a built image's own `created` field finally reflects
real build time). Both keys were already named as real, unimplemented
gaps in `Command::Prune`'s own doc comment since 0192.

## Real, checked-directly semantics

### `until=<duration-or-timestamp>`

Checked directly against a real installed `podman image prune`/
`docker image prune`, cross-referenced with `~/git/moby/daemon/images/
image_prune.go`'s own `getUntilFromPruneFilters`/its use:

* `until = reference_time.Add(-duration)` for a plain duration string
  (e.g. `until=1h` → threshold = now minus one hour), or the absolute
  timestamp itself for an RFC3339 value.
* An image is *kept* (not removed) if its own `created` is missing
  entirely, **or** strictly *after* the threshold; only an image whose
  own `created` is at or before the threshold actually gets removed.
* Confirmed directly, three isolated scenarios: a freshly built
  dangling image survives `--filter until=1h` (created only seconds
  ago, well after the threshold); the same image, after a real 2-second
  sleep, is removed by `--filter until=1s` (now genuinely older than
  the threshold); a real dangling image built moments ago is also
  removed by `until=1h` if its own `created` happens to be *inherited*
  from a much older base (this project's own `record_layer`/0197 fix
  means this specific inherited-timestamp gotcha mostly only shows up
  for a bare `FROM` with no real instructions — the far more common
  case of an actual `RUN`/`COPY`/`LABEL`-driven build always gets a
  real, current `created` now).

### `dangling=true`/`dangling=false`

Checked directly against a real installed `podman image prune`:
* `--filter dangling=true` **always** restricts removal to dangling
  (untagged) images only, *even with* `--all` — confirmed directly:
  `podman image prune --all --filter dangling=true` still leaves a
  real, unused, tagged image completely alone.
* `--filter dangling=false` **always** expands removal to every unused
  image regardless of tag, *even without* `--all` — confirmed
  directly: `podman image prune --filter dangling=false` (no `-a` at
  all) removed a real tagged, unused `busybox:latest` outright.
* So an explicit `dangling=` filter value always wins outright over
  whatever `--all`/no-`--all` would otherwise decide on its own.
* Giving both `dangling=true` and `dangling=false` together is a clear
  error — matches real docker's own identical refusal
  (`~/git/moby/daemon/internal/filters/parse.go`'s own
  `GetBoolOrDefault`: "conflicting truthy/falsy value").

### Filter combination (unchanged from 0192)

Different filter *keys* still AND together (an image must satisfy
every one given); only repeated values of the *same* key (`label=`)
still OR. `until=`/`dangling=` each only ever take one active value
(an explicit conflict, or more than one `until=`, is a clear error —
matching real docker's own identical refusal for both), so this
doesn't add any new same-key-OR case of its own.

## The fix

* `LabelFilter` unchanged; a new `PruneFilters` struct now holds it
  alongside an `Option<SystemTime>` (`until`, already resolved to the
  real absolute threshold time) and an `Option<bool>` (`dangling`).
* A new, deliberately narrow `parse_simple_duration`: a Go-`time.
  ParseDuration`-*like* parser for `<number><unit>` pairs
  (`h`/`m`/`s` only, fractional amounts allowed, combinable like
  `1h30m`) — not every unit real Go's own parser accepts (`ns`/`us`/
  `µs`/`ms`/a leading sign aren't), a clear parse error for anything
  it doesn't understand rather than a silently-wrong duration. An
  `until=` value that doesn't parse as a duration falls back to
  `oci_spec_types::time::parse_rfc3339_utc` (already existing, reused
  as-is) before erroring.
* `parse_prune_filters` now dispatches on `label=`/`label!=`/`until=`/
  `dangling=`, erroring for a still-genuinely-unsupported key (e.g.
  `reference=`) exactly as before.
* `cmd_prune`: `dangling_only = filters.dangling.unwrap_or(!all)` —
  the one place `--all` and the new filter actually meet, the filter
  always overriding when given. The per-image loop fetches `image_
  config` whenever *either* `labels` or `until` is active (previously
  only for `labels`), checking `until` first (skip/keep if `created`
  is missing or after the threshold) before the existing label check.

## Tests

Ten new integration tests in `tests/tests/ociman_prune.rs`: `until=`
duration keeping a fresh image, `until=` duration removing a genuinely
older one (via a real 2-second sleep), `until=` accepting an RFC3339
timestamp, an invalid `until=` value erroring, more than one `until=`
erroring, `dangling=true` overriding `--all`, `dangling=false`
overriding no-`--all`, an invalid `dangling=` value erroring, and
conflicting `dangling=true`+`dangling=false` erroring. One pre-existing
test (`prune_filter_unsupported_key_is_a_clear_error`) used `until=24h`
as its own "still unsupported" example — updated to `reference=foo`,
still genuinely unsupported. All other 15 pre-existing `ociman prune`
tests continue to pass unchanged (25 total now).

Full `cargo build --workspace --locked`/`cargo test --workspace
--locked` (2 clean runs, 83/83 result blocks)/`cargo fmt --all
--check`/`cargo clippy --workspace --all-targets --locked -- -D
warnings`/`python3 ci/guards.py`/`cargo deny check`/`bash
ci/native-ci.sh` all clean. No performance regression (`ociman prune`
on an empty store: sub-millisecond, unaffected — this command was
never on the startup/destroy-time critical path this project
benchmarks most closely).

## What this doesn't do yet

Real `reference=<pattern>` (matching a name/tag glob) remains
unimplemented, still a clear error rather than silently ignored.
`--timestamp` (reproducible `ociman build` output) and the larger
`RUN --mount=`/heredoc/multi-platform gaps named by the earlier
milestone-4 survey also remain open.
