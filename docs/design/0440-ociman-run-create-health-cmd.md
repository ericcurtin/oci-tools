# Design note 0440: `ociman run`/`create --health-cmd`/`--no-healthcheck`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_healthcheck.rs`.

## What this closes

`HealthcheckConfig`'s own doc comment already flagged "executing a
healthcheck periodically is out of scope for this project so far" —
that's still true. But a real, separate, well-defined gap alongside
it: `ociman run`/`create` had no way to *declare or override* a
container's own healthcheck at all — every healthcheck this project
could ever run came from the resolved image's own `HEALTHCHECK`,
frozen at pull/build time. Real `podman run --health-cmd`/`--no-
healthcheck` (and `docker run`'s identical pair) let a container
override or disable that entirely at creation time, independent of
whatever the image itself declared.

## Real, checked-directly confirmation

`~/git/podman/pkg/specgenutil/specgen.go` (`ToSpecGen`, lines
~352-363):

```go
if len(c.HealthCmd) > 0 {
    if c.NoHealthCheck {
        return errors.New("cannot specify both --no-healthcheck and --health-cmd")
    }
    s.HealthConfig, err = MakeHealthCheckFromCli(c.HealthCmd, c.HealthInterval, c.HealthRetries, c.HealthTimeout, c.HealthStartPeriod, false)
} else if c.NoHealthCheck {
    s.HealthConfig = &manifest.Schema2HealthConfig{Test: []string{"NONE"}}
}
```

A real, checked-directly, easy-to-miss consequence found while reading
this: `~/git/podman/pkg/specgen/generate/container.go`'s own
`applyHealthCheckOverrides` — the function that would otherwise merge
`--health-interval`/`--health-retries`/`--health-timeout`/`--health-
start-period` onto the *image's* own inherited healthcheck — is only
ever called when `CompleteSpec` sees `s.HealthConfig == nil || len(s.
HealthConfig.Test) == 0`. Since `--health-cmd` (when given) sets
`s.HealthConfig.Test` to something non-empty right there in
`ToSpecGen`, that later merge call is skipped entirely, and the four
numeric/duration flags are consumed exclusively inside `MakeHealthCheck
FromCli`, itself only ever called from that same `--health-cmd`
branch. **There is no code path in real podman at all where any of
those four flags has any effect without `--health-cmd` also being
given** — confirmed by grepping every other reference to `c.
HealthInterval`/`c.HealthRetries`/`c.HealthTimeout`/`c.HealthStartPeriod`
in the whole `pkg/specgenutil`/`pkg/specgen` tree: none exist.

`MakeHealthCheckFromCli` (same file, lines ~977-1049) is the exact
grammar `parse_health_cmd_test`/`make_healthcheck_from_cli` port here:
a JSON array is tried first; failing that, the string is split on its
first space purely to sniff a leading `CMD`/`CMD-SHELL`/`NONE`
keyword; `NONE` (case-insensitive, discarding anything after it, even
from a JSON array) always collapses to plain `["NONE"]`; a leading
`CMD-SHELL` is left untouched; anything else becomes `["CMD-SHELL",
<command>]` (the *unwrapped* single element for a one-element JSON
array, the original string verbatim otherwise); a JSON array of two
or more elements with no recognized leading keyword gets a bare `CMD`
prepended. `retries` must be `>= 1`, `timeout` must parse to at least
one second, `start_period` must be non-negative, `interval ==
"disable"` maps to a real `0` (no automatic timer — irrelevant to
this project's own on-demand-only `ociman healthcheck run`, but
stored faithfully regardless).

## Implementation

- `RunArgs` (shared by `Command::Run`/`Command::Create`) gains
  `health_cmd: Option<String>`, `health_interval: String` (`--health-
  interval`, default `"30s"`), `health_retries: u32` (`--health-
  retries`, default `3`), `health_timeout: String` (`--health-
  timeout`, default `"30s"`), `health_start_period: String`
  (`--health-start-period`, default `"0s"`), and `no_healthcheck:
  bool` (`--no-healthcheck`).
- New `parse_health_cmd_test`/`make_healthcheck_from_cli` functions
  port `MakeHealthCheckFromCli` exactly (11 new unit tests), reusing
  this project's own pre-existing `parse_simple_duration` (`h`/`m`/`s`
  combined units, already sufficient for every real healthcheck
  duration example either tool documents) rather than adding a second
  duration parser.
- `prepare_container` validates the `--no-healthcheck`/`--health-cmd`
  mutual exclusivity eagerly (matching real podman's own exact error
  wording) and builds an `Option<HealthcheckConfig>` override: `Some`
  from `make_healthcheck_from_cli` when `--health-cmd` was given,
  `Some(HealthcheckConfig { test: ["NONE"], .. })` when only
  `--no-healthcheck` was, `None` (no persisted override at all, the
  exact pre-existing behavior) otherwise.
- New `ANNOTATION_HEALTHCHECK`: a single JSON-encoded
  `HealthcheckConfig`, persisted only when that override is `Some` —
  the same "one annotation, not one per field" convention
  `ANNOTATION_LABELS` already established.
- `cmd_healthcheck_run` now checks `ANNOTATION_HEALTHCHECK` *first*:
  when present, it's used exclusively (the base-image lookup is
  skipped entirely — this container's own healthcheck is then fully
  self-contained, matching real podman's own already-fully-resolved-
  at-create-time persisted `HealthConfig`); absent, falls back to the
  resolved image's own declared `HEALTHCHECK`, completely unchanged
  from before this existed.

## Tests

Four new tests in `tests/tests/ociman_healthcheck.rs`: `run_health_
cmd_overrides_the_images_own_declared_healthcheck` (the image
declares a healthcheck checking for `/image-healthy`, `--health-cmd`
at `run` time declares a genuinely different one checking for
`/cli-healthy`; creating only the image's own expected file still
reports `unhealthy`, proving the image's own check genuinely never
runs at all, not merely also satisfied — creating the CLI-declared
file instead reports `healthy`), `run_no_healthcheck_disables_even_
an_image_declared_one`, `run_health_cmd_and_no_healthcheck_together_
is_a_clear_error`, and `run_health_cmd_none_disables_the_healthcheck_
too`. All 5 prior tests in the file pass unmodified (9/9 total). Plus
11 new unit tests for `parse_health_cmd_test`/`make_healthcheck_from_
cli` covering every branch of the real grammar above (bare string,
single word, `NONE`/`none` with and without trailing text, leading
`CMD`/`CMD-SHELL` keywords, a multi-element JSON array with and
without a leading `CMD`, a single-element JSON array, an empty JSON
array error, every numeric field round-tripping, `interval ==
"disable"`, and both validation rejections).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (120 test-result
blocks, 0 failures, clean on the first full run), `python3 ci/
guards.py`, `cargo deny check`, `bash ci/native-ci.sh` (clean,
120/120), `bash ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg -r`
round trip). Pure metadata plumbing — parsing/validation happens once
per `run`/`create` invocation, entirely off the container-launch hot
path itself (no new syscall, no change at all when neither flag is
given) — no benchmark re-run needed.

## Deliberately still out of scope

Real podman's own `--health-on-failure` (needs a real on-failure
*action* — restart/stop/kill the container — this project has no
periodic-execution loop to ever trigger from at all), `--health-log-
destination`/`--health-max-log-count`/`--health-max-log-size` (needs
a real per-container health-check log file this project doesn't have
either), and the entire separate `--health-startup-*` flag family
(this project's own `HealthcheckConfig` has no startup-healthcheck
variant concept at all yet) — all three already flagged as future
work by `HealthcheckConfig`'s own pre-existing doc comment, unchanged
by this increment.
