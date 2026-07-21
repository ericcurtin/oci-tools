# Design note 0116: `ociman build` supports `HEALTHCHECK`

Status: implemented (parse + store only — executing a healthcheck
periodically against a live container stays explicitly out of scope)
Scope: `crates/oci-spec-types/src/image.rs` (`HealthcheckConfig` new,
`ContainerConfig::healthcheck` new field); `crates/oci-dockerfile/src/
instruction.rs` (`HealthcheckCommand`, `Instruction::Healthcheck`,
`parse_healthcheck`, `parse_go_duration`/`parse_optional_go_duration`);
`crates/oci-dockerfile/src/expand_stage.rs` (one more "deliberately
untouched" match arm); `bin/ociman/src/build.rs` (`apply_instruction`'s
own new match arm).

## Why this, now

`HEALTHCHECK` was one of the few remaining hard parse-time rejections
in `oci-dockerfile` (alongside `ONBUILD`, heredocs, and several
BuildKit-only flags) — but unlike those, it's a genuinely common
real-world instruction (most production base images from major distros
and language ecosystems ship one), meaning any real Dockerfile using it
currently fails `ociman build` outright: a direct "drop-in replacement"
gap, not a rarely-hit edge case. `ONBUILD`/heredocs/BuildKit-only flags
stay out of scope (lower real-world frequency, no real `podman build`
equivalent for some of them either).

## Grammar, checked directly against real BuildKit source

`~/git/moby/vendor/github.com/moby/buildkit/frontend/dockerfile/
instructions/parse.go`'s own `parseHealthcheck`:

* `HEALTHCHECK NONE` — explicitly disables any healthcheck inherited
  from the base image; no arguments allowed after `NONE`.
* `HEALTHCHECK [--interval=][--timeout=][--start-period=]
  [--start-interval=][--retries=] CMD <command>` — `<command>` is
  parsed exactly like `RUN`/`CMD` (JSON-array exec form or a plain
  shell-form string), producing `Test = ["CMD", ...args]` for exec form
  or `Test = ["CMD-SHELL", "<command>"]` for shell form — the same
  distinction real Docker's own `dockerspec.HealthcheckConfig.Test`
  makes. Flags are parsed unconditionally (even ahead of `NONE`,
  matching the real parser's own structure) — `--interval=` next to
  `NONE` parses without error, simply unused, exactly like real
  BuildKit.
* Duration flags (`--interval=`/`--timeout=`/`--start-period=`/
  `--start-interval=`) accept a Go-style duration string (`"30s"`,
  `"1h30m"`, ...) — implemented from scratch (`parse_go_duration`, no
  new dependency added), matching real Go's own documented
  `time.ParseDuration` grammar: an optional sign, then one or more
  `<number><unit>` pairs concatenated (`"2h45m"`), units `ns`/`us`
  (`µs`/`μs` too)/`ms`/`s`/`m`/`h`, plus the literal `"0"` special case.
  Verified directly against known Go duration-parsing examples in a
  new unit test (`parse_go_duration_matches_real_go_time_
  parseduration_examples`), not just reasoned about.
* `--retries=` is a plain non-negative integer; a negative value is a
  real, checked-directly error (`"--retries cannot be negative
  (N)"`, real BuildKit's own message verbatim). A duration under 1ms
  (but not exactly `0`, which means "unset") is likewise rejected,
  matching real BuildKit's own floor exactly.

## Storage: exactly matches real Docker's own wire representation

`oci_spec_types::image::HealthcheckConfig` (`Test`/`Interval`/
`Timeout`/`StartPeriod`/`StartInterval`/`Retries`, `#[serde(rename_all
= "PascalCase")]`, durations as raw nanosecond integers) is a
byte-for-byte match of real Docker's own `HealthcheckConfig` struct —
so a real pulled image's own `Healthcheck` object round-trips through
`ociman`'s own `ImageConfig` unchanged, and an image `ociman build`
produces looks identical to what real `docker build`/`podman build`
would have written for the same instruction. `0` means "not set" for
every numeric field, the same convention real Docker uses (there is no
separate "field omitted" vs. "field is zero" distinction in the wire
format either).

## Deliberately out of scope

Actually *running* a healthcheck periodically against a live container
(the process real `dockerd` and `podman`'s own healthcheck subsystem
run) is real, substantial additional work — a background timer,
process supervision, a health-status state machine, `ociman inspect`
surfacing it — orthogonal to just unblocking real Dockerfiles from
failing to *build*. Matches this project's own already-established
"narrow first increment" pattern (e.g. `ociman top`'s own deliberately
narrower support surface than real `podman top`): this increment only
ever parses `HEALTHCHECK`, stores it as inert config metadata, and lets
it round-trip through later stages/`ociman inspect`/`ociman history` —
never executes it.

## Real, automated tests

`oci-dockerfile`'s own unit tests (12 new, in `instruction.rs`):
`NONE` (with and without trailing arguments, the latter a real error);
`CMD` in both shell and exec form producing the right `Test` shape;
missing-command and unknown-type errors (matching real BuildKit's own
message wording); every flag parsed into the right nanosecond count,
including a compound duration (`"1h30m"`); negative `--retries`,
sub-millisecond duration, and an unsupported flag all rejected with
clear, checked-directly error messages; and `parse_go_duration` itself
verified against known real Go duration-parsing examples directly.

`tests/tests/ociman_build.rs` (3 new integration tests): a `HEALTHCHECK
CMD` with every flag set, verifying the built image's own stored
`ContainerConfig.healthcheck` end to end through the real CLI and real
on-disk store; `HEALTHCHECK NONE` correctly overriding (not just
dropping) a base image's own already-set healthcheck; and an invalid
`--retries=-1` surfacing as a real build failure with a clear message,
not silently swallowed. All 45 pre-existing `ociman build` tests still
pass unmodified. Full workspace `cargo build --workspace --locked`,
`cargo test --workspace --locked` (2 clean runs), `cargo fmt --all
--check`, and `cargo clippy --workspace --all-targets --locked -- -D
warnings` all clean.
