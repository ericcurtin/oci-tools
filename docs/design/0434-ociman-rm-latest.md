# Design note 0434: `ociman rm --latest`/`-l`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_ps.rs`,
`README.md`.

## What this closes

`ociman rm` had no `--latest`/`-l` flag at all — real `podman rm
--latest` acts on the single, most-recently-created container
without needing to name it explicitly, a real, common interactive
convenience (`podman run -d ... && podman logs --latest` style
workflows). This closes that gap for `rm` — the first of several
sibling commands real podman offers the identical flag on.

## Real, checked-directly confirmation

`~/git/podman/cmd/podman/validate/latest.go:8-14`: `AddLatestFlag`
registers `-l`/`--latest`. `~/git/podman/libpod/runtime_ctr.go:
1313-1330`, `GetLatestContainer`:

```go
func (r *Runtime) GetLatestContainer() (*Container, error) {
    ctrs, err := r.GetAllContainers()
    ...
    for containerIndex, ctr := range ctrs {
        createdTime := ctr.config.CreatedTime
        if createdTime.After(lastCreatedTime) {
            lastCreatedTime = createdTime
            lastCreatedIndex = containerIndex
        }
    }
    return ctrs[lastCreatedIndex], nil
}
```

Confirmed directly: considers *every* container regardless of state
(never just running ones — a `Created`, never-started one still
counts), compared by real creation time, and a real, immediate error
(`ErrNoSuchCtr`) if none exist at all.
`~/git/podman/cmd/podman/validate/args.go`'s own `CheckAllLatestAndIDFile`
confirms the exact real mutual-exclusivity matrix: `--latest` cannot
be combined with an explicit id, `--all`, `--cidfile`, or (via the
early `--filter` return) `--filter` either.

## Implementation

- New shared `resolve_latest_container(containers: &StateStore) ->
  anyhow::Result<String>` — `containers.list()?.into_iter().
  max_by_key(|state| parse_rfc3339_utc(&state.created)...)`, a
  one-line real port of `GetLatestContainer`'s own logic, explicitly
  documented as shared infrastructure for every future `--latest`
  this project grows (`stop`/`restart`/`pause`/`unpause`), so a
  future sibling's own resolution can never silently drift from this
  one.
- `Command::Rm` gains `latest: bool` (`#[arg(short = 'l', long)]`).
- `cmd_rm` gains a `latest: bool` parameter and the same mutual-
  exclusivity check its `--filter`/`--all`/`--cidfile` siblings
  already have; when set, resolves the single id via `resolve_
  latest_container` and flows through the exact same single-target
  `remove_container` call the rest of `cmd_rm` already uses — no
  separate removal logic of its own at all.

## Tests

Three new tests in `tests/tests/ociman_ps.rs` (where this project's
own existing `rm` test suite already lives):
`rm_latest_removes_only_the_most_recently_created_container` (two
real, already-`Stopped` containers with a real, distinguishable
creation-time gap; only the newer one is removed, confirming both
`--latest` and `-l` behave identically), `rm_latest_on_an_empty_
store_is_a_clear_error`, and `rm_latest_combined_with_anything_else_
is_a_clear_error` (all three real mutual-exclusivity cases). All 51
prior tests in `ociman_ps.rs` continue to pass unmodified (54/54
total).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean, no diff), `cargo clippy --workspace --all-targets --locked --
-D warnings`, `cargo test --workspace --locked` (119 test-result
blocks, 0 failures, clean on the first full run), `python3 ci/
guards.py`, `cargo deny check`, `bash ci/native-ci.sh` (clean,
119/119 on retry — one earlier attempt hit the known, pre-existing
`ocicri_container.rs` host-contention flake, confirmed environmental
via an immediate isolated rerun), `bash ci/build-deb.sh` (real
`dpkg -i`/`--version`/`dpkg -r` round trip). Touches only `ociman
rm`'s own selection logic, not any hot path at all — no benchmark
re-run needed.

## Deliberately still out of scope

`ociman stop`/`restart`/`pause`/`unpause --latest`/`-l` — the same
real, systemic sibling gap real podman shares across five commands,
deliberately not bundled into this same increment (matching this
project's own established "one command per note" convention already
used for the `--filter` family, `0418`-`0422`); each is now a
natural, separate future increment reusing the exact same shared
`resolve_latest_container` this increment introduces.
