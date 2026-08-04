# Design note 0408: `ocirun exec --no-new-privs`

Status: implemented
Scope: `bin/ocirun/src/main.rs`, `tests/tests/ocirun_exec.rs`,
`README.md`.

## What this closes

Real `runc exec --no-new-privs`/`crun exec --no-new-privs` had no
`ocirun exec` equivalent, despite the underlying primitive already
being fully wired: `oci_runtime_core::exec::ExecRequest::no_new_
privileges` already existed and was already applied by `cmd_exec`,
just hardcoded to inherit the container's own declared `process.
noNewPrivileges` with no per-exec CLI override at all.

## Real, checked-directly confirmation

- `~/git/runc/exec.go`: a plain `cli.BoolFlag` (`"no-new-privs"`);
  `if cmd.IsSet("no-new-privs") { p.NoNewPrivileges = cmd.Bool(...) }`
  — since a bare boolean CLI flag with no explicit value syntax can
  only ever be "given" (true) or "not given" (untouched), this is, in
  practice, "given at all forces `true`; not given leaves the
  container's own already-declared value alone."
- `~/git/crun/src/exec.c`: identical real shape —
  `if (exec_options.no_new_privs) process->no_new_privileges = 1;`,
  with its own doc comment noting the base spec's own default is
  otherwise `true` unless the container's own spec explicitly says
  `false`.

## Implementation

`Command::Exec` gains `no_new_privs: bool` (`--no-new-privs`);
`cmd_exec`'s existing `no_new_privileges: process_spec.no_new_
privileges` becomes `no_new_privileges: no_new_privs ||
process_spec.no_new_privileges` — a one-line, zero-cost-when-unused
change (a single boolean `||`) on the exact same `ExecRequest`
construction this command already builds on every real invocation.

## Tests

One new end-to-end integration test in `tests/tests/ocirun_exec.rs`,
`exec_no_new_privs_flag_forces_it_on_regardless_of_the_containers_
own_declared_value` — a real container whose own bundle explicitly
declares `process.noNewPrivileges: false`, proving `exec` with no
flag still inherits that `false` (a real, live `/proc/self/status`
`NoNewPrivs:\t0` read from inside the exec'd process), while `exec
--no-new-privs` forces `NoNewPrivs:\t1` regardless. All existing
tests continue to pass unmodified (15/15 in `ocirun_exec.rs`).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures — one
unrelated, pre-existing, load-sensitive `ocicri_container.rs`
`ExecSync` flake was observed twice across repeated full-workspace
runs, a different specific test each time, consistent with the
already-documented, already-investigated environmental issue this
project's own test file already carries debug instrumentation for;
not touched here), `python3 ci/guards.py`, `cargo deny check`, `bash
ci/native-ci.sh`, `bash ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip). This touches `cmd_exec`, part of `ocirun
exec`'s own hot path `ci/bench.sh` measures directly, so it was
re-run: `ocirun exec` held at 1.70×/9.10× faster than `crun`/`runc
exec` (previously 1.61-1.62×/9.17-9.57×, within the same real,
noisy-single-host-measurement range this project's own benchmark
methodology has always shown run to run) — the added check is a
single boolean `||` with no new syscalls or allocations.

## Deliberately still out of scope

`--console-socket`/`--tty` (real PTY allocation — an already-
documented, project-wide gap), `--pidfd-socket`/`--cgroup` (niche),
`--process-label`/`--apparmor` (no SELinux/AppArmor support anywhere
in this project), `--process` (an alternate JSON-file process-spec
input mode), and `--detach` (a materially different wait/foreground
model) remain unimplemented — each a real, separate, bigger gap than
this single boolean flag.
