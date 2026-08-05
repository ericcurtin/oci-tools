# Design note 0468: `ocibox enter --clean-path`/`-c` + real host-`$PATH`-merge default

Status: implemented
Scope: `bin/ocibox/src/main.rs`, `tests/tests/ocibox_enter.rs`.

## What this closes

`ocibox enter` never touched `PATH` at all — the entered process
always inherited whichever bare `PATH` the box's own image declared
(or `DEFAULT_ENV_WHEN_BOX_DECLARES_NONE`'s own hardcoded fallback when
the image declared none). Real `distrobox enter` does something
genuinely different by default: it merges the **host**'s own `$PATH`
into the container's, so tools installed only on the host (and not
inside the box's own image) are still reachable by name once inside.
This is not just a missing flag — it is a missing default *behavior*,
closed here at the same time as the flag (`--clean-path`) that opts
back *out* of it.

## Real, checked-directly confirmation

- `~/git/distrobox/pkg/containermanager/containermanager.go:179-241`:
  `BuildContainerPath(cleanPath bool, hostPath, containerPath string)
  string` and `reorderFHSPath(path string) string` — the exact two
  functions ported verbatim below.
  - `BuildContainerPath`: if `cleanPath` is true, returns the bare six
    standard FHS dirs joined, ignoring both other arguments outright.
    Otherwise, if `hostPath` is empty, returns `containerPath` if
    non-empty, else the same bare standard-dirs join. Otherwise,
    starts from `hostPath` and appends any of the six standard dirs
    not already present as a whole `:`-delimited segment (a real
    segment-membership check, not a substring search — `/opt/usr/bin`
    must not be mistaken for already containing `/usr/bin`), then
    calls `reorderFHSPath` on the result.
  - `reorderFHSPath`: walks the path's own segments; whenever it sees
    `/usr/bin` or `/usr/sbin`, it inserts that segment's own
    `/usr/local/{bin,sbin}` counterpart immediately before it (if not
    already present earlier); any local dir whose own `/usr`
    counterpart never appeared at all gets prepended to the very
    front afterward.
- `~/git/distrobox/pkg/containermanager/providers/podman.go:877` /
  `docker.go:790`: `BuildContainerPath(cleanPath, os.Getenv("PATH"),
  containerConfig.ContainerPath)` — confirms the real host's own
  `$PATH` (not some hardcoded value) is the second argument, and
  confirms the call site is unconditional (always runs, `cleanPath`
  is just the third case inside `BuildContainerPath` itself). Lines
  ~802/685 respectively confirm `containerConfig.ContainerPath` comes
  from inspecting the *container's own* declared `PATH=` env entry.
- `~/git/distrobox/internal/cli/enter.go:36-44`: the real
  `--clean-path`/`-c` flag, default `false`.
- `~/git/distrobox/internal/cli/ephemeral.go`: confirmed no
  `--clean-path` flag exists there at all — `ocibox ephemeral` is
  updated to always behave as if it were `false` (never settable),
  matching that real absence rather than inventing a flag real
  distrobox itself doesn't offer on that subcommand.

## Implementation

- `STANDARD_PATH_DIRS: &[&str]` (new const): the six FHS dirs in
  their real fixed order, `["/usr/local/sbin", "/usr/local/bin",
  "/usr/sbin", "/usr/bin", "/sbin", "/bin"]`.
- `build_container_path(clean_path: bool, host_path: Option<&str>,
  container_path: &str) -> String` and `reorder_fhs_path(path: &str)
  -> String` (new pure functions): direct Rust ports of the two real
  Go functions above, segment-membership checks done via `.split(':')
  .any(|s| s == dir)` rather than `.contains(dir)` to match the real
  non-substring semantics.
- `enter_spec`: right after `process.env` is populated (from either
  `record.env` or the existing `DEFAULT_ENV_WHEN_BOX_DECLARES_NONE`
  fallback), the current `PATH=` entry (if any) is extracted as
  `container_path`, `build_container_path(clean_path,
  std::env::var("PATH").ok().as_deref(), &container_path)` is
  computed, and the `PATH=` entry in `process.env` is replaced in
  place (or pushed new if somehow absent).
- `enter_and_get_exit_code`/`cmd_enter` gain a threaded `clean_path:
  bool` parameter; `Command::Enter` gains `#[arg(long = "clean-path",
  short = 'c')] clean_path: bool`. `cmd_ephemeral`'s own call site
  passes a hardcoded `false` (see above — real `ephemeral` has no such
  flag).

## Tests

Seven new unit tests directly against `build_container_path`/
`reorder_fhs_path` (clean-path always wins; no-host-path falls back to
container-path then to the standard join; a real host path merges only
the genuinely-missing standard dirs and gets FHS-reordered; a
`/opt/usr/bin`-style segment is never mistaken for `/usr/bin` itself;
`reorder_fhs_path` on its own, both the "move local dir before its own
`/usr` counterpart" and "prepend a local dir whose counterpart never
appeared" cases).

Two new integration tests in `tests/tests/ocibox_enter.rs`, both
against the real built `ocibox` binary and a real running container:
`enter_merges_the_real_hosts_own_path_into_the_boxs_own_by_default`
(sets a real, deliberately-`PATH`-poor value as the *test process's
own* `PATH` env var before spawning `ocibox enter -- /bin/sh -c 'echo
$PATH'`, asserts the exact merged-and-reordered string comes back);
`enter_clean_path_resets_to_the_bare_fhs_standard_ignoring_the_real_
host_path` (same setup, `--clean-path` given, asserts the bare six-dir
join comes back instead, proving the flag reaches `enter_spec` and not
just that it parses). Grepped both pre-existing `ocibox_enter.rs` and
`ocibox_create.rs` first to confirm no prior test asserted on `$PATH`
content at all, so there was no silent false-green risk from changing
the default behavior. All 13 tests in `ocibox_enter.rs` pass (11 prior
+ 2 new); all 8 in `ocibox_create.rs` pass unmodified; 18 pre-existing
+ new `ocibox` unit tests pass.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (120 test-result
blocks, 0 failures on the confirming rerun — the first attempt hit one
transient, already-documented flaky failure in `ocicri_container.rs`'s
own `create_container_applies_the_sandboxs_own_sysctls_to_a_real_
running_container`, exit code 126 "process exited before exec",
confirmed unrelated and passing instantly in isolation), `python3
ci/guards.py` (clean), `cargo deny check` (clean), `bash
ci/native-ci.sh` (hit two further transient flakes across its first
three attempts — `ocicri_container.rs`'s own capability test twice,
`ociman_logs.rs`'s own follow test once, all consistent with this dev
host's already-documented long-running CPU-spinning background
process, each confirmed passing instantly in isolation before
retrying the full script; the fourth attempt passed clean, 120/120),
`bash ci/build-deb.sh` (clean, real `dpkg -i`/`--version`/`dpkg -r`
round trip on the first attempt). No benchmark re-run needed: grepped
`ci/bench.sh` directly and confirmed it never references `ocibox` at
all.

## Deliberately still out of scope

Real `distrobox enter`'s own genuine cross-session persistence model
(a long-lived keeper process a box's later `enter` calls attach back
onto, rather than each `enter` being its own independent foreground
container) remains this project's own already-documented, pre-
existing `ocibox` limitation (see `Command::Enter`'s own doc comment)
— entirely unrelated to `PATH` handling, not touched by this
increment.
