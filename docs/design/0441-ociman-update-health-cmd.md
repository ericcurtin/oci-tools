# Design note 0441: `ociman update --health-cmd`/`--no-healthcheck`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_update.rs`.

## What this closes

`0440` gave `ociman run`/`create` a way to declare/override a
container's own healthcheck at creation time, but there was still no
way to change an *already-existing* container's own healthcheck
afterward — real `podman update --health-cmd`/`--health-interval`/
`--health-retries`/`--health-timeout`/`--health-start-period`/
`--no-healthcheck` do exactly that.

## Real, checked-directly confirmation

`~/git/podman/cmd/podman/containers/update.go`'s own
`GetChangedHealthCheckConfiguration` (lines ~71-121) collects only the
health flags the user actually `cmd.Flags().Changed(...)`, then
`~/git/podman/libpod/healthcheck_config.go`'s own
`GetNewHealthCheckConfig` applies them as a real, **partial** update:

1. Start from real documented defaults (`30s`/`3`/`30s`/`0s`, empty
   command).
2. If the container already has a healthcheck, overwrite that
   baseline with its *current* values — critically, `Cmd` becomes
   `strings.Join(h.Test, " ")`, a plain space-rejoin of the existing
   `Test` array.
3. Apply only the fields actually given on the command line on top of
   that baseline.
4. `--no-healthcheck` together with *any* other health flag is a
   real, immediate error (`"cannot specify both --no-healthcheck and
   other HealthCheck flags"` — a real, checked-directly **broader**
   restriction than `create`'s own, which only conflicts with
   `--health-cmd` specifically).
5. `--no-healthcheck` alone sets `Test: ["NONE"]`.
6. Otherwise, the assembled `Cmd`/interval/retries/timeout/start-period
   go through the identical `MakeHealthCheckFromCli` `0440` already
   ported — **except** `GetNewHealthCheckConfig` always passes
   `isStartup=true` to it, regardless of which of real podman's two
   healthcheck kinds (regular or startup) is actually being updated.
   The only real effect of that flag inside `MakeHealthCheckFromCli`
   is skipping the `retries >= 1` validation — confirmed directly from
   source, not assumed: `ociman update --health-retries 0` succeeds,
   `ociman create --health-cmd ... --health-retries 0` still doesn't.

## Implementation

- `Command::Update` gains the same six flags `Command::Run` has
  (`health_cmd: Option<String>`, `health_interval: Option<String>`,
  `health_retries: Option<u32>`, `health_timeout: Option<String>`,
  `health_start_period: Option<String>`, `no_healthcheck: bool`) —
  all genuinely `Option`-typed here (unlike `RunArgs`'s own always-
  defaulted `String`/`u32` versions), since `update` needs to
  distinguish "not given at all" from "given, even with a real
  default-shaped value" for its own partial-merge semantics.
- New shared `resolve_effective_healthcheck` (factored out of `cmd_
  healthcheck_run`'s own pre-existing inline logic, zero behavior
  change there): container-level `ANNOTATION_HEALTHCHECK` override
  first, else the resolved image's own declared `HEALTHCHECK`, else
  `None` — now reused by both `cmd_healthcheck_run` and the new
  `update_healthcheck`.
- `cmd_update` now accepts both the pre-existing resource flags and
  the six new health flags: a resource-flag change still requires
  the container to actually be running (unchanged); a health-flag
  change requires only that the container *exist* — real podman's
  own identical "persisted config change" scope, verified directly
  with a real `created`-but-never-`started` container. "No resource
  *or* health flags at all" is the new combined error.
- New `update_healthcheck` ports `GetNewHealthCheckConfig`'s exact
  merge: `--no-healthcheck` mutual exclusivity against every other
  health flag (not just `--health-cmd`), a baseline `Cmd` built from
  `existing.test.join(" ")` when `--health-cmd` wasn't given (empty
  string if there's no existing healthcheck at all, matching real
  podman's own identical starting point), and a new
  `make_healthcheck_from_cli_for_update` — the same `make_healthcheck_
  from_cli` `0440` already has, minus the `retries >= 1` check,
  matching the real `isStartup=true` quirk above.
- New `format_duration_seconds(nanos, default)` reconstructs a plain
  `<seconds>s` string `parse_simple_duration` can re-parse from an
  existing healthcheck's own nanosecond field (`0` — "never set" —
  falls back to the given real default instead of a meaningless
  `"0s"` override), the same round-trip-through-a-string-then-reparse
  technique real podman's own `Cmd` reconstruction already uses.

## Tests

Six new tests in `tests/tests/ociman_update.rs`: `update_health_cmd_
on_a_created_but_never_started_container_persists_without_running`
(a real, live proof of the "no running container needed" scope: the
override survives a later `start`), `update_health_interval_alone_
preserves_the_existing_health_cmd` and `update_health_retries_alone_
rebuilds_from_the_images_own_declared_command` (both real, convincing
proofs that only the given field changes — the *original* command,
whether from an earlier `--health-cmd` or straight from the image's
own declaration, is still the one actually exec'd afterward, not
merely "some command still exists"), `update_no_healthcheck_
disables_even_an_image_declared_one`, `update_no_healthcheck_
combined_with_any_other_health_flag_is_a_clear_error`, and
`update_health_retries_zero_succeeds_unlike_create` (a direct,
side-by-side proof of the real upstream quirk: the identical `create`
call still fails, the `update` one doesn't). One pre-existing test's
own error-message assertion updated for the new combined wording
(`update_with_no_resource_flags_at_all_is_a_clear_error` ->
`update_with_no_resource_or_health_flags_at_all_is_a_clear_error`).
All 4 other prior tests pass unmodified (10/10 total). Plus 4 new
unit tests for `format_duration_seconds`/`make_healthcheck_from_cli_
for_update`.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (120 test-result
blocks, 0 failures, clean on the third full run — two earlier runs
each hit one, different instance of the pre-existing, previously-
documented host-contention flakiness from the long-running runaway
CPU-spinning process on this host, `ocicri_container.rs` then
`ociman_logs.rs`, each confirmed unrelated and transient by an
isolated rerun), `python3 ci/guards.py`, `cargo deny check`, `bash
ci/native-ci.sh` (clean, 120/120 — one earlier run hit the identical
class of `ocicri_container.rs` flake, confirmed transient the same
way, then a clean rerun), `bash ci/build-deb.sh` (real `dpkg -i`/
`--version`/`dpkg -r` round trip). Pure metadata plumbing — string/
JSON parsing on one `ociman update` invocation, entirely off the
container-launch hot path — no benchmark re-run needed.

## Deliberately still out of scope

Exactly the same three items `0440`'s own "still out of scope"
section already named (`--health-on-failure`, `--health-log-
destination`/`--health-max-log-count`/`--health-max-log-size`, the
entire `--health-startup-*` family) — none of `update`'s own
equivalent flags for those are implemented here either, for the
identical reasons.
