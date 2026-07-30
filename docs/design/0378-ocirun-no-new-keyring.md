# Design note 0378: `ocirun run`/`ocirun create --no-new-keyring`

Status: implemented
Scope: `crates/oci-runtime-core/src/keyring.rs` (new),
`crates/oci-runtime-core/src/launch.rs`, `crates/oci-runtime-core/src/
lib.rs`, `bin/ocirun/src/main.rs`, `bin/ociman/src/main.rs`,
`bin/ociman/src/build.rs`, `bin/ocibox/src/main.rs`, `bin/ocivmm/src/
main.rs`, `bin/ocicri/src/launcher.rs`, `tests/tests/ocirun_run.rs`,
`README.md`.

## What this closes

A real, previously-unnoticed isolation gap, found while researching
this exact flag: real `runc run`/`crun run` both join a fresh,
container-scoped session keyring for every container by default
(`--no-new-keyring` is the opt-out). Grepping this crate's entire tree
for `keyctl`/`keyring` before this change: zero hits anywhere — every
`ocirun`-launched container previously silently shared whatever
session keyring `ocirun` itself happened to have, with no isolation of
its own at all. Since `launch::run_reporting_pid`/`launch::run`/
`launch::create` are shared by every container-launching binary in
this project (`ociman`, `ocibox`, `ocivmm`, `ociman build`'s own `RUN`
steps, `ocicri`), this gap affected all of them, not just `ocirun`
itself.

## Real, checked-directly confirmation

- `~/git/runc/libcontainer/standard_init_linux.go`'s `Init()`: the
  very first thing it does (before rootfs setup, before capability
  drop, before seccomp, long before `execve`) is `keys.
  JoinSessionKeyring(ringname)` → `unix.KeyctlJoinSessionKeyring` → a
  raw `SYS_KEYCTL` syscall, operation `KEYCTL_JOIN_SESSION_KEYRING`
  (`1`), ring name `"_ses." + containerID`. `setns_init_linux.go`
  (`runc exec`'s own init path) does the identical join, using the
  *same persisted* `NoNewKeyring` config from container-creation time
  — there's no separate per-exec flag in real runc.
- `~/git/crun/src/libcrun/linux.c`'s `syscall_keyctl_join`: the exact
  same syscall/args (`syscall(__NR_keyctl, KEYCTL_JOIN_SESSION_KEYRING,
  name, 0)`), ring name is literally the container's own id (not
  `_ses.`-prefixed). Called from `container.c`'s `setup_container_
  keyring`, in the **parent** process, before `libcrun_run_linux_
  container`'s `clone()` — the forked child simply inherits the
  freshly-joined ring (session keyrings are inherited across
  `fork`/`clone`/`execve`, confirmed via `man 7 session-keyring`).
  crun's own `exec.c`/`libcrun_container_exec*` never touch the
  keyring at all — no rejoin, unlike runc.
- `man 2 keyctl`/`man 7 session-keyring`: `KEYCTL_JOIN_SESSION_KEYRING`
  is an ordinary, unprivileged operation for a *fresh* named keyring —
  no capability required, and nothing about it is conditioned on the
  calling process's own user-namespace identity. `ocirun`'s own
  default rootless (fresh user namespace) setup is no obstacle.
- `libc` (the vendored crate) has no `keyctl(2)` wrapper at all
  (matching real glibc, which doesn't either) and doesn't publicly
  expose `KEYCTL_JOIN_SESSION_KEYRING` (defined only in a
  crate-private module) — so, matching real crun's own identical
  `#define`, this defines its own local constant and calls the raw
  `libc::syscall(libc::SYS_keyctl, ...)` directly.
- `oci-spec-types`: no `config.json`/runtime-spec field exists for
  this at all (confirmed by grep) — matching how both real runc's
  `NoNewKeyring` and crun's `context->no_new_keyring` live purely as
  CLI-populated internal state, never round-tripped through
  `config.json`'s own JSON. So this is pure CLI/`ChildSetup` state, no
  `oci-spec-types` change needed.

## Implementation

- New `crates/oci-runtime-core/src/keyring.rs`: `pub fn join_session_
  keyring(name: &str) -> io::Result<()>` — the raw syscall wrapper
  described above, tolerating `ENOSYS` (old/`CONFIG_KEYS`-disabled
  kernel) exactly like both reference runtimes do, surfacing any other
  error.
- `ChildSetup` gains a plain `id: String` field (previously the
  container id was only stored inside the *optional* `ContainerHooks`,
  populated only when hooks exist — this needs it unconditionally) and
  a `no_new_keyring: bool` field, mirroring `no_pivot`'s own existing
  shape exactly.
- `mount_pivot_and_exec` calls `keyring::join_session_keyring(&self.
  id)` (using the container's own id as the ring name, matching crun's
  choice rather than runc's `_ses.`-prefixed one) right after the
  existing hooks-readiness wait, unconditionally before the `pivot_
  root` planning loop and `identity::apply` call further down — the
  same real ordering both reference runtimes use (keyring first,
  rootfs/capabilities/seccomp/`exec` after), gated on `!self.no_new_
  keyring`.
- `launch::run`/`launch::create`/`launch::run_reporting_pid` each gain
  a new `no_new_keyring: bool` parameter (mirroring `no_pivot`'s own
  existing plumbing exactly). Every non-`ocirun` call site (`ociman`'s
  own `cmd_run`/`build.rs`'s `RUN`-step runner, `ocibox enter`, `ocivmm
  create`, `ocicri`'s container launcher) passes a hardcoded `false` —
  none of those layers have (or need) an equivalent CLI flag of their
  own, exactly matching the existing precedent `no_pivot`/
  `preserve_fds` already established for flags that are genuinely
  `ocirun`-CLI-only.
- `ocirun`'s own `Command::Run`/`Command::Create` both gain a real
  `--no-new-keyring` flag, threaded through `cmd_run`/`run_and_
  finalize`/`cmd_create` down to the `launch` calls above.

**Deliberately not implemented for `ocirun exec`** (joining an
already-running container) — matching real crun's own identical
choice (checked directly: crun's `exec.c` never touches the keyring at
all), not real runc's own exec-time rejoin of the container's already-
existing named ring. Real runc's rejoin needs the original `NoNewKeyring`
setting persisted from container-creation time for a later, separate
`exec` invocation to read back — a real capability this project's
architecture doesn't have today (`ocirun`'s own persisted `state.json`
has no equivalent field, and this project's established pattern favors
crun's narrower model over runc's richer one in exactly this kind of
comparable-but-not-identical case). Called out explicitly in `Child
Setup::no_new_keyring`'s own doc comment as a deliberate, documented
divergence rather than an oversight.

## Tests

New unit tests in `crates/oci-runtime-core/src/keyring.rs` (a real,
live syscall against whatever kernel the test actually runs on, plus a
NUL-byte-rejection case). Two new integration tests in `tests/tests/
ocirun_run.rs`, both real, kernel-level verifications rather than
persisted-JSON checks: `run_joins_a_new_session_keyring_named_after_
the_container_id_by_default` (a bundle with `/proc/keys` un-masked
just for this one test — real runc/docker/this project all mask it by
default for good reason, never disturbed for ordinary containers —
confirms a real keyring literally named after the container's own id
appears) and `run_no_new_keyring_skips_joining_a_new_session_keyring`
(the direct contrast: no such keyring appears at all with the flag
given). All 22 tests in the file (20 pre-existing + 2 new) pass; every
other pre-existing test across the whole workspace that launches any
container at all (`ocirun_lifecycle.rs`, `ocirun_hooks.rs`, every
`ociman_*`/`ocicri_*`/`ocibox_*` test) continues to pass unmodified
too, confirming the new unconditional-by-default keyring join breaks
nothing.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures, full clean
run), `python3 ci/guards.py`, `cargo deny check`, `bash
ci/native-ci.sh`, `bash ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip). This change touches the single shared
container-launch path every binary in this project uses — ran the
**full** `ci/bench.sh` suite (not just a targeted subset, given the
breadth of what this touches): every figure (`ocirun run` 3.5ms,
`ocirun exec` 2.1ms, `ociman exec` 2.8ms, `ociman run --rm` 35.1ms,
`ociman run -d` 37.5ms, `ociman rm` 1.7ms, `ociman commit` 3.9ms,
`ociman build --no-cache` 64.0ms, `ociman build` cached 8.7ms) landed
within the existing `docs/benchmarks.md` baseline's own noise band, no
regression — a single, extremely cheap `keyctl` syscall per container
start, as expected.
