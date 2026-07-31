# Design note 0395: `linux.sysctl` + `ociman run/create --sysctl`

Status: implemented
Scope: `crates/oci-spec-types/src/runtime.rs`, `crates/oci-runtime-core/src/sysctl.rs` (new),
`crates/oci-runtime-core/src/launch.rs`, `crates/oci-runtime-core/src/lib.rs`,
`bin/ociman/src/main.rs`, `tests/tests/ocirun_run.rs`, `tests/tests/ociman_run.rs`,
`README.md`.

## What this closes

A real, previously-silent gap of the same shape `0393`/`0394` (umask,
`oom_score_adj`) just closed: `linux.sysctl` — a real, optional
runtime-spec field setting kernel parameters (`/proc/sys/...`) for a
container — had no representation anywhere in this project's own spec
types at all, and no code anywhere ever applied one. This project's
own `oci_runtime_core::validate` module already documented the gap
explicitly ("Not yet ported: sysctls, ..."). Real `podman run --sysctl
KEY=VALUE` is a genuine, common flag, completely absent from `ociman`.

## A real safety property, not just a missing feature

This project's own containers always share the host's real network
namespace (`Spec::into_rootless` unconditionally drops any `Network`
namespace — rootless containers here never get a private one). A
naive, unchecked `net.*` sysctl write would therefore silently modify
the *host's own* real networking configuration — a serious,
unexpected side effect no real container user would want. Real crun's
own `validate_sysctl` (`~/git/crun/src/libcrun/linux.c:4482-4543`)
already solves exactly this, checked directly: it validates every key
against an allow-list of recognized prefixes, each requiring the
matching namespace to actually be present in the container's own
declared namespace list —

- `fs/mqueue/*` and eight specific `kernel/*` keys (`msgmax`, `msgmnb`,
  `msgmni`, `sem`, `shmall`, `shmmax`, `shmmni`, `shm_rmid_forced`)
  require an IPC namespace.
- `kernel/domainname` requires a UTS namespace (crun also cross-checks
  it against the OCI spec's own separate `domainname` field, a check
  this project's own port deliberately skips — `oci-spec-types` has no
  `domainname` field at all to conflict with, only `hostname`).
- `kernel/hostname` is always rejected outright (conflicts with the
  OCI `hostname` field).
- `net/*` requires a Network namespace.
- anything else is rejected as "not namespaced" — crun uses a strict
  allow-list, not a deny-list.

Ported verbatim into `oci_runtime_core::sysctl::validate`. Since this
project's own rootless containers never have a real Network namespace
(and never will, per `into_rootless`'s own design), this validation
means a `net.*` sysctl request is **always** a clear, immediate error
here — not a limitation this note introduces, but the exact same real
protection crun itself already provides, now correctly inherited.

## Real, checked-directly confirmation

- `~/git/container-libs/vendor/github.com/opencontainers/runtime-spec/
  specs-go/config.go:243`: `Linux.Sysctl map[string]string`.
- `~/git/crun/src/libcrun/container.c:1336` (`libcrun_set_sysctl`,
  called right before `HANDLER_CONFIGURE_BEFORE_MOUNTS`): applied
  after namespaces are unshared but before the rootfs is ever mounted/
  `pivot_root`ed — the same relative position `0393`/`0394`'s own
  `oom::apply`/`identity::apply` already occupy, though sysctls
  genuinely need to run *after* `unshare(2)` (unlike those two plain,
  namespace-independent process attributes), since validation checks
  which namespaces actually exist.
- `libcrun_open_proc_file(container, "sys", O_DIRECTORY | O_PATH,
  err)`: the real path is `<proc_root>/sys/<name>` (dots translated to
  slashes) — **not** `<proc_root>/self/sys/...`. Unlike `/proc/<pid>/
  oom_score_adj`, `/proc/sys/` is already namespace-relative for
  whichever process opens it, with no `self/` component at all.
- `~/git/podman/cmd/podman/common/create.go`'s own `sysctlFlagName`
  (a repeatable string slice, `KEY=VALUE`) and `~/git/podman/pkg/
  util/utils.go`'s own `ValidateSysctls` (a CLI-level allow-list
  check, separate from and less precise than crun's own runtime-level
  namespace check — podman's own check would let a `net.*` value
  through at parse time, only for crun to reject it later at real
  container start).

## Implementation

- `oci_spec_types::runtime::Linux` gains `pub sysctl: BTreeMap<String,
  String>` (empty by default, omitted from serialized JSON when
  empty).
- New `oci_runtime_core::sysctl` module: `validate(key, namespaces)`
  (the allow-list port above) and `apply(proc_root, namespaces,
  &sysctl)` (validates then writes each entry, in `BTreeMap`'s own
  deterministic order, fail-fast on the first invalid one before any
  further writes).
- `launch.rs`'s `ChildSetup` gains a `sysctl: BTreeMap<String, String>`
  field; `ChildSetup::run()` calls `sysctl::apply` right after the
  session-keyring join (post-`unshare`, pre-rootfs/`pivot_root`,
  matching crun's own exact placement), passing `self.flags` (the
  container's own real, already-applied `UnshareFlags`) as the
  namespace-presence check crun's own `namespaces_created` bitmask
  equivalently provides. `exec.rs` is deliberately untouched, matching
  the confirmed real-crun/runc "container-creation-time only" scope
  (no call from either project's own exec path).
- `ociman run`/`create` gains `--sysctl KEY=VALUE` (repeatable, a new
  `parse_sysctls` doing only `KEY=VALUE` syntax validation — the
  deeper "is this key even meaningful for this container" check is
  deliberately left to the runtime layer's own `sysctl::validate`,
  matching real crun's own division of labor rather than duplicating
  podman's own weaker, separate CLI-level allow-list). `ocirun` needs
  no new flag: an explicit `linux.sysctl` in a hand-written
  `config.json` is now genuinely honored.

## Tests

Ten new unit tests in the new `oci_runtime_core::sysctl` module
covering every allow-list branch (IPC/UTS/network namespace presence
and absence, the always-rejected `kernel.hostname`, an unrecognized
prefix, deterministic multi-entry ordering, and the empty-map no-op).
Five new unit tests for `parse_sysctls` in `ociman`. Four new real,
end-to-end integration tests: `tests/tests/ocirun_run.rs`'s
`run_honors_an_explicit_sysctl_declared_in_the_bundle`/`run_rejects_a_
net_sysctl_declared_in_the_bundle` and `tests/tests/ociman_run.rs`'s
`run_sysctl_flag_sets_a_real_kernel_parameter`/`run_sysctl_flag_
rejects_a_net_key_since_there_is_no_real_network_namespace` — the
latter pair directly proving the real safety property above, not just
asserting an error code. All existing tests across `oci-runtime-core`
(243 pre-existing), `ocirun_run.rs` (25), and `ociman_run.rs` (103)
continue to pass unmodified.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures), `python3
ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh`, `bash
ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg -r` round trip).
This change touches `launch.rs`'s `ChildSetup::run` again (the same
shared hot-path primitive `0393`/`0394` already re-verified with a
full `ci/bench.sh` run) — done again rather than skipped: every figure
held at or improved on its own recorded baseline (`ocirun run` 3.2ms
vs `crun run` 7.1ms/`runc run` 21.3ms; `ociman run --rm`
6.13×/8.50× faster than `podman`/`docker`; `ociman exec`
15.84×/48.57× faster; `ociman rm` 40.32× faster; `ociman commit`
29.80× faster; `ociman build` 17.66-27.91× faster both cached and
uncached) — the added `sysctl::apply` call is a single, empty-map-
short-circuited no-op on every one of these measured paths.

## Deliberately still out of scope

`podman run --sysctl`'s own additional CLI-level allow-list check
(`ValidateSysctls`) is not ported at all — this project's own runtime-
level `sysctl::validate` is strictly more precise (it checks the
container's own *actual* declared namespaces, not a static list), so
duplicating podman's weaker check would add nothing. Every other
`LinuxContainerSecurityContext`/`Process`/`Linux` field surveyed
alongside `readonly_rootfs`/`privileged`/`resources`/`masked_paths`/
`capabilities`/`umask`/`oom_score_adj` (`0388`-`0394`'s own
"deliberately still out of scope" sections) remains a real, separate,
unrelated gap.
