# Design note 0472: `ociman mount --all`/`--latest`/multi-id support

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_mount.rs`.

## What this closes

`0470`'s own "still out of scope" section flagged this directly:
`ociman mount --latest`/`--all` (the mirror-image gap on `mount`
itself). This closes it, along with multi-id support (`mount
CONTAINER CONTAINER...`), a third real gap surfaced while re-checking
the exact upstream argument shape this increment.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/mount.go:30`: `Use: "mount
  [options] [CONTAINER...]"` — real podman's own `mount` already
  accepts *multiple* explicit containers, not just zero or one.
- Lines 33-35 (`Args`): `validate.CheckAllLatestAndIDFile(cmd, args,
  true, "")` — `ignoreArgLen=true` (unlike `unmount`'s own `false`,
  `0471`), so the generic validator never itself rejects a totally
  bare invocation, nor a `--latest`-with-explicit-args combination.
  It still rejects `--all`+`--latest` together (`"--all and --latest
  cannot be used together"`) and `--all` with an explicit container
  (`"no arguments are needed with --all"`).
- Lines 79-81 (`mount()`'s own `RunE`): a *separate*, manual check —
  `if len(args) > 0 && mountOpts.Latest { return errors.New("--latest
  and containers cannot be used together") }` — fills the one gap
  `ignoreArgLen=true` otherwise leaves open.
- Lines 87-104: **a real, checked-directly output-shape divergence**
  — `if len(args) > 0 || mountOpts.Latest || mountOpts.All { ...
  print just each report's own `r.Path`, one per line, no id, no tab
  ... }`, entirely separate from the true-bare-invocation branch
  further down that prints the `{{.ID}}\t{{.Path}}` table (`0470`).
  So `mount --all`, `mount --latest`, and `mount CONTAINER
  [CONTAINER...]` all print **paths only** — the table is reserved
  for the truly bare case alone.
- `~/git/podman/pkg/domain/infra/abi/containers.go:1489-1503`
  (`ContainerMount`, `--all` branch): continues past an individual
  `ErrNoSuchCtr`/`ErrCtrRemoved` (skips silently) but *still appends*
  every other real error to `reports`, which `mount()`'s own `RunE`
  then collects into `errs` without aborting the loop over other
  containers — i.e. `--all` tolerates individual failures and reports
  a combined error only at the end, exactly matching this project's
  own already-established `kill --all` convention (`0320`-era).
- Plain multi-id (no `--all`/`--latest`): `getContainers`'s own
  `default:` case (`names` non-empty) returns a bare Go error
  immediately the moment any one name fails to resolve — which
  `ContainerMount` propagates straight up as `return nil, err`,
  discarding every *already-successful* report gathered so far too.
  A real, checked-directly "abort before printing anything" behavior,
  matching `unmount`'s own identical two-phase convention (`0471`).

## Implementation

- `Command::Mount::container: Option<String>` → `containers:
  Vec<String>`, new `all: bool` (`-a`/`--all`), `latest: bool`
  (`-l`/`--latest`).
- `cmd_mount(ids: &[String], all: bool, latest: bool)`: the same
  three-check validation order as above (no fourth "none given"
  check — a truly bare invocation is valid, unlike `unmount`), then:
  - `--all`: iterates every container (sorted by `created` ascending,
    matching the bare-mode order), continuing past the one real
    rootless-overlay-rootfs gap container per `kill --all`'s own
    convention (`eprintln!`+remember the first error, keep going),
    printing every other one's real root path; returns the first
    error (if any) only at the end.
  - `--latest`: resolves the single most-recently-created container
    (`resolve_latest_container`, shared with every other `--latest`
    command) and prints its path, hard-erroring on the overlay gap
    like the original single-container case always did (never
    tolerant the way `--all` is — there's only one target).
  - One explicit id: unchanged from before this increment.
  - Two or more explicit ids: the same two-phase "resolve every one
    first, abort before printing anything if any fails" convention
    `cmd_unmount` already established (`0471`).
  - None of the above: the truly bare listing mode, unchanged from
    `0470`.

## Tests

Seven new integration tests in `tests/tests/ociman_mount.rs`:
multi-id success (verified against the plain-path output shape, never
the table), multi-id-with-one-unknown two-phase abort, `--all`
(verified against plain paths, unordered-compared since sweep order
here is `created`-ascending but real podman's own is unspecified),
`--latest`, and the three validation-error cases (`--all`+`--latest`,
`--all`+explicit, `--latest`+explicit). All 23 tests in the file pass
(16 prior + 7 new).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (120 test-result
blocks, 0 failures on the fourth attempt with `--test-threads=4` —
the first three attempts each hit transient, already-documented flaky
failures across `ocicri_container.rs`/`ociman_run.rs`'s own cgroup
tests, all confirmed unrelated and passing instantly in isolation,
consistent with unusually heavy load from this dev host's long-
running CPU-spinning background process this session — load average
noticeably higher than prior sessions' own typical baseline),
`python3 ci/guards.py` (clean), `cargo deny check` (clean), `bash
ci/native-ci.sh` (clean, 120/120 on the first attempt), `bash
ci/build-deb.sh` (clean, real `dpkg -i`/`--version`/`dpkg -r` round
trip on the first attempt). No benchmark re-run needed: `ociman mount`
is not exercised by `ci/bench.sh` at all.

## Deliberately still out of scope

`--format`/`--no-trunc` on `mount` (real podman's own richer output
shapes for the bare-listing table specifically) — this project's own
single default line shape remains the only one implemented.
