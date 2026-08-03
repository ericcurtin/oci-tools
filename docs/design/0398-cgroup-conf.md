# Design note 0398: `linux.resources.unified` + `ociman run/create --cgroup-conf`

Status: implemented
Scope: `crates/oci-spec-types/src/runtime.rs`, `crates/oci-runtime-core/src/cgroups.rs`,
`crates/oci-runtime-core/src/launch.rs`, `crates/oci-runtime-core/src/systemd_cgroup.rs`,
`bin/ociman/src/main.rs`, `bin/ocirun/src/main.rs`, `bin/ocicri/src/runtime_service.rs`,
`tests/tests/ociman_run.rs`, `README.md`.

## What this closes

A real, previously-silent gap of the same shape `0390`-`0397` already
closed: `LinuxResources.unified` — the real runtime-spec's own
`map[string]string` escape hatch for writing an arbitrary cgroup v2
interface file verbatim — had no representation anywhere in this
project's own spec types, and no code anywhere ever applied one. Real
`podman run --cgroup-conf KEY=VALUE` is a genuine, documented flag,
completely absent from `ociman`.

## Real, checked-directly confirmation

- `~/git/container-libs/vendor/github.com/opencontainers/runtime-spec/
  specs-go/config.go`: `LinuxResources.Unified map[string]string`.
- `~/git/podman/cmd/podman/common/create.go`'s own `cgroupConfFlagName`
  (`--cgroup-conf`, a repeatable `KEY=VALUE` string slice feeding
  `ResourceLimits.Unified` directly, no CLI-level validation of its
  own beyond `KEY=VALUE` syntax).
- `~/git/crun/src/libcrun/cgroup-resources.c`'s own
  `write_unified_resources`: applied strictly *after* every other
  structured resource field ("They have higher precedence and
  override any previous setting"), and rejects a key containing a `/`
  outright (escaping the intended cgroup directory).
- `~/git/crun/src/libcrun/cgroup.c`'s own
  `libcrun_update_cgroup_resources`: for the systemd cgroup manager,
  the manager-specific step (D-Bus properties) runs first, then the
  same raw `unified` cgroupfs writes always additionally happen too,
  unconditionally, regardless of which manager actually created the
  cgroup — there is no systemd D-Bus property equivalent for an
  arbitrary raw file.

## Implementation

- `oci_spec_types::runtime::LinuxResources` gains `pub unified:
  BTreeMap<String, String>` (empty by default, omitted when empty).
- `oci_runtime_core::cgroups::apply_unified(cgroup_dir, unified)`
  writes each entry verbatim, rejecting a key containing `/` with a
  real, immediate `io::ErrorKind::InvalidInput` error — matching real
  crun's own identical safety check.
- Called strictly *after* the structured `plan_resources`/`apply` pair
  everywhere resources are written: `launch.rs`'s create-time
  `ChildSetup::run` (raw cgroupfs driver) and `run_reporting_pid`'s
  systemd-scope path (after `create_scope` returns, since `Delegate`
  and the resource *properties* cover everything else but not an
  arbitrary raw file), `ocirun update`, `ociman update` (unchanged,
  reuses the same shared function), and `ocicri
  UpdateContainerResources`.
- `ociman run`/`create` gains `--cgroup-conf KEY=VALUE` (repeatable),
  parsed with a new, shared `parse_key_value_entries` helper
  (`parse_sysctls`/`parse_cgroup_confs` are now both thin wrappers
  around it — only syntax, `KEY=VALUE`, validated at parse time,
  matching real podman's own division of labor; the `/`-rejection
  safety check lives at the runtime layer, `apply_unified`, the same
  split `0395`'s own `sysctl` module already established for a
  different reason).
- `synthesize_spec` threads `cgroup_conf` through even when no other
  resource flag was given at all (the common case for every other
  resource flag `resources_from_cli` already covers), since an empty
  `cgroup_conf` map is itself indistinguishable from "not given" and
  changes nothing for a caller not using this feature.

## A real bug found and fixed while verifying this end to end

`systemd_cgroup::create_scope`'s own long-standing doc comment claimed
it returns "the real cgroup path `pid` ended up in", but the actual
returned `PathBuf` (from `parse_own_cgroup_path`, parsing `/proc/<pid>/
cgroup`'s own `"0::"` line) is relative to wherever cgroup v2 happens
to be mounted, not an absolute filesystem path — despite starting with
a leading `/` that makes it *look* like one. This was entirely latent
before this change: every existing caller of `create_scope` discarded
its `Ok(PathBuf)` value outright (only the `Err` case was ever
inspected), so nothing had ever actually dereferenced the path as a
real filesystem location. Wiring `apply_unified` onto it directly was
what surfaced it — a real `std::fs::write` against e.g.
`user.slice/user-1000.slice/.../ociman-<id>.scope/memory.max` (no
leading `/sys/fs/cgroup`) failed with a plain, otherwise-inexplicable
`ENOENT`, caught immediately by this change's own new integration
tests rather than shipped silently. Fixed at the source: `create_scope`
now joins the parsed, mount-relative path onto a real `CGROUP_ROOT`
(`/sys/fs/cgroup`, the same literal every other real cgroup-path
consumer in this crate already hardcodes) before ever returning it, so
every future caller gets a genuinely usable absolute path, not just
this one.

## Tests

Four new unit tests for `apply_unified` (writes every entry verbatim,
a real no-op on an empty map, rejects a key containing `/`, and a real
precedence proof that a `unified` write after `apply`'s own structured
write genuinely overrides it). Four new unit tests for
`parse_cgroup_confs` (mirroring `parse_sysctls`'s own existing
coverage, now both routed through the shared helper). Two new real,
end-to-end integration tests in `tests/tests/ociman_run.rs`:
`run_cgroup_conf_flag_writes_a_real_cgroup_v2_file` (a real started
container's own live `memory.max` cgroup file, read back via the real
resolved cgroup directory, not just the generated spec) and
`run_cgroup_conf_overrides_an_overlapping_memory_flag` (the real
precedence property above, proven against a live cgroup with both
`--memory` and an overlapping `--cgroup-conf memory.max=` given
together). Both new tests initially failed against the bug described
above; fixing `create_scope` made them pass without any change to the
tests themselves. All existing tests continue to pass unmodified.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures), `python3
ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh`, `bash
ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg -r` round trip).
This change touches `launch.rs`'s `ChildSetup::run`/`run_reporting_pid`
again (the same shared hot-path primitive several recent notes already
re-verified with a full `ci/bench.sh` run) — re-run since this is
genuinely hot-startup-path-adjacent code: every figure held at or
improved on its own recorded baseline, and the added `apply_unified`
call is a single, empty-map-short-circuited no-op on every measured
path that doesn't use this new, opt-in feature.

## Deliberately still out of scope

Real `podman run --cgroup-conf`'s own additional validation (if any
exists beyond `KEY=VALUE` syntax) is not separately ported — this
project's own runtime-level `apply_unified` already provides the one
safety property (`/`-containing keys rejected) that actually matters,
matching real crun's own division of labor exactly. Per-device
block-IO, huge-page, network, and RDMA `unified` keys work exactly
like any other (a raw file write), but nothing in this project
constructs or validates their contents beyond the generic safety
check, matching every other unmodeled `LinuxResources` field's own
documented scope limit (`oci-spec-types/src/runtime.rs`'s own doc
comment).
