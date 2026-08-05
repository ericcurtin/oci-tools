# Design note 0485: `ociman system prune`/`system info` aliases

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_prune.rs`,
`tests/tests/ociman_info.rs`.

## What this closes

`SystemCommand`'s own doc comment had already reasoned through why
`prune`/`info` were absent from the `system` family, but that
reasoning stopped short of actually closing the resulting real gap
the way `0430`-`0482` closed the analogous one for `ociman image`/
`container`: `ociman system prune`/`ociman system info` both failed
with clap's plain "unrecognized subcommand" error.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/system/prune.go:22-53`: `pruneCommand` is
  registered with `Parent: systemCmd` **and nowhere else at all** —
  real podman has *no* bare top-level `podman prune` anywhere.
  Confirmed directly: grepped every `prune`-named command file in the
  whole `cmd/podman` tree (`containers/prune.go`, `images/prune.go`,
  `networks/prune.go`, `pods/prune.go`, `system/prune.go`,
  `volumes/prune.go`) — each is nested under its own real family,
  none top-level. This means `ociman system prune` isn't a *second*
  registration of an already-real top-level command the way `image
  tag`/`history`/etc. were (`0478`-`0482`) — it's this real command's
  **only** real home; this project's own flat `ociman prune`
  (`0111`/`0117`, predating the whole `system` family) is the
  deliberate divergence, already correctly explained by the pre-
  existing doc comment, just never actually given its own real nested
  home.
- `~/git/podman/cmd/podman/system/info.go:22-40,53-64`:
  `systemInfoCommand` (`Parent: systemCmd`) and `infoCommand`
  (top-level) share `Args`/`Use`/`Short`/`Long`/`RunE`/
  `ValidArgsFunction` verbatim — the identical pure-alias shape the
  `image`/`container` families already established repeatedly.

## Implementation

Pure dispatch-reuse, matching the `0478`-`0482` pattern exactly:

- `SystemCommand` gains `Prune { all: bool, filter: Vec<String> }`
  (field-for-field identical to the already-existing `Command::
  Prune`) and `Info { format: Option<String> }` (identical to
  `Command::Info`).
- Two new dispatch arms: `SystemCommand::Prune { all, filter } =>
  cmd_prune(cli.global.json, all, &filter)` and `SystemCommand::Info
  { format } => cmd_info(cli.global.json, format.as_deref())` —
  the exact same free functions the top-level commands already call.
- `SystemCommand`'s own top-level doc comment updated to note both
  are now covered and to make the real `prune`/`info` distinction
  explicit (one is the real command's only home; the other is a
  genuine second registration of an already-real top-level twin).

## Tests

Two new integration tests: `system_prune_is_a_byte_identical_alias_
for_prune` (`tests/tests/ociman_prune.rs` — since `prune` mutates the
store, the identical seed-then-untag-then-prune scenario is replayed
in two separately-seeded stores and their `--json` outputs compared,
rather than running both against the same store, which would make
the second call trivially report "nothing left to reclaim"),
`system_info_is_a_byte_identical_alias_for_info` (`tests/tests/
ociman_info.rs` — a real, previously-hit mistake caught and fixed
before landing: `ociman info`'s own report includes the real, live
`host.mem_free` reading, which genuinely differs by a few kilobytes
between two separate process invocations a few milliseconds apart, so
a naive byte-for-byte `stdout` comparison flakes; fixed by comparing
`--json` output with that one specific field nulled out first,
matching `info_json_reports_real_sane_host_values`'s own established
"present and sane, never an exact value" treatment for the identical
field). All 27 tests in `ociman_prune.rs` pass (26 prior + 1 new);
all 7 in `ociman_info.rs` pass (6 prior + 1 new).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (122 test-result
blocks, 0 failures on the third attempt — this dev host was again
under unusually heavy concurrent load this session; both earlier
flaky failures (`ociman_build.rs`'s own cgroup test,
`ocicri_container.rs`'s own capabilities test) were independently
confirmed passing instantly in isolation before retrying), `python3
ci/guards.py` (clean), `cargo deny check` (clean), `bash
ci/native-ci.sh` (clean, 122/122 on the first attempt), `bash
ci/build-deb.sh` (clean, real `dpkg -i`/`--version`/`dpkg -r` round
trip on the first attempt). No benchmark re-run needed: neither
`ociman system prune` nor `system info` is exercised by `ci/
bench.sh`, and this is a pure dispatch-reuse addition touching no
existing function's body at all.
