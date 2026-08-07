# Design note 0559: `ociman attach`/`container attach --sig-proxy`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_attach.rs`,
`tests/tests/ociman_container.rs`.

## What this closes

`docs/design/0230` introduced `ociman attach` as deliberately
output-only, explicitly naming `--sig-proxy` (bundled with
`--no-stdin`/`--detach-keys`) as a flag offered nowhere at all.
`docs/design/0491`/`0504` both re-confirmed it as "a genuinely
separate, still-open ... gap ... left for its own future increment"
as recently as those two increments, with no later note closing it
since. This closes it: `--sig-proxy` (default `true`) is now offered
on both `ociman attach` and its `ociman container attach` alias.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/attach.go:52`: flag
  registration — `flags.BoolVar(&attachOpts.SigProxy, "sig-proxy",
  true, "Proxy received signals to the process")`.
- `~/git/podman/pkg/domain/infra/abi/containers.go:872`: live call
  site — `ContainerAttach` calls `terminal.StartAttachCtr(...,
  options.SigProxy, false)`.
- `~/git/podman/pkg/domain/infra/abi/terminal/sigproxy_commn.go:
  18-52`: the real, live consumer — `ProxySignals` catches every
  signal via `signal.CatchAll`, forwarding each one via `ctr.Kill
  (uint(syscallSignal))` (an ordinary `kill(pid, sig)` on the
  container's own init pid) in a background goroutine, until the
  container dies or is detached from.
- `~/git/podman/pkg/signal/signal_linux.go:92-101`:
  `isSignalIgnoredBySigProxy`'s own exact exclusion set — `SIGCHLD`
  (meant for the CLI process itself), `SIGPIPE` (ditto), `SIGURG`
  (Go 1.14 preemption noise), `SIGSTOP` (can never be caught by any
  process at all). `SIGKILL` is implicitly excluded too, for the
  identical un-catchable reason, even though the Go exclusion list
  doesn't name it explicitly (Go's own signal-catching API silently
  ignores a request to catch it).

## Real functional gap, not a no-op

Before this, a real `Ctrl-C` (or any other signal) sent to `ociman
attach` itself only killed the *attach* process (default Rust
disposition) while the container kept running completely untouched —
a real, observable divergence from real podman/docker, where the
same signal is, by default, forwarded straight into the container
(and typically kills it too, since most simple containers have no
trap of their own). Live-verified by hand: a container running `sh -c
'trap "echo GOT_TERM; exit 42" TERM; ...'`, attached to, then sent a
real `SIGTERM` from a separate shell targeting the `attach` process's
own pid — with `--sig-proxy` (the default), the trap fired inside the
container (`GOT_TERM` printed, container's own real exit code `42`,
container genuinely stopped); with `--sig-proxy=false`, the `attach`
process died immediately from the plain, unhandled signal and the
container was left running, completely untouched.

## Why this is narrow

Entirely contained to one process's one blocking call:
[`cmd_attach`]/[`attach_and_wait_for_exit`]. The container's own pid
is already available with zero new persisted state —
`PersistedState::pid` is already recorded at `create`/`start` time
for every other command's own use, and `effective_status() ==
Running` (already checked right above) already guarantees it's
`Some`. A temporary signal handler is installed for the duration of
this one invocation and never torn down: `cmd_attach` already calls
`std::process::exit` immediately after its one attach loop returns —
the same real "the whole process exits right after, so nothing needs
restoring" reasoning real podman's own identical `ProxySignals`
relies on too (it never itself calls a `signal.Reset`-equivalent
either). No `run`/`create`/`start`/`kill`/`delete`/`exec`/`update`
call site needs any change at all.

## Implementation

- `Command::Attach` and `ContainerCommand::Attach` both gain
  `sig_proxy: bool` (`#[arg(long = "sig-proxy", default_value_t =
  true, num_args = 0..=1, default_missing_value = "true", action =
  clap::ArgAction::Set)]` — the same established pattern
  `Command::Run::tls_verify` already uses for a default-`true` flag
  that still accepts an explicit `--sig-proxy=false`).
- `cmd_attach` gains a `sig_proxy: bool` parameter; when true, calls
  the new `install_sig_proxy(pid)` before entering its own existing
  polling loop.
- `install_sig_proxy`: stores the target pid into a new
  `static SIG_PROXY_TARGET_PID: AtomicI32`, then installs a real,
  `extern "C"` handler (`sig_proxy_handler`) via raw `libc::sigaction`
  for every signal in a new `SIG_PROXY_FORWARDED_SIGNALS` constant —
  every signal name `oci_runtime_core::signal` already recognizes,
  minus `SIGCHLD`/`SIGPIPE`/`SIGURG`/`SIGSTOP`/`SIGKILL` per the
  citations above. The handler itself does a plain, relaxed atomic
  load of the target pid followed by `libc::kill(pid, signal)` — both
  async-signal-safe operations, the standard, well-established
  pattern for a minimal signal-forwarding handler in Rust with no new
  dependency (this project's own established "never add an
  unnecessary runtime dependency" convention — no `signal-hook`/
  `ctrlc` crate needed; `libc` is already a dependency everywhere).

## Tests

Two new integration tests in `tests/tests/ociman_attach.rs`:
`attach_sig_proxy_forwards_a_real_signal_into_the_container` (spawns
a real `ociman attach` as a background process, sends it a real
`SIGTERM` from the test itself, and proves the signal reached the
container via a real trap handler's own distinguishing output and
exit code) and `attach_sig_proxy_false_leaves_the_container_running`
(the same setup with `--sig-proxy=false`, proving the `attach`
process dies from the default disposition while the container stays
completely untouched). One new test in `tests/tests/ociman_container.
rs`: `container_attach_accepts_sig_proxy_false` (proves the alias
accepts and correctly threads the flag too, without re-testing full
forwarding semantics a second time).

Manually verified end to end beyond the automated tests, exactly as
described in "Real functional gap" above.

Full workspace: `cargo build --workspace --locked` (clean, after
fixing two build-time warnings from the raw `sigaction`/function-
pointer cast — `#[allow(unsafe_code)]` with a `SAFETY` comment on
each `unsafe` block, matching this project's own established
convention, and casting through `*const ()` first per the compiler's
own `function_casts_as_integer` suggestion), `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean). `cargo test --workspace --locked` needed one real
test fix along the way: the new `container_attach_accepts_sig_proxy_
false` test originally ran the container as a bare `exit 0` — fast
enough that it had sometimes already stopped by the time `attach`
itself ran, a genuine test-timing bug (not environmental flakiness),
fixed by giving it a real `sleep 0.3` first, the same pattern every
other `attach`-adjacent test in this project already uses. After that
fix: 128 test-result blocks, all passing — needed several attempts
under this host's own heavy, persistent concurrent-session CPU
contention today (an `ociman_run.rs` cgroup-conf flake and two
separate `ocicri_container.rs` flakes, each independently confirmed
transient by an isolated rerun), with a fully clean run landing using
`RUST_TEST_THREADS=2`. `python3 ci/guards.py` (clean), `cargo deny
check` (clean), `bash ci/native-ci.sh` (clean on the first attempt
using `RUST_TEST_THREADS=2` from the start, given the same day's
contention), `bash ci/build-deb.sh` (clean on the first attempt, real
`dpkg -i`/`--version`/`dpkg -r` round trip). `ociman attach` is not
exercised by `ci/bench.sh` at all, and the added cost when
`--sig-proxy` is on (the default) is a one-time, ~26-`sigaction(2)`-
call setup before the existing polling loop begins, not a repeated
hot-path cost — no benchmark rerun needed.

## Deliberately still out of scope

`--no-stdin`/`--detach-keys` remain a genuinely separate, still-open
architectural gap (this project's own current architecture only ever
wires up a container's stdin once, at its original `run`/`create`
time, with no live channel an already-detached, already-running
container's own stdin could be reattached to later) — `--sig-proxy`
was always separable from that (a one-directional, host-to-container
forwarding concern needing no reattachable stdin channel at all),
exactly as `0504`'s own note already anticipated.
