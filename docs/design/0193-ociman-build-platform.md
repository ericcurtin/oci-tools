# Design note 0193: `ociman build --platform`, and closing a real
silent `FROM --platform=` gap

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Build`'s new `--platform`
flag); `bin/ociman/src/build.rs` (`parse_platform_spec`, `cmd_build`'s
new platform check); `tests/tests/ociman_build.rs`.

## Refocusing on milestone 4 itself

The last several increments closed small, real gaps in milestone 3
("beyond original scope"). Milestone 4 (`ociman build`) is this
project's own current, explicitly "in progress" milestone per
`README.md` — surveyed its own current source (not stale doc comments)
for genuinely open gaps there specifically, to work on the project's
own stated current priority rather than continuing to drift further
into milestone-3-adjacent polish.

## A real, previously-unnoticed bug found by the survey

`crates/oci-dockerfile/src/instruction.rs` already parses `FROM
--platform=<value>` into `Instruction::From.platform`/`Stage.platform`
(and `$VAR`-expands it, `expand_stage.rs`) — but `bin/ociman/src/
build.rs` never read that field anywhere at all. A Containerfile
requesting a non-host platform (e.g. `FROM --platform=linux/arm64
ubuntu` run on an x86_64 host) silently got the host platform instead,
with no warning or error whatsoever — a real, silent correctness gap
that predates this increment.

## Real precedence, checked directly

Read `~/git/moby/vendor/github.com/moby/buildkit/frontend/dockerfile/
dockerfile2llb/convert.go` directly before designing anything: a
per-stage `FROM --platform=` value, when given, always overrides the
whole build's own `--platform` flag; the build-wide flag only ever
fills in for a stage that doesn't specify its own.

## The fix, scoped honestly

This project has no real cross-architecture emulation of any kind, so
rather than attempt full cross-platform pulling/building (a much
larger feature — see "what this doesn't do yet"), the fix makes the
previously-silent gap a clear, immediate error instead:

* `Command::Build` gains `--platform <os/arch[/variant]>`.
* `parse_platform_spec` parses the `os/arch[/variant]` form into a
  real `Platform`.
* For each stage that resolves an external base image, the effective
  requested platform (`stage.platform` if given, else the CLI flag) is
  checked against `Platform::host()`; a mismatch is a clear, immediate
  error naming both the requested and actual platform. A platform that
  *does* match the host (the common case for a Containerfile that
  pins its own platform explicitly, even when it happens to already
  match) builds completely normally, unchanged.

## A real bug in my own first attempt at this exact check, found while
verifying it end to end

The very first version used `host.matches(&requested_platform)` and
failed even for a platform that *does* match the host: on this
session's own real aarch64 host, `Platform::host()`'s own `variant`
is `Some("v8")` (`host_variant()`'s own doc comment), but a bare
`linux/arm64` request (no variant given at all) has `variant: None`.
`Platform::matches`'s own semantics (`self.variant.is_none() || ...`)
treat "no variant specified" as "no requirement" only when it's the
*left-hand* side (the selection criterion) that omits it — calling it
with `host` on the left made the *host's own* variant the requirement,
which an unqualified request could then never satisfy. Fixed by
swapping the call to `requested_platform.matches(&host)` — the
*request* is the selection criterion, the *host* the candidate being
checked against it, exactly the same direction `Platform::matches` is
already used for picking a real manifest-list entry elsewhere in this
project. Caught by hand, testing the exact positive case first (a
`--platform` matching the real host), before ever considering this
done — the same rigor this project always applies.

## Tests

Three new integration tests in `tests/tests/ociman_build.rs`: a
`--platform` matching the real host succeeds; a `--platform` naming
the *other* architecture is a clear, immediate error; a per-stage
`FROM --platform=` overrides a matching global `--platform` flag
(still surfacing the one clear error). All portable across whichever
real architecture runs them (`host_goarch()`, mirroring `Platform::
host`'s own internal naming). All 92 pre-existing `ociman build` tests
continue to pass unchanged. Full `cargo build --workspace --locked`/
`cargo test --workspace --locked` (2 clean runs, 83/83 result blocks)/
`cargo fmt --all --check`/`cargo clippy --workspace --all-targets
--locked -- -D warnings`/`python3 ci/guards.py`/`cargo deny check`/
`bash ci/native-ci.sh` all clean. No performance regression (`ociman
build --no-cache`, one `RUN` step, ~25.8ms, consistent with this
project's own prior measurements for the same scenario — one string
comparison per stage costs nothing measurable).

## What this doesn't do yet

Real cross-architecture building (actually pulling/selecting a
non-host manifest-list entry and — the much harder part — actually
*running* a foreign-architecture `RUN` step, which would need real
emulation, e.g. `binfmt_misc`/QEMU user-mode, this project has none of
at all) remains unimplemented; the new check makes that gap an honest,
immediate error instead of a silent, wrong substitution, which is as
far as this increment goes. `--platform` combined with multi-platform/
manifest-list *output* (building for several architectures at once,
`docs/design/0192`'s own sibling survey item) is a separate, larger,
still-open gap.
