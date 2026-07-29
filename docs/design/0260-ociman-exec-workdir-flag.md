# Design note 0260: `ociman exec -w`/`--workdir` (renamed from `--cwd`)

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_exec.rs`.

## A real, checked-directly drop-in-fidelity bug

`ociman run`/`create` already correctly use `-w`/`--workdir`, matching
real `podman run -w`/`docker run -w` exactly (the flag's own doc
comment even cross-references `ociman exec`'s "analogous override" —
but under the *wrong* name). `ociman exec` instead only accepted
`--cwd`, an entirely different flag name than real `podman exec -w`/
`--workdir` (confirmed directly against a real installed `podman exec
--help`). Any podman muscle-memory user typing `ociman exec -w /tmp
ctr cmd` today got a hard clap parse error — exactly the kind of
"should be a drop-in replacement" break this project exists to avoid.

(`ocirun exec --cwd` is correct as-is and untouched by this slice —
real `runc exec` also uses `--cwd`; only `ociman`'s own copy, at the
higher `podman`-equivalent layer, was wrong.)

## What changed

Pure CLI surface change: the `Exec` subcommand's field renamed
`cwd` → `workdir`, with `#[arg(short = 'w', long = "workdir")]`
replacing the old bare `#[arg(long)]` — no alias kept for the old
`--cwd` spelling (this project has no released/stable CLI compatibility
promise yet, still early development per its own README milestone
table, and real podman itself has no such alias for `exec` either, so
keeping one would just be extra surface not present in the tool being
emulated). `cmd_exec`'s own internal parameter name stays `cwd`
(matching the OCI runtime spec's own `Process.Cwd` field name,
unrelated to CLI flag naming).

## Verified

- The existing `exec_cwd_and_env_flags_override_the_defaults` test
  renamed to `exec_workdir_and_env_flags_override_the_defaults` and
  updated to pass `--workdir` — same real container, same assertions.
- A new test, `exec_workdir_short_flag_overrides_the_default`,
  confirms the short `-w` form (not just the long spelling) works
  identically, matching real `podman exec -w` exactly.
- Full workspace: `cargo build`/`test --workspace` (108 test
  binaries), `cargo fmt --all --check`, `cargo clippy --workspace
  --all-targets -- -D warnings`, `python3 ci/guards.py`, `cargo deny
  check`, `ci/native-ci.sh`, `ci/build-deb.sh`.
