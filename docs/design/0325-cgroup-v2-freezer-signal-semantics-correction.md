# Design note 0325: correcting `0319`/`0324`'s own cgroup v2 freezer premise

Status: implemented
Scope: `crates/oci-runtime-core/src/cgroups.rs`,
`docs/design/0319-kill-thaws-paused-containers.md`,
`docs/design/0324-stop-restart-thaw-paused-containers.md`.

## What prompted this

While investigating `0324`'s own flagged "still ahead" candidate —
whether `ociman rm --force`'s `remove_container` needed the same
`kill_thawing_if_paused` fix `kill`/`stop`/`restart` already got — a
raw `kill -9 $PID` sent directly to a process in a genuinely frozen
cgroup v2 cgroup (no `ociman` involved at all) killed it (confirmed:
became a zombie) in well under a second, *without any thaw*. This
directly contradicted `0319`'s own documented premise (sourced from
real runc's own cgroup-v1-specific comment: "killing a process in a
frozen cgroup does nothing until it's thawed"), which this project had
generalized to "a frozen cgroup's own freezer queues every signal
completely identically."

## The real, authoritative mechanism (checked directly, not assumed)

The kernel's own authoritative docs
(`Documentation/admin-guide/cgroup-v2.rst`, "cgroup.freeze") state
plainly:

> Processes in the frozen cgroup can be killed by a fatal signal.

Confirmed empirically, using a real, live `systemd-run --user --scope
-p Delegate=yes` cgroup (the same real-cgroup technique this project's
own tests already use) for two distinct real child processes:

- **No handler installed** (a plain `sleep`, sent `SIGKILL`, which can
  never be handled by any process regardless): dies within under a
  second while the cgroup is still frozen and never thawed at all — a
  fatal signal reaches straight through a freeze.
- **A live handler installed** (`bash -c 'trap "exit 42" USR1; ...'`,
  sent `SIGUSR1`): does *not* react at all while still frozen — no
  exit, no side effect — only actually running the trap (observed via
  its own distinctive exit code) once the cgroup is explicitly
  thawed. A signal the target process would *handle* is genuinely
  queued, not delivered, while frozen.

So the real distinguishing factor was never "every signal vs. just
`SIGKILL`" — it's **fatal vs. handled**, a property of how the *target
process* would react to that specific signal, not a property of the
signal number itself. `SIGKILL` is always fatal (can never be
handled). A signal like `SIGTERM` is fatal *by default*, but not once
a process installs its own handler for it — which is exactly what a
real container's own init process commonly does for graceful
shutdown, precisely the scenario `stop`'s own first-phase signal is
meant to trigger.

## Was the shipped fix ever actually wrong?

No — `kill_thawing_if_paused` (`0319`) and its use in `stop`/`restart`
(`0324`) remain fully correct and necessary. Two real, independent
problems it closes, now correctly attributed:

1. **A handled signal genuinely is queued until thaw.** `stop`'s own
   graceful `TERM`-then-escalate sequence would otherwise silently hang
   against a paused container with its own `TERM` handler installed,
   for the entire grace window, before the final `KILL` escalation
   eventually got through on its own (see problem 2). `kill` sending a
   non-`KILL` signal to a paused container has the identical exposure.
2. **The cgroup's own `frozen` flag is stuck, not the process.** Even
   when a fatal signal *does* get through immediately (no thaw needed
   for the process to actually die), the cgroup's own freeze state is
   a property of the cgroup, not the (now-dead) process inside it — it
   stays at `1` forever unless something explicitly thaws it. Without
   this fix, `ociman`'s own `display_status` (`ps`/`inspect`) would
   permanently misreport an already-dead container as still `Paused`,
   which is exactly the originally-observed symptom (`ociman kill` on
   a paused container "silently doing nothing" — the process actually
   died right away; it was the *status* that stayed stuck).

Neither problem needed `SIGKILL` specifically to reproduce, and the
fix already generalizes to any signal, which remains exactly correct
— just for reason (1), not the previously-assumed "every signal is
queued identically."

## Was `ociman rm --force` (the prompting question) affected?

No fix needed. `remove_container`'s own SIGKILL-before-removal poll
loop calls `oci_runtime_core::process::alive(pid)` — a raw,
direct `/proc/<pid>` liveness check — never `display_status`/
`is_frozen`. It is already immune to problem 2 above (no status ever
gets read there), and a plain `SIGKILL` already reaches a frozen
process directly per the mechanism confirmed above, so problem 1
doesn't apply either (`rm --force` never sends anything but `SIGKILL`,
which can't be "handled" by definition). Empirically confirmed too: a
manually paused container's process does become a zombie well within
`remove_container`'s own 5-second poll window, exactly as the
existing, unmodified code already assumes.

## What actually changed

Doc-only correction, no behavior change to already-shipped, tested
code:

- `kill_thawing_if_paused`'s own doc comment
  (`crates/oci-runtime-core/src/cgroups.rs`) rewritten to explain the
  real fatal-vs-handled distinction, cite the kernel's own
  authoritative doc, and correctly attribute the fix's necessity to
  two independent, precise reasons instead of the original,
  overgeneralized "every signal is queued" claim.
- `0319`/`0324` both get a short, clearly-marked correction pointing
  here, without rewriting their own historical "Verified"/"Real,
  checked-directly semantics" sections wholesale — those verifications
  (the code works, the tests pass) were never wrong, only the
  *explanation* of the underlying kernel mechanism was.
- `0324`'s own "still ahead" mention of `rm --force` as a candidate is
  resolved here: investigated, found not to need any change (see
  above), and updated in place to record that conclusion.

## New regression coverage

`crates/oci-runtime-core/src/cgroups.rs`'s own test module gets one new
test, `cgroup_v2_freezer_lets_a_fatal_signal_through_but_queues_a_
handled_one`: two real child processes in two real, live, delegated
`systemd-run --user --scope` cgroups (gated on a reachable `systemd
--user` session, matching this crate's own existing convention), one
sent a fatal signal (dies while still frozen, no thaw), one sent a
signal it has a live handler for (stays alive while frozen, only reacts
once thawed) — locking in the *real* distinction permanently, not just
asserting it in a comment. Deliberately does **not** launch these via a
raw `libc::fork()` inside the test process itself: this crate's own
`process::debug_assert_single_threaded` doc comment already explains
why that would be unsound here (`cargo test`'s own multi-threaded
harness, plus a leftover background D-Bus thread from an earlier
`systemd_cgroup::create_scope`-style call, risks a lock held forever in
a raw-forked child) — confirmed directly, not just assumed: an earlier
draft of this same test that *did* fork raw children between two
`create_scope` calls reliably hung indefinitely. Using
`std::process::Command::new("systemd-run").args(["--user", "--scope",
...])` instead avoids this entirely (`Command::spawn` does its own safe
fork+exec, and `--scope` execs the target in place, so `child.id()` is
the real target pid directly).

## Verified

`cargo test -p oci-runtime-core --locked --lib
cgroup_v2_freezer_lets_a_fatal_signal_through_but_queues_a_handled_one`:
passes consistently across repeated runs (4 consecutive, no flakes),
~1.7s each, no leftover systemd scopes or processes afterward.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked`, `python3 ci/guards.py`,
`cargo deny check`, `ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg
-i`/`--version`/`dpkg -r` round trip).

Performance: doc-only change plus one new, non-hot-path unit test; no
effect on any tracked benchmark.

## Still ahead

Nothing new opened by this note. The `ocibox`/`ocivmm` remaining gaps
`0324` already listed remain the same separately-scoped future
candidates.
