# Design note 0544: `ocibox enter`/`ocibox ephemeral` trailing command parsing, plus a real `ephemeral` stdout-contamination bug found along the way

Status: implemented
Scope: `bin/ocibox/src/main.rs`, `tests/tests/ocibox_enter.rs`,
`tests/tests/ocibox_ephemeral.rs`.

## What this closes

Two things, both real, checked-directly bugs against this project's
own already-built binary (not guesses):

1. `ocibox enter <NAME> <COMMAND>...`/`ocibox ephemeral <COMMAND>...`
   previously required an explicit `--` before any command whose own
   arguments looked like flags (e.g. `ocibox enter mybox ls -la`,
   `ocibox ephemeral -i busybox printenv -0`) — a real, immediate clap
   parse error (`unexpected argument '-l' found`) without one. Real
   `distrobox enter`/`distrobox ephemeral` accept this shape directly,
   with no `--` needed at all.
2. Found while writing this increment's own test: `ocibox ephemeral`'s
   internal cleanup step unconditionally printed the generated box's
   own name to stdout, right after the entered command's own real
   output, with no separating newline — a real, previously-unnoticed
   stdout-contamination bug, independent of the trailing-args fix
   above.

## Part 1: trailing command parsing

### Real, checked-directly confirmation

- `~/git/distrobox/internal/cli/parse.go:9-15` (`PrepareArgs`'s own
  doc comment): lets `enter`/`ephemeral` accept distrobox flags after
  the container name, "like the bash `distrobox-enter`" — finds the
  real command itself and splices a bare `"--"` in front of it, then
  lets the CLI framework parse the rest.
- `~/git/distrobox/internal/cli/parse.go:69-116`
  (`splitExecCommand`): once the box name is resolved, the first bare,
  non-flag token is where the real command begins; no error is raised
  even if later tokens look like flags — they're already past the
  spliced `--`.
- Real, live-consumed confirmation via real distrobox's own unit
  tests: `~/git/distrobox/internal/cli/parse_internal_test.go:39-42`
  (`"implicit command"`: `enter suse echo ciao` → `--` spliced
  automatically) and `:67-70` (`"command word before its own flag"`:
  `enter suse vim --help` → `enter suse -- vim --help`, i.e. `vim`'s
  own `--help` passes straight through, never triggering distrobox's
  own help).
- `PrepareArgs` is genuinely wired into the real binary's entrypoint:
  `~/git/distrobox/cmd/distrobox/main.go:31-34`.

### Fix

`#[arg(trailing_var_arg = true, allow_hyphen_values = true)]` added to
`Command::Enter::command` (`bin/ocibox/src/main.rs`) and
`Command::Ephemeral::command` — the exact same attribute pair already
established in this codebase for an identical shape:
`bin/ociman/src/main.rs`'s own `RunArgs::args`. No new dependency, no
new namespace/launch code — a pure clap-attribute fix on an
already-existing field.

Live-verified this matches real distrobox's own one real constraint
too: once the trailing command starts being consumed, no further
`ocibox`-own flag can be recognized in the tail (e.g. `ocibox
ephemeral true -i busybox` treats `-i busybox` as part of the
*command*, not `--image`, leaving neither `--image` nor `--clone`
given) — the same real requirement `splitExecCommand`'s own
first-bare-token rule already imposes upstream (every distrobox-own
flag must precede the command), not a new limitation introduced here.

## Part 2: `ocibox ephemeral`'s own stdout contamination (found along the way)

### Real, checked-directly confirmation

- `~/git/distrobox/pkg/commands/rm.go`'s own `Execute`: no
  success-path `Print*` call at all — a successful removal, whether
  from standalone `distrobox rm` or `ephemeral`'s own internal
  cleanup, prints *nothing*. Only warnings/errors are ever printed.
- `~/git/distrobox/internal/cli/ephemeral.go:109`: the printer bound
  to `ephemeral`'s own internal `rm` cleanup call is deliberately
  `ui.NewPrinter(os.Stderr, true)` — even a cleanup *failure*'s own
  warning is routed to stderr, specifically so it can never
  contaminate the entered command's own real stdout output. (Compare
  `~/git/distrobox/internal/cli/rm.go:73`, the *standalone* `rm`
  command's own printer, which *is* `os.Stdout` — the two commands
  deliberately differ here.)

### This project's own prior state

`remove_one_box` (used by both `ocibox rm`'s per-name and `--all`
paths) always printed the removed name via `println!` on success — a
real, deliberate, already-tested, pre-existing choice for the
*standalone* `ocibox rm` command, independent of real `distrobox rm`'s
own silent-on-success behavior, matching real `podman rm`/`docker
rm`'s own identical printed-name-on-success convention instead (see
`tests/tests/ocibox_list_rm.rs`'s own `rm_removes_a_real_box_entirely`,
unchanged by this note). `cmd_ephemeral`'s own internal cleanup step
reused this exact same function, inheriting its `println!` — visible
only once a test asserted an *exact* stdout value (an `echo -n`
suppressing its own trailing newline let the box name concatenate
directly onto the command's own output on the same line).

### Fix

Split `remove_one_box` into `remove_box_dir` (validate + remove, no
output) and `remove_one_box` (`remove_box_dir` plus the print, for
`ocibox rm`'s own two call sites, unchanged). `cmd_ephemeral`'s own
cleanup now calls `remove_box_dir` directly — success is silent
(matching real distrobox's own identical silent-on-success `rm`), a
cleanup failure still only ever produces a warning on stderr (already
true here via the pre-existing `eprintln!`), matching real
distrobox's own deliberate stderr-only routing for this exact case.

## Tests

- `enter_runs_a_command_with_its_own_flag_like_arguments_without_a_
  leading_double_dash` (`tests/tests/ocibox_enter.rs`): `ocibox enter
  testbox /bin/sh -c "exit 0"`/`"exit 42"`, no `--`, both forwarding
  the real exit code correctly — previously a clap parse error.
- `ephemeral_runs_a_command_with_its_own_flag_like_arguments_without_a_
  leading_double_dash` (`tests/tests/ocibox_ephemeral.rs`): `ocibox
  ephemeral --image ... /bin/echo -n hello-ephemeral`, no `--`,
  asserting the *exact* stdout is `hello-ephemeral` (no trailing
  newline, and — now that Part 2 is also fixed — no box name appended
  either). This is the same test that surfaced the Part 2 bug in the
  first place: before that fix, this assertion failed with
  `"hello-ephemeralocibox-<id>\n"` even though the trailing-args
  parsing itself was already correct.

Manually exercised beyond the automated tests: `ocibox enter mybox ls
-la` (progresses past parsing to a real, expected "no such box"
error, instead of a clap parse error), `ocibox ephemeral -i busybox
true -x` (a real, successful end-to-end run), and `ocibox ephemeral
true -i busybox` (confirms `-i busybox` is swallowed into the command
once trailing-var-arg consumption starts, matching real distrobox's
own identical constraint, not a new limitation).

## Verification

`cargo build --workspace --locked` (clean), `cargo fmt --all` (clean),
`cargo clippy --workspace --all-targets --locked -- -D warnings`
(clean), targeted `ocibox_enter.rs`/`ocibox_ephemeral.rs`/
`ocibox_list_rm.rs` runs (19/19, 11/11, 25/25 — confirming the
standalone `ocibox rm`'s own printed-name convention is untouched), a
full `cargo test --workspace --locked` run (clean), `python3
ci/guards.py` (clean), `cargo deny check` (clean), `bash
ci/native-ci.sh` (clean), `bash ci/build-deb.sh` (clean, real `dpkg
-i`/`--version`/`dpkg -r` round trip). Pure CLI-parsing-attribute plus
a small stdout-routing fix — no hot path touched, no `ci/bench.sh`
rerun needed.
