# Design note 0443: `ociman exec --latest`/`-l`/`--cidfile`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_exec.rs`.

## What this closes

`ociman exec` had no `--latest`/`-l` flag at all, unlike the whole
`rm`/`stop`/`restart`/`pause`/`unpause` family (`0434`-`0437`) — real
`podman exec --latest`/`--cidfile` exec into the single, most-
recently-created container, or one named by a cidfile, without
needing an explicit `ID`/`--name` at all.

## Real, checked-directly confirmation

Confirmed live against an installed `podman 4.9.3`:

```
$ podman exec 2>&1
Error: exec requires the name or ID of a container or the --latest flag
$ podman exec --latest 2>&1
Error: must provide a non-empty command to start an exec session: invalid argument
```

`--cidfile` doesn't appear in that installed `4.9.3`'s own `--help`
output at all, but is genuinely present in the newer cloned source
tree (`~/git/podman/cmd/podman/containers/exec.go:70-72`) — a real,
newer flag, not a stale citation; implemented here alongside
`--latest` since the two share the identical selection/disambiguation
code path in real podman's own source, `determineTargetCtrAndCmd`
(same file, lines ~223-245):

```go
if len(args) == 0 && !latestSpecified && !execCidFileProvided {
    return "", nil, errors.New("exec requires the name or ID of a container or the --latest or --cidfile flag")
} else if latestSpecified && execCidFileProvided {
    return "", nil, errors.New("--latest and --cidfile can not be used together")
}
command = args
if !latestSpecified {
    if !execCidFileProvided {
        command = args[1:]
        nameOrID = strings.TrimPrefix(args[0], "/")
    } else {
        // read cidfile's first line
    }
}
```

The key real, checked-directly detail: **which positional argument is
the container reference genuinely depends on whether `--latest`/
`--cidfile` was given at all** — with neither, `args[0]` is the
container and `args[1:]` is the command; with either, every element
of `args` is the command instead. This is a real, load-bearing
ambiguity real podman's own Go code sidesteps by manually inspecting
a raw `[]string` rather than any structured flag-parsing positional
binding — exactly the same ambiguity `clap`'s own declarative
positional-argument model can't resolve automatically either (a
separate `id: String` positional field would always eagerly consume
the first token regardless of `--latest`, silently mis-binding a
command's own first word as a container reference the moment
`--latest`/`--cidfile` is used).

## Implementation

- `Command::Exec`'s previous `id: String` + `args: Vec<String>`
  (`required = true`) positional fields are replaced with a single
  `positional: Vec<String>` (`trailing_var_arg = true`), plus new
  `latest: bool` (`#[arg(short = 'l', long)]`) and `cidfile:
  Option<PathBuf>` (`#[arg(long = "cidfile")]`).
- The dispatch arm for `Command::Exec` now performs the exact same
  manual disambiguation real podman's own `determineTargetCtrAndCmd`
  does: `--latest`/`--cidfile` mutual exclusivity checked first (real
  podman's own exact wording); if either is given, the container
  comes from that flag (via the already-shared `resolve_latest_
  container`, or the cidfile's own first line) and every positional
  element is the command; otherwise the first positional element is
  the container reference (with a leading `/` stripped, matching real
  podman's own identical docker-compatibility quirk,
  `strings.TrimPrefix(args[0], "/")`) and the rest is the command. An
  empty resulting command is a real, immediate error either way
  (real podman's own exact wording, confirmed live: `"must provide a
  non-empty command to start an exec session: invalid argument"`).
- `cmd_exec`'s own signature is completely unchanged — it still just
  takes a resolved `id: &str` and `args: &[String]`, exactly as
  before; only the CLI-layer disambiguation above it changed at all.

## Tests

Six new tests in `tests/tests/ociman_exec.rs` (plus a new
`ociman_run_detached_named`/`wait_for_container_status_by_name` pair,
mirroring `ociman_kill.rs`'s/`ociman_pause.rs`'s own identical
existing helpers, needed for the first time in this file):
`exec_latest_execs_into_the_most_recently_created_running_container`
(two running containers with a real creation-time gap; only the
*newer* one's own command ever creates a marker file, and `exec
--latest test -f <marker>` succeeds — a real, convincing proof
`--latest` genuinely targeted it, not merely that some exec
succeeded against something), `exec_cidfile_reads_the_container_id_
from_a_file`, `exec_latest_and_cidfile_together_is_a_clear_error`,
`exec_with_nothing_at_all_is_a_clear_error`, `exec_latest_with_no_
command_is_a_clear_error`, and `exec_strips_a_leading_slash_from_the_
container_reference`. All 12 prior tests in the file pass unmodified
(18/18 total).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (120 test-result
blocks, 0 failures, clean on the second full run — the first hit the
pre-existing, previously-documented `ociman_run.rs` host-contention
flakiness from the long-running runaway CPU-spinning process on this
host, confirmed unrelated and transient by an immediate isolated
rerun), `python3 ci/guards.py`, `cargo deny check`, `bash
ci/native-ci.sh` (clean, 120/120, after one similarly-flaky
`ocicri_container.rs` retry), `bash ci/build-deb.sh` (real `dpkg -i`/
`--version`/`dpkg -r` round trip). Pure CLI-parsing-shape change —
the actual exec syscall path/`resolve_container_id` are completely
untouched, and the ordinary single-explicit-id case does the same
work as before plus one cheap string-prefix check — no benchmark
re-run needed.
