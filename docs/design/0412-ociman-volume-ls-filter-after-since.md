# Design note 0412: `ociman volume ls --filter after=`/`since=`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_volume.rs`,
`README.md`.

## What this closes

Closes the last of the three candidates `0410`'s own "Deliberately
still out of scope" section explicitly flagged (`until=` closed by
`0411`; `after=`/`since=` closed here) — matches a volume created
strictly after a *named* reference volume's own creation time.

## Real, checked-directly confirmation, including a real, checked-directly asymmetry

`~/git/podman/pkg/domain/filters/volumes.go`'s own dispatch table:
`case "after", "since": return createAfterFilterVolumeFunction(...)`
— real, checked-directly synonyms for the identical filter. Unlike
`images`/`ps --filter before=`/`since=` (both of which have a real,
separate `before=` key), **real podman's own volume filters have no
`before=` key at all** — only this one. `createAfterFilterVolumeFunction`
itself:

```go
func createAfterFilterVolumeFunction(filterValues []string, runtime *libpod.Runtime) (libpod.VolumeFilter, error) {
    var createTime time.Time
    for _, filterValue := range filterValues {
        vol, err := runtime.LookupVolume(filterValue)
        ...
        if createTime.IsZero() || createTime.After(vol.CreatedTime()) {
            createTime = vol.CreatedTime()
        }
    }
    return func(v *libpod.Volume) bool {
        return createTime.Before(v.CreatedTime())
    }, nil
}
```

For multiple values, `createTime` ends up the **earliest** of every
referenced volume's own creation time (each iteration keeps whichever
of the running value and the new one is earlier) — the same
"earliest, for either key" rule `ociman ps --filter before=`/`since=`
already established for containers (`0280`), *not* `images --filter
before=`/`since=`'s own separate earliest-for-`before`/latest-for-
`since` rule (`0293`) — a real, checked-directly distinction worth
getting right rather than assumed to generalize from the more
recently touched `images` case.

## Implementation

- `VolumeCommand::Ls`'s own `filter` field doc comment gains
  `after=`/`since=`, including the real "no `before=` key at all"
  asymmetry.
- `resolve_volume_created`/`earliest_referenced_volume_creation`
  mirror `ociman ps`'s own `resolve_container_created`/`earliest_
  referenced_creation` (containers) exactly in shape — resolving each
  reference volume via the already-existing `VolumeStore::get`, then
  reducing to the earliest.
- `cmd_volume_ls` resolves `after`/`since` values (an alias via
  `strip_prefix("after=").or_else(|| strip_prefix("since="))`,
  matching real podman's own dispatch) up front, then `Vec::retain`s
  by a strict `threshold < created` comparison — the mirror image of
  `until=`'s own strict `created < threshold`, with the identical
  "absence over fabrication" treatment for an unparseable
  `created_at`.

## Tests

One new end-to-end integration test in `tests/tests/ociman_volume.rs`,
`volume_ls_filter_after_and_since_use_the_referenced_volumes_own_
creation_time` — three real volumes spaced apart in time,
`after=<vol-1>` matching exactly the later two, `since=<vol-1>`
producing the identical result (proving the real synonym), multiple
values (`after=<vol-2>` and `after=<vol-1>` together) using the
earliest of the two and matching the same set `after=<vol-1>` alone
would, and an unresolvable reference volume a clear error. All
existing tests continue to pass unmodified (37/37 in
`ociman_volume.rs`).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures, clean on
the first full run), `python3 ci/guards.py`, `cargo deny check`,
`bash ci/native-ci.sh`, `bash ci/build-deb.sh` (real `dpkg -i`/
`--version`/`dpkg -r` round trip). This touches only `ociman volume
ls`'s own filter-matching path, not any hot path at all — no
benchmark re-run needed.

## Deliberately still out of scope

`driver=`/`label=`/`label!=`/`opt=`/`dangling=` remain unimplemented
— each needs real schema/data this project's volumes don't store at
all yet (labels, driver options) or a real "is this volume currently
referenced by any container" concept this command doesn't compute
today (`dangling=`, though `containers_using_volume` already exists
and computes the identical real answer for `rm`/`prune`, making it
the single most plausible remaining candidate for a future
increment). `ociman volume ls --filter` now has real, working
coverage of every key its own most common real-world uses would
reach for.
