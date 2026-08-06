# Design note 0509: `ocirun exec --process`/`-p`

Status: implemented
Scope: `bin/ocirun/src/main.rs`, `tests/tests/ocirun_exec.rs`.

## What this closes

`0408` first flagged `--process` as "an alternate JSON-file process-
spec input mode" and deferred it alongside `--console-socket`/`--tty`
(real PTY allocation, an already-documented, project-wide gap) and
`--detach` (a materially different wait/foreground model), grouped
together as "each a real, separate, bigger gap than [a] single
boolean flag" with no further individual justification. Re-examined
directly this time (the same re-examination `0499` did for `container
diff`, which turned out simpler than `0482`'s own original grouping
assumed): `--process` turns out to need neither a new subsystem nor a
new dependency — it reuses the exact same `oci_spec_types::runtime::
Process` struct real container bundles already use, with its own
already-`camelCase`-renamed `Deserialize` already matching the real
OCI runtime-spec JSON shape verbatim.

## Real, checked-directly confirmation

- `~/git/runc/exec.go`: a plain `cli.StringFlag` (`"process"`/`"p"`,
  `Usage: "path to the process.json"`). `getProcess`: given this
  flag, opens and JSON-decodes the file straight into a
  `*specs.Process`, calls `validateProcessSpec` on it, and returns
  immediately — every other CLI flag (`--cwd`/`--apparmor`/
  `--process-label`/`--cap`/etc., and the `COMMAND` positional args)
  is never even read in that branch, only reached in the `else`
  branch (built from `spec.Process` + CLI flags) when `--process`
  isn't given at all.
- `~/git/runc/utils_linux.go:354-370` (`validateProcessSpec`): the
  exact real validation — a non-empty, absolute `Cwd`, and a
  non-empty `Args`.
- `~/git/crun/src/exec.c:73,167,274,291-292`: the identical real
  shape — `-p`/`--process FILE`; `crun_assert_n_args(argc - first_arg,
  exec_options.process ? 1 : 2, -1)` (only the container ID required
  on the command line when `--process` is given, versus container ID
  + at least one command word otherwise); `exec_opts.path =
  exec_options.process` set directly, with the entire `else` branch
  (building a process from `--cwd`/`--user`/`--cap`/etc. + `argv`)
  skipped entirely — never merged with the file.

## Implementation

`Command::Exec` gains `process: Option<PathBuf>` (`--process`/`-p`).
`args: Vec<String>` loses its own previous `required = true` (needed
to make `--process` alone, with no trailing command, valid at the
clap level) — the previous "must give a command" requirement is
re-enforced at runtime instead, in the branch where `--process` isn't
given, matching real runc's own identical `"exec args cannot be
empty"` wording exactly.

`cmd_exec` branches on `process`:
- Given: reads the file, deserializes it directly into
  `oci_spec_types::runtime::Process` (zero new type, zero new
  dependency — the exact same struct real bundles' own `config.json`
  `process` section already round-trips through), validates `cwd`
  (non-empty, absolute) and `args` (non-empty) matching real runc's
  own `validateProcessSpec` wording verbatim, and uses its `user`/
  `capabilities`/`no_new_privileges`/`cwd`/`env`/`args` fields
  directly for `ExecRequest` — every other CLI flag (`--user`/
  `--cwd`/`--env`/`--cap`/`--no-new-privs`, and any trailing
  `COMMAND`) is silently unused, exactly matching both reference
  runtimes' own identical early-return shape.
- Not given: the exact pre-existing per-flag-override logic,
  unchanged, just now with an explicit `!args.is_empty()` check where
  clap's own `required = true` previously stood.

## Tests

Two new integration tests in `tests/tests/ocirun_exec.rs`:
- `exec_process_flag_reads_the_entire_spec_from_a_json_file_ignoring_other_flags`
  — a real `process.json` declaring a `cwd` (`/bin`, deliberately
  different from the bundle's own default `/`, and genuinely present
  in the minimal test rootfs), a custom `env`, and
  `noNewPrivileges: true`; given together with deliberately mismatched
  `--cwd`/`--env` CLI flags, proves the JSON file's own values win
  outright (`pwd`, the custom env var, and a real `/proc/self/status`
  `NoNewPrivs:\t1` read all confirm it), not merged with the CLI
  flags at all.
- `exec_with_neither_process_nor_a_command_is_a_clear_error` —
  confirms the previous "a command is required" behavior still holds
  at runtime now that clap itself no longer enforces it
  unconditionally.

All 17 tests in `tests/tests/ocirun_exec.rs` pass (15 prior + 2 new).

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (122
test-result blocks, 0 failures — no new test file added, so the block
count is unchanged from `0508`; clean on the first attempt with
`RUST_TEST_THREADS=2`), `python3 ci/guards.py` (clean), `cargo deny
check` (clean), `bash ci/native-ci.sh` (clean on the first attempt,
also with `RUST_TEST_THREADS=2`), `bash ci/build-deb.sh` (clean on
the first attempt, real `dpkg -i`/`--version`/`dpkg -r` round trip).
This touches `cmd_exec`, part of `ocirun exec`'s own hot path `ci/
bench.sh` measures directly (the same reasoning `0408` already
established), so it was re-run: `ocirun exec` held at 1.70×/9.07×
faster than `crun`/`runc exec` (matching `0408`'s own previously
recorded 1.70×/9.10×, within the same real, noisy-single-host-
measurement range this project's own benchmark methodology has
always shown run to run) — the `--process` branch only ever runs when
the flag is actually given, adding zero cost to the default path
beyond a single `if let` check.

## Deliberately still out of scope

`--console-socket`/`--tty` (real PTY allocation — an already-
documented, project-wide gap), `--pidfd-socket`/`--cgroup` (niche),
`--process-label`/`--apparmor` (no SELinux/AppArmor support anywhere
in this project), and `--detach` (a materially different wait/
foreground model) remain unimplemented — each a real, separate,
bigger gap than this increment's own narrower scope.
</content>
