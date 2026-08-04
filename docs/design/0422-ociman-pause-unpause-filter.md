# Design note 0422: `ociman pause --filter` / `ociman unpause --filter`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_pause.rs`,
`README.md`.

## What this closes

`ociman pause`/`ociman unpause` had no `--filter` support at all.
This closes the `label=`/`label!=`/`until=` slice — the fifth and
sixth ports in the same `--filter` family `ociman container prune`/
`stop`/`rm`/`restart` (0418-0421) already established, completing
every command in this family. Unlike the four prior ports, this one
needed a real, deliberate behavioral correction, not just CLI wiring
— see below.

## Real, checked-directly confirmation

`~/git/podman/cmd/podman/containers/pause.go`/`unpause.go`: both
register `--filter` identically to `stop`/`rm`/`restart`, wired
through the identical `getContainers`. But `~/git/podman/pkg/domain/
infra/abi/containers.go`'s own `ContainerPause`/`ContainerUnpause`:

```go
for _, c := range containers {
    err := c.Pause()
    if err != nil && options.All && errors.Is(err, define.ErrCtrStateInvalid) {
        logrus.Debugf("Container %s is not running", c.ID())
        continue
    }
    ...
}
```

Confirmed directly, not assumed: the tolerant skip for a container in
the wrong state (`ErrCtrStateInvalid`, this project's own equivalent
of "not currently running"/"not currently paused") is gated on
`options.All` **specifically** — not on "a filter/selection mechanism
other than explicit names was used." A `--filter` match that isn't
actually eligible is therefore a real, reported error here too,
exactly like an explicit multi-id call already is — a genuine,
narrower divergence from the shape `stop`/`rm`/`restart --filter`
all share (where the eligibility tolerance is unconditional, not
gated on `--all` at all, confirmed separately in `0419`'s own
research). Missing this distinction would have been a real, silent
correctness bug, not just an incomplete feature.

## Implementation

- `Command::Pause`/`Command::Unpause` each gain `filter: Vec<String>`
  (`#[arg(long = "filter")]`), documented with the real divergence
  above spelled out explicitly on `Command::Pause::filter` (with
  `Command::Unpause::filter` referring back to it) so a future reader
  can't mistake this for the same shape as its four siblings.
- `cmd_pause`/`cmd_unpause` (both thin wrappers over the shared
  `cmd_pause_or_unpause`) and `cmd_pause_or_unpause` itself gain a
  `filter: &[String]` parameter and the same mutual-exclusivity check
  its siblings already have. When non-empty, a **new, separate** loop
  (deliberately not sharing the existing `--all` loop's own tolerant-
  skip branch) attempts every filter match and reports every real
  failure — mirroring the existing explicit-multi-id loop's own shape
  instead.

## Tests

Three new tests in `tests/tests/ociman_pause.rs`:
`pause_and_unpause_filter_label_only_act_on_a_matching_container`
(two real running containers, one matching `--filter
label=env=prod`, one not; `pause` then `unpause` both only ever
touch the matching one), `pause_filter_on_a_non_running_match_is_a_
real_error_unlike_all` (the one genuinely new-behavior test: a
never-started, filter-matching container is a real, reported error,
proving the `--all`-only tolerance distinction above is actually
implemented, not just documented), and `pause_and_unpause_filter_
combined_with_all_or_an_explicit_id_is_a_clear_error` (all four
mutual-exclusivity cases across both commands). All 8 prior tests in
`ociman_pause.rs` continue to pass unmodified (11/11 total).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean, no diff), `cargo clippy --workspace --all-targets --locked --
-D warnings`, `cargo test --workspace --locked` (119 test-result
blocks, 0 failures, clean on the first full run), `python3 ci/
guards.py`, `cargo deny check`, `bash ci/native-ci.sh` (clean,
119/119), `bash ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg
-r` round trip). Touches only `ociman pause`/`unpause`'s own
selection logic, not any hot path at all — no benchmark re-run
needed.

## Deliberately still out of scope

Combining `--filter` with explicit names/`--cidfile`/`--all`; the
wider `ps`-grammar keys (needs `cmd_ps`'s own inline matching closure
extracted first) — both noted identically in `0418`-`0421`'s own
design notes. With this increment, the `--filter` family started at
`0418` is now complete across all six commands that share it in real
podman (`container prune`/`stop`/`rm`/`restart`/`pause`/`unpause`).
