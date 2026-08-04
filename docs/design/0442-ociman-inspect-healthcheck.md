# Design note 0442: `ociman inspect`'s own `healthcheck` field

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_inspect.rs`.

## What this closes

`0440`/`0441` gave `ociman run`/`create`/`update` a real way to
declare, override, and partially update a container's own
healthcheck, but `ociman inspect` never surfaced any of it at all —
there was no way to see what a container's own currently-effective
healthcheck actually is (image-declared, CLI-overridden, or disabled)
without indirectly inferring it from `ociman healthcheck run`'s own
pass/fail exit code. Real `podman inspect`'s own `Config.Healthcheck`
field is exactly this.

## Implementation

- `ContainerInspectView` gains `healthcheck: Option<HealthcheckConfig>`
  (`#[serde(skip_serializing_if = "Option::is_none")]`, omitted
  entirely rather than `null` for a container with genuinely none at
  all) — deliberately reusing this project's own pre-existing
  `HealthcheckConfig` shape directly (already `Serialize`, already
  the exact same type `ociman history`/image inspect output use)
  rather than a field-for-field port of real podman's own richer,
  differently-named `define.HealthCheckConfig`.
- Populated via the exact same `resolve_effective_healthcheck` (`0441`)
  `ociman healthcheck run`/`ociman update` already share: a `run`/
  `create`/`update --health-cmd`/`--no-healthcheck` override first,
  else the resolved base image's own declared `HEALTHCHECK`, else
  `None`. A resolution failure (e.g. the base image having since been
  removed from local storage) is `None` here too, never a hard
  `inspect` failure — the same "best-effort enrichment, absence over
  a spurious whole-command failure" convention this exact view's own
  `labels`/`mounts`/`display_status` already establish.
- Wired directly into `ContainerInspectView::from_state` itself
  (alongside `extra_mounts`'s own identical best-effort pattern),
  not deferred to `cmd_inspect` the way the opt-in `--size` field is
  — real podman's own `Config.Healthcheck` is always shown
  unconditionally too, and the underlying resolution is cheap (a
  single already-cached annotation read, or at worst one manifest+
  config blob read already paid identically by `healthcheck run`).
- A genuinely live-resolved view, not a value snapshotted once at
  creation time: a *later* `ociman update --health-cmd` change is
  immediately reflected in the very next `ociman inspect`, verified
  directly (see Tests below) — there is no separate, stale copy of
  this anywhere in `ContainerInspectView` for the two to ever drift
  apart from each other.

## Tests

Four new tests in `tests/tests/ociman_inspect.rs`: `inspect_
healthcheck_shows_a_health_cmd_override_taking_precedence_over_the_
image` (the image declares one healthcheck, `create --health-cmd`
declares a different one; `inspect` shows the CLI one), `inspect_
healthcheck_falls_back_to_the_images_own_declared_one` (no CLI
override at all), `inspect_healthcheck_field_is_absent_with_no_
healthcheck_at_all` (omitted, not `null`), and `inspect_healthcheck_
reflects_a_later_update_health_cmd_change` (absent before, present
and correct immediately after an `ociman update --health-cmd` call
against the same still-running container). All 22 prior tests in the
file pass unmodified (26/26 total).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (120 test-result
blocks, 0 failures, clean on the third full run — the first two each
hit one or two different instances of the pre-existing, previously-
documented `ocicri_container.rs` host-contention flakiness from the
long-running runaway CPU-spinning process on this host, each
confirmed unrelated and transient by an immediate isolated rerun),
`python3 ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh`
(clean, 120/120 — one earlier run hit the identical class of flake,
confirmed transient the same way, then a clean rerun), `bash
ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg -r` round trip).
`ociman inspect` is not on any `ci/bench.sh`-measured hot path
(confirmed by grep: `bench.sh` never calls `ociman inspect` at all) —
no benchmark re-run needed.
