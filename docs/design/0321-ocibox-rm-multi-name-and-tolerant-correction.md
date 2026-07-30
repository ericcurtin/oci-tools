# Design note 0321: `ocibox rm` multi-name support, and a real behavioral correction

Status: implemented
Scope: `bin/ocibox/src/main.rs`, `tests/tests/ocibox_list_rm.rs`.

## Why `ocibox` specifically, this turn

`ociman`'s own container-lifecycle commands (`kill`/`stop`/`restart`/
`rm`/`pause`/`unpause`) all now have the full `--all`/multi-target
shape real podman gives them (0310-0320). A broader survey turned up
that `ocibox` (the `distrobox` equivalent) has had comparatively little
dedicated attention recently — its own last dedicated feature commit
predates this entire recent streak. `ocibox rm` was still a single-
name-only command (`name: Option<String>`), unlike real `distrobox rm
NAME [NAME...]` — the same class of gap this project had already
closed several times over for `ociman`, and a genuinely small, well-
scoped next increment.

## A real, previously-incorrect behavioral assumption found and corrected

Reading real `distrobox`'s own source directly before implementing
anything (`~/git/distrobox/pkg/commands/rm.go`'s own `Execute`/
`getContainersToRemove`/`warnUnknownContainers`, traced all the way to
`cmd/distrobox/main.go`'s own top-level `run()`/`log.Fatal` to confirm
exactly what determines the process's own real exit code) turned up
two real, checked-directly surprises, neither matching this project's
own pre-existing `ocibox rm` implementation or its own prior design
assumptions:

1. **An explicitly-named box that doesn't resolve at all is only ever
   a printed warning, never a hard, aborting error** — real
   `distrobox rm somename` on a name that doesn't exist prints
   `Cannot find container somename.` and still exits `0`. Likewise, a
   genuine per-box removal failure inside the loop is only ever
   printed (`c.printer.PrintErrorln`), never propagated as a process
   exit code — `Execute` unconditionally returns a nil error once its
   own removal loop finishes, regardless of what happened inside it.
   This project's own pre-existing `ocibox rm <unknown-name>`
   previously hard-errored (exit non-zero) — a real, incorrect
   assumption, now corrected.
2. **`--all` and explicit names are not mutually exclusive at all** —
   real `distrobox rm somebox --all` still removes *every* box, with
   `somebox` (and any other given names) simply ignored entirely
   (`getContainersToRemove` returns every container unconditionally
   when `all` is true, never even consulting the `names` parameter).
   This project's own pre-existing implementation instead treated
   giving both as a hard error — again, a real, incorrect assumption,
   now corrected to match.

One deliberate, *non*-correction: a malformed/path-traversal name
(this project's own defensive `validate_box_name` check, protecting
`remove_dir_all` from ever reaching outside `boxes_root()`) still
remains a real, immediate, whole-call-aborting error, checked
*before* attempting to remove anything. Real distrobox has no
equivalent separate validation step at all — an invalid name there
would simply never match any real, listed container and fall into the
same "not found, warn" path as an ordinary typo. This project's own
security-motivated validation is a deliberate addition worth keeping
strict even though real distrobox's own architecture doesn't have (or
need) an equivalent distinction — weakening it to a mere warning would
be a real, if narrow, security regression for no compatibility
benefit.

## Implementation

`Command::Rm`: `name: Option<String>` → `names: Vec<String>`. `cmd_rm`
now: under `--all`, unconditionally attempts every real box regardless
of any names also given (only ever printing a per-box error, never
propagating one); otherwise, requires at least one name, validates
every given name up front (aborting immediately on a malformed one),
then attempts every name's own removal regardless of an earlier
failure — a genuinely nonexistent name only ever prints
`{name}: no such box` to stderr and moves on, never aborting the
call or affecting the final exit code.

## Verified

Manual: `ocibox rm box1 box2` removes both, printing each; `ocibox rm
box1 nonexistent` still removes `box1` and prints a warning for the
other, exiting `0`; `ocibox rm box1 --all` removes *every* box
(including ones other than `box1`), not just `box1`; a path-traversal
attempt (`ocibox rm ../canary.txt`) is still a real, immediate,
whole-call-aborting error, with the canary file outside `boxes_root`
confirmed untouched.

Integration (`tests/tests/ocibox_list_rm.rs`): one new test
(`rm_accepts_multiple_names_and_tolerates_an_unresolvable_one`); two
existing tests corrected to reflect the verified real behavior
(`rm_of_an_unknown_name_is_a_clear_error` → `rm_of_an_unknown_name_
prints_a_warning_but_still_succeeds`; `rm_requires_exactly_one_of_
name_or_all` split into `rm_requires_a_name_or_all` plus a new,
more meaningful `rm_all_takes_priority_over_any_names_also_given`
that creates a real box and confirms `--all` removes it despite an
unrelated, nonexistent name also being given); the pre-existing
path-traversal-rejection test needed no change at all, confirming the
deliberate non-correction above stayed intact. 12 total tests in this
file (10 pre-existing, minus the two rewritten, plus 2 net new).

Regression: all 12 `ocibox_list_rm.rs` tests pass; the rest of the
`ocibox` test suite (`ocibox_create.rs`, `ocibox_enter.rs`,
`ocibox_ephemeral.rs`, `ocibox_export.rs`) is unaffected, since none of
them call `rm` in a way this change alters. Full `cargo test
--workspace --locked`: 112 test result blocks, 0 failures.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).

Performance: `ocibox rm` is a one-shot, offline command, not part of
any hot-path benchmark tracked in `docs/benchmarks.md`. No
re-benchmark needed.

## Still ahead

Real `distrobox` has several other subcommands/features `ocibox`
doesn't have at all yet — `stop` (blocked on this project's own
current `enter`-is-a-single-foreground-process architecture having no
persistent, background container to stop at all — a materially bigger
feature, already repeatedly named across 0206/0208/0211/0252),
`upgrade` (needs a real, in-container multi-distro package-manager
dispatch, also already named as out of scope), `generate-entry`/
`assemble` (batch/manifest-driven creation), and `export --app`
(desktop-entry export with icon handling — the most self-contained of
these remaining gaps, and a real, separately-scoped candidate worth
picking up next for `ocibox` specifically, since — unlike `stop`/
`upgrade` — it needs no new architectural decisions at all, only new
file-generation logic reusing `export`'s own already-established
`--bin` machinery).
