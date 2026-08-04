# Design note 0456: `ociman build --http-proxy`

Status: implemented
Scope: `bin/ociman/src/build.rs`, `bin/ociman/src/main.rs`,
`tests/tests/ociman_build.rs`.

## What this closes

`ociman build` had no `--http-proxy` flag at all — real `podman
build --http-proxy` (default `true`) passes the build host's own
proxy environment variables through into every `RUN` step, so a
Containerfile built behind a corporate/CI proxy doesn't need every
single `RUN` step to redeclare `http_proxy`/`https_proxy`/etc by hand.
Continues the same reuse-the-existing-`StageContext`-field shape
`0453`-`0455` established, though (unlike those three) this one needs
no existing `run`/`create` primitive to reuse at all — real `docker
run`/`podman run` have no equivalent flag of their own (proxy
passthrough is a `build`-only concept, since only a `RUN` step's
process is ever genuinely "offline until proven otherwise" the way a
build environment commonly is).

## Real, checked-directly confirmation

`~/git/podman/vendor/go.podman.io/buildah/pkg/cli/common.go:163,442`:
`HTTPProxy bool`/`fs.BoolVar(&flags.HTTPProxy, "http-proxy", true,
"pass through HTTP Proxy environment variables")` — one build-wide
value, default `true`, no per-stage or per-instruction variant (same
shape as `Ulimit`/`ShmSize`/`Memory`). `~/git/podman/vendor/
go.podman.io/buildah/run_common.go`'s own `configureEnvironment`:
when `b.CommonBuildOpts.HTTPProxy` is true, walks `config.ProxyEnv`
(`~/git/podman/vendor/go.podman.io/common/pkg/config/config.go:31-40`:
`http_proxy`/`https_proxy`/`ftp_proxy`/`no_proxy`, both lower- and
upper-case spellings) and copies each one in from the *build host's*
own process environment (`os.LookupEnv`) if set — called from the
exact same `Builder.Run` setup path (right before the
default+persisted+`options.Env` merge) `0453`-`0455` already found
wiring `addRlimits`/`setupSpecialMountSpecChanges`/`SetLinuxResources
Memory*` into.

## Implementation

- New `PROXY_ENV_NAMES` constant in `build.rs` (`build.rs`'s own copy
  of real buildah's `config.ProxyEnv`, ported directly — no existing
  primitive to reuse this time, since neither `ociman run`/`create`
  nor any other command already has this list anywhere).
- `StageContext<'a>` gains a plain `http_proxy: bool` field (no
  parsing/validation needed at all, unlike every earlier field in
  this series — the CLI value is used completely as-is), carried the
  same way `rlimits`/`shm_size_bytes`/`resources` already are.
- `apply_instruction`'s `Instruction::Run` arm passes `stage_ctx.
  http_proxy` through to `run_instruction`, which threads it to `run_
  step_spec`'s new trailing `http_proxy: bool` parameter; its body,
  right after the existing `ARG`-overlay loop finishes building
  `process.env`, walks `PROXY_ENV_NAMES` and pushes `NAME=value` for
  any name **not already present** in `process.env` (an explicit
  `ENV`/active `ARG` in the Containerfile always wins over the host's
  own ambient value — matching real buildah's own ordering, verified
  directly by this increment's own override test) whose value is
  actually set in `ociman build`'s own process environment
  (`std::env::var`).
- `Command::Build` gains `http_proxy: bool` (`--http-proxy`, the same
  `default_value_t = true, num_args = 0..=1, default_missing_value =
  "true", action = clap::ArgAction::Set` shape this project's own
  `--tls-verify` already established, so `--http-proxy=false` and a
  bare `--http-proxy` both parse correctly), inserted after
  `memory_swap`, before `quiet`.
- A small, related doc-comment correction to `0455`'s own `--memory`
  flag: its "no `--memory-reservation`/`--cpus`/`--cpuset-cpus`/
  `--cpuset-mems` counterpart yet" note was imprecise — checked
  directly this time, real `podman build` has **no**
  `--memory-reservation`/`--cpus` of its own at all (both are
  `run`/`create`/`update`-only convenience flags, confirmed absent
  from buildah's own `CommonBuildOptions`/flag registration); only
  `--cpu-period`/`--cpu-quota`/`--cpu-shares`/`--cpuset-cpus`/
  `--cpuset-mems` are real, still-missing `build` flags.

## Tests

Three new tests in `tests/tests/ociman_build.rs`:
`build_http_proxy_default_passes_the_hosts_proxy_env_into_run_steps`
(a real proxy value set in the *test process's* own environment,
captured from inside a `RUN` step to a file, read back via a
follow-up `run` — the same pattern already established by `build_
dns_flags_synthesize_a_real_resolv_conf_for_run_steps`/`build_ulimit_
sets_a_real_kernel_enforced_rlimit_for_run_steps`),
`build_http_proxy_false_keeps_the_hosts_proxy_env_out_of_run_steps`
(the same setup, `--http-proxy=false`, the captured value is empty),
and `build_explicit_env_overrides_the_hosts_own_http_proxy_value` (an
explicit `ENV http_proxy=...` in the Containerfile wins over the
host's own ambient value, proving the override ordering directly
rather than assuming it). All 130 prior tests in the file pass
unmodified (133/133 total).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean, needed a new `#[allow(clippy::too_many_arguments)]`
on `run_step_spec` now that it has crossed clippy's default
7-argument threshold), `cargo test --workspace --locked` (120
test-result blocks, 0 failures, clean on the first full run), `python3
ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh` (clean,
120/120, clean on the first run too), `bash ci/build-deb.sh` (real
`dpkg -i`/`--version`/`dpkg -r` round trip). Unlike `0455`, this one
touches neither the `RUN`-step launch mechanism nor any cgroup
concept at all — just a handful of `std::env::var` lookups against a
small, fixed name list — so no benchmark re-run was needed.

## Deliberately still out of scope

`--cpu-period`/`--cpu-quota`/`--cpu-shares`/`--cpuset-cpus`/
`--cpuset-mems` remain the real, still-missing tail of buildah's own
`CommonBuildOptions` resource-limit cluster (see `0455`'s own note,
corrected above) — `--cpuset-cpus`/`--cpuset-mems` are a
straightforward follow-up (plain string pass-through, no numeric
grain mismatch, `resources_from_cli` already accepts both), while
`--cpu-period`/`--cpu-quota`/`--cpu-shares` would need a small
`resources_from_cli` signature extension first (real `build` exposes
raw period/quota/shares directly, unlike `run`/`create`'s own
`--cpus`-float-to-quota-conversion, which `resources_from_cli`
currently bakes in as its only path to `LinuxCpu`). `NoHosts`/
`NoHostname`/`OmitHistory` (real buildah's own remaining
`CommonBuildOptions` booleans) are also still out of scope, along
with `ociman build --volume` (BuildKit-/buildah-style `RUN
--mount=type=bind`, a larger, differently-shaped gap).
