# Design note 0376: `ociman run`/`ociman create --ulimit`

Status: implemented
Scope: `bin/ociman/Cargo.toml`, `bin/ociman/src/main.rs`,
`crates/oci-runtime-core/src/rlimits.rs`, `tests/tests/ociman_run.rs`,
`README.md`.

## What this closes

`ociman run`/`ociman create` had no `--ulimit` flag at all (confirmed:
absent from `ociman run --help`'s 42-flag surface) — real `docker run
--ulimit`/`podman run --ulimit`'s way of setting a container's own
`RLIMIT_*` resource limits (max open files, max processes, core dump
size, ...). The backend was already 100% built and unit-tested:
`oci-runtime-core::rlimits::apply` already implements every one of the
15 real names this closes, already wired unconditionally into
`launch::run_reporting_pid`'s own `ChildSetup`. This was purely a
missing CLI-to-`config.json` plumbing gap — no new low-level primitive
needed.

## Real, checked-directly confirmation

`~/git/moby/vendor/github.com/docker/go-units/ulimit.go`'s own
`ulimitNameMapping`/`ParseUlimit`: `NAME=soft[:hard]`, `-1` on either
side means unlimited, a bare `NAME=value` (no `:hard`) sets both soft
and hard to the same value, and `as` is deliberately excluded from the
name table ("doesn't seem usable with the way Docker inits a
container") even though `RLIMIT_AS` is otherwise fully supported —
matched here exactly, including the exclusion.

## A real, previously-unnoticed divergence found while verifying end to
   end

Manually running `ociman run --ulimit nofile=-1:-1 ...` failed with a
real `EPERM` from `setrlimit(2)` — an unprivileged process can never
actually raise a rlimit's own hard ceiling without `CAP_SYS_RESOURCE`,
which neither this project's own default nor `--privileged`'s wider
capability set grants. Checked directly against a real installed
`podman run --ulimit nofile=-1:-1`: it does **not** fail this way at
all, reporting a real, large *number* (`500000` on this host, matching
this test host's own live `ulimit -Hn`), not the literal
`RLIM_INFINITY`. Read `~/git/podman/pkg/specgen/generate/oci.go`'s own
`addRlimits` → `~/git/podman/pkg/util/rlimit.go`'s own
`ClampRlimitToHost`: in rootless mode, a `-1` on either side is
silently translated to the *calling* process's own real, current
`getrlimit(2)` hard ceiling for that resource instead of the raw,
unachievable sentinel — exactly what a forked child would inherit
unchanged anyway. `ociman` always runs every container inside a fresh
user namespace (`synthesize_spec`'s own unconditional `into_rootless`),
so this clamp always applies here, unlike real podman's own conditional
(which only takes this path when the whole podman *process* itself is
rootless — a distinction this project's always-userns'd design makes
moot). Ported as `clamp_ulimit_to_host`, applied as a separate mapping
step after `parse_ulimit` (mirroring real podman's own two-step
`GenRlimits`-then-`ClampRlimitToHost` structure) — `oci_runtime_core::
rlimits::resource_named` (previously private) was made `pub` so this
client-side clamp can reuse the exact same name→`Resource` table
`apply` itself already uses as its single source of truth, rather than
duplicating it.

This clamp is deliberately `ociman`-only, not `ocirun`/`oci_runtime_
core` — real `runc run`/`crun run` do not perform any such clamping
themselves either (it's a podman-client-side compatibility shim, not a
runtime-level concern); `ocirun`'s own `oci_runtime_core::rlimits::
apply` correctly keeps its existing, faithful pass-through-and-fail
behavior for a `config.json` that genuinely asks for something
unachievable, matching real crun/runc exactly.

## Implementation

- `RunArgs` (shared by `Command::Run`/`Command::Create` via
  `#[command(flatten)]`) gains `ulimit: Vec<String>`.
- `prepare_container` (the single function `synthesize_spec` is called
  from) parses+clamps eagerly, alongside its existing `--stop-signal`
  validation: `args.ulimit.iter().map(|u| parse_ulimit(u).map(clamp_
  ulimit_to_host)).collect()`.
- `synthesize_spec` gains a `rlimits: &[PosixRlimit]` parameter, setting
  `process.rlimits = rlimits.to_vec()` right next to the existing
  `process.no_new_privileges` line.
- `parse_ulimit`/`ulimit_rlimit_name`/`parse_ulimit_value` (new, `bin/
  ociman/src/main.rs`): pure parsing/validation, no syscalls — mirrors
  `parse_memory_limit`'s own existing style. A soft value exceeding a
  *given* (non-unlimited) hard value is a clear, immediate error,
  matching real docker's own identical validation.
- `clamp_ulimit_to_host` (new): the real `getrlimit(2)`-based clamp
  described above, kept as a separate function from `parse_ulimit` so
  the latter stays pure and unit-testable without a live syscall.
- `bin/ociman/Cargo.toml` gained a plain `libc.workspace = true`
  dependency (for `libc::RLIM_INFINITY`, the same sentinel `oci_
  runtime_core::rlimits` already uses) — `rustix::process::getrlimit`
  was already reachable via the existing `rustix` dependency's
  `"process"` feature (added for `0375`'s own detach keeper).

## Tests

Six new unit tests in `bin/ociman/src/main.rs` for `parse_ulimit`
(every real name, `as`'s deliberate exclusion, bare-value/soft-equals-
hard, `-1` handling, soft-exceeds-hard rejection, malformed input).
Five new integration tests in `tests/tests/ociman_run.rs`, each a real,
kernel-enforced verification via busybox `ash`'s own `ulimit -n`/
`ulimit -Hn` builtins (real `getrlimit(2)` reads, not this project's
own bookkeeping): distinct real soft/hard values; a bare value setting
both; an unrecognized name; soft exceeding hard; and `-1` clamping to
this test host's own real ceiling (queried the same way, directly, so
the assertion holds on any host rather than hardcoding a number).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures), `python3
ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh`, `bash
ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg -r` round trip).
This change touches `ociman run`'s own spec-synthesis path (a directly
`ci/bench.sh`-measured hot path when no `--ulimit` is given at all, the
overwhelmingly common case) — ran the full `ci/bench.sh` suite: `ociman
run --rm` 34.6ms/`run -d` 40.6ms/`rm` 1.6ms/`commit` 3.3ms/`exec`
2.7ms, all within the existing baseline's own noise band (`docs/
benchmarks.md`'s recorded 32.7/32.8/1.7/3.4/2.8ms respectively), no
regression.
