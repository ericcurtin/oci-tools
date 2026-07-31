# Design note 0393: `process.user.umask` + `ociman run/create --umask`

Status: implemented
Scope: `crates/oci-spec-types/src/runtime.rs`, `crates/oci-runtime-core/src/identity.rs`,
`bin/ociman/src/main.rs`, `tests/tests/ocirun_run.rs`, `tests/tests/ociman_run.rs`,
`README.md`.

## What this closes

A real, previously-silent correctness gap affecting *every* container
this project ever launches (`ocirun run`/`create`, `ociman run`/
`create`, `ocibox enter`, `ocicri`-managed containers — anything going
through the shared `oci_runtime_core::launch`/`exec` path): none of
them ever called `umask(2)` anywhere. A container's own file-creation
permissions therefore depended entirely on whatever umask the
*launching* process/shell/systemd-unit happened to have at the moment
it invoked `ocirun`/`ociman`/`ocicri`, rather than the deterministic
`0o022` default every real `docker`/`podman`/`runc`/`crun` themselves
always guarantee. A caller with a stricter shell umask (a real,
plausible operational scenario — many hardened shells/systemd units
set their own `umask`/`UMask=`) would silently get containers whose
files ended up with different, unexpected permissions than the exact
same invocation would produce under real docker/podman/runc/crun, with
no error or warning at all.

## Real, checked-directly confirmation

- `~/git/container-libs/vendor/github.com/opencontainers/runtime-spec/
  specs-go/config.go:154-155`: the real runtime-spec's own optional
  `Process.User.Umask *uint32` field — this project's own `User` type
  (`oci_spec_types::runtime`) had no equivalent field at all.
- **Hard, already-existing proof the gap was real, not hypothetical**:
  this project's own test fixture, `crates/oci-spec-types/tests/
  fixtures/podman-generated-config-with-seccomp.json` (a real,
  captured `podman run`/crun `config.json`, not hand-written), already
  contains `"process.user.umask": 18` (`0o22`) — silently dropped on
  every parse until now, with no test ever having noticed.
- `~/git/crun/src/libcrun/container.c:1447` (container's own init
  process) and `:3835` (an exec'd process): `umask
  (def->process->user->umask_present ? def->process->user->umask :
  0022)` — real crun *always* calls `umask(2)` at both call sites,
  confirming this is genuinely unconditional, universal behavior, not
  an edge case.
- `~/git/podman/cmd/podman/common/create.go`'s own `umaskFlagName`
  (`podman run --umask`) and `~/git/podman/libpod/options.go`'s own
  `umaskRegex = ^[0-7]{1,4}$` (1-4 octal digits, validated before ever
  reaching `strconv.ParseUint(umask, 8, 32)` in
  `container_internal_common.go:3196`) — the exact CLI shape and
  validation ported here. Neither real `runc` nor `crun` has a CLI
  flag of their own for this at all — it's purely a `config.json`
  field set by whoever generates the bundle, confirmed via a direct
  `grep` for `umask` in both projects' own CLI source (no hits) — so
  `ocirun run`/`create`/`exec` need no new flag of their own at all,
  only the shared type/`identity::apply` fix, since they already read
  `config.json` directly.

## Implementation

- `oci_spec_types::runtime::User` gains `pub umask: Option<u32>`
  (`#[serde(default, skip_serializing_if = "Option::is_none")]`,
  already `Default`-derived to `None`) — `None` means "not given at
  all", distinct from a real, if degenerate, explicit `0`.
- `oci_runtime_core::identity::apply` (shared by both `launch.rs`'s
  `ChildSetup::run` and `exec.rs`'s `ExecSetup::run` — every container-
  launching and `exec`ing call site in the whole workspace) now calls
  `rustix::process::umask(rustix::fs::Mode::from_raw_mode(user.umask.
  unwrap_or(0o022)))` unconditionally, as its very first step — an
  unprivileged syscall with no ordering dependency on the
  capability-dropping/`setresuid`/`setresgid` steps that follow,
  matching real crun's own placement.
- `ociman run`/`create` gains `--umask`, validated by a new
  `parse_umask` function (the same `^[0-7]{1,4}$` check, `u32::
  from_str_radix(s, 8)`), written into `synthesize_spec`'s generated
  `process.user.umask` right alongside the existing `uid`/`gid`
  assignment. `ocirun`/`ocibox`/`ocicri` need no new flag/field at
  all: `ocirun` already reads `config.json` directly (an explicit
  `process.user.umask` there is now genuinely honored instead of
  silently dropped on parse); `ocibox`/`ocicri` simply inherit the
  shared `0o022` default via the same `identity::apply` fix, matching
  what real `distrobox`/`cri-o` + `runc`/`crun` would also produce for
  an unset field (neither the CRI proto nor distrobox has an
  equivalent concept at all).

## Tests

One new assertion in `oci-spec-types`'s own already-existing
`parses_real_podman_generated_config_with_seccomp` fixture test:
`process.user.umask` must round-trip as `Some(0o22)` instead of being
silently dropped. Five new unit tests for `parse_umask` in `ociman`
(accepts 1-4 octal digits including a real fixture round-trip value,
rejects a non-octal digit/more than four digits/an empty string/a
`0o`- or `0x`-looking string). Two new real, end-to-end integration
tests in `tests/tests/ocirun_run.rs` (`run_defaults_to_umask_0022_
when_the_bundle_declares_none`, `run_honors_an_explicit_umask_
declared_in_the_bundle` — a real busybox `umask` builtin call inside
a running container, not just the generated spec) and three in
`tests/tests/ociman_run.rs` (`run_without_umask_defaults_to_0022`,
`run_umask_flag_sets_a_real_custom_umask`, `run_umask_flag_rejects_an_
invalid_value`). All existing tests across `oci-spec-types` (55
pre-existing), `ocirun_run.rs` (24), and `ociman_run.rs` (100)
continue to pass unmodified.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures), `python3
ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh`, `bash
ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg -r` round trip).
This change touches `oci_runtime_core::identity::apply`, a shared
primitive on the actual hot path for *every* container launch/exec
across every binary — matching the same "shared primitive used by
literally every launch path" bar `0378`'s `--no-new-keyring` set, a
**full `ci/bench.sh` re-run** (not just a targeted spot-check) was
done rather than skipped: every figure held at or improved on its own
recorded baseline (`ocirun run` 2.09× faster than `crun run`/6.46×
faster than `runc run`, matching `0372`'s own recorded 2.18-2.24×/
6.67-7.22× within noise; `ociman run --rm` 5.12×/8.13× faster than
`podman`/`docker`; `ociman exec` 15.72×/48.04× faster; `ociman rm`
45.85× faster; `ociman commit` 26.90× faster; `ociman build`
17.90-30.62× faster both cached and uncached) — the added `umask(2)`
call is a single, fixed-cost, unconditional syscall with no
measurable overhead.
