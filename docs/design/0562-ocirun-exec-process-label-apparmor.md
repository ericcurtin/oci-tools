# Design note 0562: `ocirun exec --process-label`/`--apparmor`

Status: implemented
Scope: `bin/ocirun/src/main.rs`, `tests/tests/ocirun_exec.rs`.

## What this closes

`docs/design/0408` and `docs/design/0509` both explicitly named
`--process-label`/`--apparmor` (no SELinux/AppArmor support anywhere
in this project) as `ocirun exec`'s own real, still-open gap. No
later note (checked through `0561`) closed it. This adds both flags
as real, honestly-scoped CLI-compatibility additions.

## A note on the candidate this replaces

The originally-researched 0562 candidate, `ocirun exec --cgroup`
(joining the container's real cgroup for an exec'd process), was
investigated, partially implemented, and then **rejected and fully
reverted** after real testing surfaced a genuine regression: this
project's rootless containers commonly use a real, delegated systemd
user-session cgroup (`app.slice/...`), and the Linux kernel's cgroup
v2 migration permission model requires write access to the *nearest
common ancestor* cgroup of the writing process and the target — not
just the target's own `cgroup.procs`. An `ocirun exec` invocation
launched from outside the container's own delegated systemd unit
hierarchy (the normal, tested case) genuinely cannot write directly
to the container's `cgroup.procs`, confirmed by direct, hands-on
reproduction (`echo $$ > cgroup.procs` into a fresh, own-uid-owned,
`0644`-permission leaf cgroup under a real user systemd session's
`app.slice` fails with `EACCES`, even for the exact same user, when
the writer isn't already inside that delegated hierarchy). Properly
supporting this needs a new D-Bus `AttachProcesses`-based migration
mechanism (matching how `crate::launch` already handles the
systemd-driver case for a container's *first* process, via its own
`cgroup_ready_read` synchronization protocol) — a genuinely bigger,
separately-scoped future effort, not a safe single-sitting change.
That work was cleanly reverted (`git checkout --`) before landing
anything, and a fresh, narrower research pass produced this note's
own candidate instead.

## Real, checked-directly confirmation

- `~/git/runc/exec.go:86-93`: flag registration —
  `&cli.StringFlag{Name: "process-label", Usage: "set the asm
  process label for the process commonly used with selinux"}` and
  `&cli.StringFlag{Name: "apparmor", Usage: "set the apparmor
  profile for the process"}`.
- `~/git/runc/exec.go:244-249`: live consumption — `p.ApparmorProfile
  = ap` / `p.SelinuxLabel = l`, feeding libcontainer's own real
  LSM-label-write-at-process-start step.
- `~/git/crun/src/exec.c:83-84` (registration), `:146-151` (parsing),
  `:311-315` (`process->selinux_label`/`process->apparmor_profile`
  assignment) — the identical real shape in crun.
- Confirmed as a real gap directly, not guessed: `ocirun exec
  --apparmor=foo <id> -- true` was a hard clap "unexpected argument"
  parse error before this change.

## Real functional gap, not a faithful no-op — with a real, honest
limit

This project has **no SELinux/AppArmor/LSM subsystem anywhere at
all** (confirmed independently by `ocicri`'s own module doc comment).
So while accepting the flags at all closes a real CLI-compatibility
gap (real runc/crun users scripting against this project's `ocirun`
previously couldn't even invoke `exec --apparmor=`/`--process-label=`
without a hard parse error), this project can never actually *apply*
either label. The honest, checked-directly-precedented resolution:
an empty/omitted value (the overwhelming common case — neither
reference tool defaults it) is a true no-op; a real, non-empty value
is a clear, immediate "not yet supported" error rather than silently
pretending to apply a label this project can never enforce — the
exact same convention `ociman run --security-opt apparmor=`/`label=`
(`bin/ociman/src/main.rs`'s own `resolve_security_opts`) already
established for the identical real gap on the higher-level tool's own
side.

## Why this is narrow and safe

Pure CLI parsing plus one conditional error branch — zero kernel-
level namespace/cgroup/capability/privilege interaction of any kind.
The check happens at the dispatch site in `main()`, before
`cmd_exec` is ever called (failing fast, before opening the state
store at all, when a real value is given): `cmd_exec`'s own function
signature, and every other exec call site (`ociman exec`, `ocicri
ExecSync`, which don't share this CLI surface at all), are completely
unaffected. No new field is threaded through `ExecRequest` or any
persisted state; nothing is re-read later by `start`/`stop`/`kill`/
`delete`/`update`.

## Implementation

`Command::Exec` gains `process_label: Option<String>` (`#[arg(long =
"process-label", value_name = "VALUE")]`) and `apparmor:
Option<String>` (`#[arg(long = "apparmor", value_name = "VALUE")]`).
At the dispatch site, each is checked via `.filter(|v| !v.is_empty())`
before ever calling `cmd_exec`: a real, non-empty value is a clear
`anyhow::bail!` naming exactly which subsystem is missing.

## Tests

One new integration test in `tests/tests/ocirun_exec.rs`:
`exec_process_label_and_apparmor_reject_a_real_value_but_accept_an_
empty_one` — a real running container, proving both flags reject a
real value with a "not yet supported" error, accept an empty value,
and that a plain `exec` with neither flag at all still runs
normally.

Manually verified end to end beyond the automated test: a real
bundle built via `ocirun spec`, a real running container, `ocirun
exec --apparmor=foo`/`--process-label=foo` both confirmed to produce
the exact expected error; `--apparmor=`/`--process-label=` (empty)
and a plain `exec` with neither both confirmed to succeed identically.

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (128
test-result blocks, all passing — no new test file added, so the
block count is unchanged from `0561`; `RUST_TEST_THREADS=2` given
this host's own heavy, persistent concurrent-session CPU contention
this same day), `python3 ci/guards.py` (clean), `cargo deny check`
(clean), `bash ci/native-ci.sh` (two separate, isolated
`ocicri_container.rs` flakes under the same contention, each
independently confirmed transient by an isolated rerun, with a fully
clean run on the third attempt using `RUST_TEST_THREADS=2`), `bash
ci/build-deb.sh` (clean on the first attempt, real `dpkg -i`/
`--version`/`dpkg -r` round trip). A pure CLI-parsing-and-reject
addition — no hot path touched, no `ci/bench.sh` rerun needed.

## Deliberately still out of scope

Real SELinux/AppArmor support anywhere in this project at all remains
a real, separately-scoped, much bigger future effort — this closes
only the CLI-compatibility half (accept the real flag, fail loudly
and honestly on a real value) for `ocirun exec` specifically. The
originally-researched `--cgroup` topic (joining an exec'd process
onto the container's real cgroup hierarchy) remains open too, now
understood to need a genuinely new D-Bus-based migration mechanism
for the systemd-cgroup-driver case — see "A note on the candidate
this replaces" above.
