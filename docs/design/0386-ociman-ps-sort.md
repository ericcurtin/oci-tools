# Design note 0386: `ociman ps --sort`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_ps.rs`.

## What this closes

`ociman ps` had no `--sort` flag at all — real `podman ps --sort
{command,created,id,image,names,runningfor,size,status}`. Confirmed
via `cargo run --bin ociman -- ps --help`: no such flag existed.

## Real, checked-directly confirmation

- **Exact valid key strings**: `command, created, id, image, names,
  runningfor, size, status` — confirmed both from `~/git/podman/cmd/
  podman/containers/ps.go`'s own `validate.Value` choice list and a
  real installed `podman ps --help`. Plural `names`, one-word
  `runningfor` (no hyphen) — a real clap `ValueEnum` casing trap:
  naming the enum variant `RunningFor` would kebab-case to
  `running-for`, not podman's actual spelling, so the variant is
  spelled `Runningfor` instead.
- **Invalid value**: a real, immediate clap parse error listing the
  valid choices — matching real podman's own cobra-choice-validated
  flag rejection exactly (`Error: invalid argument "bogus" for
  "--sort" flag: ... Choose from: ...`, confirmed live).
- **All eight keys sort ascending** — checked directly, no key
  (including `size`) sorts descending.
- **Default order re-verified, confirmed correct**: `~/git/podman/
  pkg/ps/ps.go`'s own unconditional final sort (line ~109-110) is
  ascending by creation time — exactly what `ociman ps`'s own
  pre-existing, unconditional `views.sort_by(|a, b| a.created.cmp(...))`
  already did. No bug there; this closes only the missing opt-in
  override.
- **`--last`/`-n` selection stays creation-time-based regardless of
  `--sort`** — confirmed live against a real installed podman:
  `podman ps -a -n 2 --sort names` still keeps the 2 most-recently-
  *created* containers, merely displaying that same set alphabetically
  (not the 2 that would sort last alphabetically). `--sort` must be
  applied *after* the existing `--last` trim, not in place of the
  pre-trim default sort that trim itself depends on.
- **`--sort` composes fully with both `--quiet` and `--format`** —
  confirmed live, neither overrides or ignores it.
- **`--sort size` without `--size`**: a real, semantic no-op (every
  size is absent, so every pair compares "not less," leaving order
  untouched), not an error — confirmed directly from `container_ps.go`
  (`psSortedSize.Less` returns `false` when either side's `Size` is
  `nil`) and live testing. With `--size` also given, sorts by real
  `RootFsSize` specifically, not `RwSize`.
- **`--sort runningfor` is a real, previously-mis-scoped finding from
  research**: it does *not* sort identically to `--sort created` (an
  earlier research pass's incorrect claim, caught and corrected during
  a second, deeper research pass before implementation). Real podman's
  own `psSortedRunningFor.Less` (`container_ps.go`) compares
  `StartedAt`, a genuinely different timestamp — confirmed live: a
  container created second but started *first* sorts before one
  created first but started second, the opposite of `--sort created`'s
  own order. `ociman`'s own `PersistedState::started_at` is the exact
  matching field, but `ContainerView` (the `ps` output row type) didn't
  carry it at all — a real, not-optional scope item, not just a
  one-line `sort_by` swap.

## Implementation

- New `PsSortKey` enum (`clap::ValueEnum`, matching `PullPolicy`/
  `SaveFormat`'s own established derive convention).
- `Command::Ps` gains `sort: Option<PsSortKey>`.
- `ContainerView` gains `started_at: Option<String>` (`#[serde(skip)]`
  — purely an internal sort key, not a new public output field this
  increment set out to add), populated from `state.started_at.clone()`
  in `ContainerView::from_state`. `None` (never started) correctly
  sorts before any `Some(_)` for free, via `Option<String>`'s own
  default `Ord` — the same real "zero-time sorts first" behavior
  podman's own Go comparison gives it.
- `cmd_ps`'s existing default sort (`views.sort_by(|a, b| a.created...)`)
  and the `--last` trim immediately after it are both left completely
  unchanged. A new `if let Some(sort) = sort { match sort { ... } }`
  block is inserted *after* the trim, re-sorting whatever set `--all`/
  `--filter`/`--last` already produced — `command`/`created`/`id`/
  `image`/`names`/`status` are direct field comparisons; `runningfor`
  compares `started_at`; `size` compares `root_fs_size` when both
  sides have a size, otherwise treats the pair as equal (matching
  real podman's own no-op-without-`--size` behavior exactly, while
  still sorting meaningfully by real, measured `root_fs_size` when
  `--size` was given).

## Tests

Six new tests in `tests/tests/ociman_ps.rs`:
`ps_sort_names_orders_alphabetically`,
`ps_sort_runningfor_differs_from_created_order` (the key regression
test proving `runningfor` is genuinely distinct from `created`, not
an alias — a container created second but started first, via real
`ociman start` calls in the opposite order from creation),
`ps_sort_composes_with_last_without_changing_which_containers_are_
selected`, `ps_sort_rejects_an_invalid_value`,
`ps_sort_size_without_size_flag_is_a_no_op`, and
`ps_sort_size_with_size_flag_orders_by_real_root_fs_size` (a real,
kernel-measured size difference — one container's own writable layer
genuinely inflated via a real `dd` write, the other left empty). All
48 tests in the file pass (42 pre-existing + 6 new).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures), `python3
ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh`, `bash
ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg -r` round trip).
This change is `ociman ps`-only, not on any container-launch hot path
at all — `ci/bench.sh` doesn't measure `ociman ps` (confirmed by
grep) — no benchmark re-verification needed.
