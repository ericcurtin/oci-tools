# Design note 0208: `ocibox rm --all`

Status: implemented
Scope: `bin/ocibox/src/main.rs` (`Command::Rm.all`, `cmd_rm`,
`remove_one_box`); `tests/tests/ocibox_list_rm.rs`.

## Continuing milestone 7

0206's own "what this doesn't do yet" named `rm --all` as still ahead
— real `distrobox rm --all` removes every existing box in one call,
matching real `distrobox stop --all`'s own identical shape (checked
directly against `~/git/distrobox/pkg/commands/rm.go`). This closes
that one gap.

## The fix

`ocibox rm` now accepts either a positional `<NAME>` or `--all`/`-a`
(matching real `distrobox rm --all`'s own flag name and shorthand),
mutually exclusive — a clear error for either "neither given" or "both
given" rather than an ambiguous silent guess. Real `distrobox rm`
itself accepts any combination of explicit names and `--all`
simultaneously (`RmOptions.ContainerNames` plus `RmOptions.All`); this
project's own narrower scope doesn't replicate that combination, since
`--all`'s whole point (remove literally everything) makes combining it
with an explicit name pointless either way.

The actual single-box removal logic (`remove_one_box`, previously
`cmd_rm`'s own entire body) is now its own small function, reused once
per box when `--all` is given — every box gets its own attempt even if
an earlier one fails (matching real `distrobox rm`'s own identical
"continue past a per-container error rather than aborting the whole
batch" behavior), with the first failure's error still what the whole
command ultimately reports and exits nonzero for once every box has
had its turn.

`--all` on an already-empty store is a real, silent no-op: nothing to
remove, nothing printed, exit success — there's no box to report a
name or a failure for, unlike `ocibox list`'s own explicit `no boxes`
message (a listing command's whole job is reporting state; a
bulk-removal command has nothing further to say once "there was
nothing to do" is true).

## Verified by hand

* Three boxes created out of alphabetical order; `rm --all` removes
  all three, one name per line, sorted by name (same order `list`
  itself reports them in); `list` afterward confirms the store is
  genuinely empty.
* `rm --all` on an empty store: silent success, nothing printed.
* `rm somebox --all` (both given): clear `cannot give both a box name
  and --all` error.
* `rm` with neither a name nor `--all`: clear `no box name given`
  error.

## Tests

Three new integration tests in `tests/tests/ocibox_list_rm.rs`
(`rm_all_removes_every_box`, `rm_all_on_an_empty_store_is_a_silent_
success`, `rm_requires_exactly_one_of_name_or_all`), alongside the
seven pre-existing `list`/`rm` tests, all still passing unchanged.

Full `cargo build --workspace --locked`/`cargo test --workspace
--locked` (2 clean runs, 88/88 result blocks — no new test binary this
time, just three more tests in an existing one)/`cargo fmt --all
--check`/`cargo clippy --workspace --all-targets --locked -- -D
warnings`/`python3 ci/guards.py`/`cargo deny check`/`bash
ci/native-ci.sh` all clean (one unrelated, pre-existing flaky test,
`ociman_logs`'s own `logs_follow_streams_a_running_containers_output_
and_stops_when_it_exits`, failed once under full-workspace parallel
contention and passed cleanly both in isolation and on an immediate
full re-run — this change touches no `ociman`/`ocirun` code at all).
No performance regression (`ociman run --rm`, ~71ms, consistent with
prior measurements).

## What this doesn't do yet

`ocibox stop`, X11/Wayland/audio/nvidia passthrough, init-hooks,
additional-package installation, cloning an existing box, and
`rm --rm-home` are all still ahead.
