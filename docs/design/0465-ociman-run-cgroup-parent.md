# Design note 0465: `ociman run`/`create --cgroup-parent`

Status: implemented
Scope: `crates/oci-runtime-core/src/systemd_cgroup.rs`, `crates/oci-
runtime-core/src/launch.rs`, `bin/ociman/src/main.rs`, `bin/ociman/
src/build.rs`, `bin/ocicri/src/launcher.rs`, `tests/tests/
ociman_run.rs`.

## What this closes

`ociman run`/`create` had no `--cgroup-parent` flag at all — real
`docker run`/`podman run`/`create`'s own optional parent cgroup.
`docs/design/0015` (this project's very first cgroup increment)
already named this exact gap explicitly: *"Real `runc` falls back to
a `--cgroup-parent`-derived name when unset; this project has no
equivalent CLI convention yet."* That note was about `ocirun`'s own
raw-cgroupfs `cgroupsPath` handling specifically — but `ocirun` itself
correctly has **no** `--cgroup-parent` flag at all (checked directly,
`~/git/runc/*.go` has zero occurrences of `cgroup-parent`/
`CgroupParent`, since real `runc` also has none — that's purely a
higher-level docker/podman concept, translated by *them* into the
lower-level `cgroupsPath`/systemd-scope-property the runtime actually
receives). The real, still-open gap this closes is one level up, at
the `ociman` layer.

## Real, checked-directly confirmation

`~/git/podman/cmd/podman/common/create.go:798-800`: `cgroupParent
FlagName := "cgroup-parent"` — real, documented flag on `run`/
`create` (`~/git/podman/docs/source/markdown/podman-run.1.md.in:112`/
`podman-create.1.md.in:93`), absent from `podman-update.1.md.in`
entirely (a container's own parent cgroup is fixed at creation, never
changed later — confirmed directly, not assumed).

Real podman's own **systemd**-driver translation (`~/git/podman/
libpod/container_internal_linux.go:358-365`) is architecturally
different from this project's: podman itself never talks to systemd
directly — it computes `%s:libpod:%s` (slice-basename:prefix:id) and
writes that as the OCI spec's own `cgroupsPath` string, leaving the
*runtime* (`crun`/`runc`, invoked as a real subprocess) to parse that
string and make the actual `StartTransientUnit` D-Bus call itself.
This project's `ociman` never shells out to `ocirun`/`crun`/`runc` at
all — it makes the exact same D-Bus call directly, in-process
(`systemd_cgroup::create_scope`). So the real translation to check
directly was real **crun**'s own systemd cgroup driver instead (the
actual code performing the D-Bus call podman's own string ends up
driving): `~/git/crun/src/libcrun/cgroup-systemd.c`'s own
`get_systemd_scope_and_slice`/`append_io_weight`-adjacent code sets a
plain `"Slice"` D-Bus property (`sd_bus_message_append (m, "(sv)",
"Slice", "s", slice)`) from the caller-supplied slice name **with no
transformation of its own at all** — confirming a direct, no-
conversion pass-through of a caller-supplied, already-`.slice`-
suffixed string is the correct, faithful real semantics to port, not
a shortcut.

## Implementation

- `oci_runtime_core::systemd_cgroup::create_scope`/`create_scope_
  dbus_roundtrip` gain a new `parent_slice: Option<&str>` parameter,
  pushing `("Slice", Value::from(slice))` onto the D-Bus properties
  list when given (after `resources`, though the two never actually
  overlap — `resource_properties` never emits a `"Slice"` entry of
  its own).
- `oci_runtime_core::launch::CgroupSetup::Systemd` gains a new
  `parent_slice: Option<String>` field, threaded straight through at
  its one call site in `launch.rs`.
- `ociman`'s own three `CgroupSetup::Systemd` construction sites all
  updated: `run_and_finalize`'s (`ociman run`'s own direct, immediate
  launch, and `ociman start`'s later resumption) gains a real
  `cgroup_parent: Option<&str>` parameter; `build.rs`'s (`ociman
  build`'s own `RUN` steps) and `ocicri`'s (`launcher.rs`) both pass
  `None` — real, deliberately deferred gaps for each, not fixed in
  this same increment (see below).
- `RunArgs` (flattened into both `Command::Run`/`Command::Create`)
  gains `cgroup_parent: Option<String>` (`--cgroup-parent`), inserted
  after `blkio_weight`.
- **Persistence across a later, separate `start`** (real podman's own
  "set once, persists for the container's whole lifetime"
  architecture, confirmed directly, `~/git/podman/libpod/container_
  internal_linux.go`'s own `c.config.CgroupParent`, read fresh on
  every real (re)start): a new `ANNOTATION_CGROUP_PARENT` constant,
  persisted by both `cmd_run`/`cmd_create` the exact same way
  `ANNOTATION_INTERACTIVE` already is, and read back by `cmd_start`
  (which has no `--cgroup-parent` flag of its own at all, matching
  real `podman start`). `launch_detached_and_confirm` (the shared
  detached-launch helper both `ociman run -d` and `ociman start` use)
  gained an owned `cgroup_parent: Option<String>` parameter, moved
  into its own fork closure the same way `container_id_for_keeper`
  already is.

## Tests

Three new integration tests in `tests/tests/ociman_run.rs`:
`run_cgroup_parent_sets_the_real_systemd_scopes_own_slice_property`
(a real, live `systemctl --user show <scope> -p Slice` check against
the existing, always-present `app.slice` default — safe to target
without first needing to create a brand new custom slice) and
`create_cgroup_parent_persists_for_a_later_separate_start` (`create`
with `--cgroup-parent`, then a genuinely separate `start` invocation,
proving the persisted annotation round-trip actually works end to
end, not just that the flag parses). All 111 prior tests in the file
pass unmodified (113/113 total after both new tests, plus `0464`'s
own already-present `run_cpu_rt_flags_are_accepted_but_set_no_real_
systemd_property_at_all`). Two new unit tests in `systemd_cgroup.rs`'s
own pre-existing call sites updated for the new signature
(mechanical, no logic change).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (120 test-result
blocks, 0 failures — three separate transient, known-flaky failures
across the first two full-suite attempts, all in previously-
catalogued flaky spots (`ocicri_container.rs`'s own exit-126 "process
exited before exec" pattern, twice, and `ociman_logs.rs`'s own
`logs_follow_streams_a_running_containers_output_and_stops_when_it_
exits`, once — all three confirmed unrelated and passing instantly in
isolation, consistent with the already-documented, accepted
environmental flakiness from this dev host's long-running CPU-
spinning background process; a third attempt passed clean 120/120),
`python3 ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh`
(clean, 120/120, clean on the first run), `bash ci/build-deb.sh` (real
`dpkg -i`/`--version`/`dpkg -r` round trip). This touches `run`/
`create`'s own real hot path — ran the full `ci/bench.sh` suite to
confirm no regression: all 9 categories show speedups consistent with
previously-recorded baselines.

## Deliberately still out of scope

`ociman build --cgroup-parent` (real, confirmed directly, `~/git/
podman/vendor/go.podman.io/buildah/pkg/cli/common.go`'s own
`CgroupParent`/`SetLinuxCgroupsPath`, applied to every `RUN` step —
unlike every other flag `0453`-`0464` found genuinely absent from
`build`, this one really does exist there too) and `ocicri`'s own
`LinuxPodSandboxConfig.cgroup_parent` (real CRI field, `crates/
oci-cri-types/proto/api.proto`'s own field 1, currently never read at
all — the CRI spec's own comment, *"the container runtime can convert
it to systemd semantics if needed,"* describes exactly the `Slice=`
translation this increment just built) are both real, genuinely
reachable follow-ups reusing the exact same new `parent_slice`
primitive — deliberately deferred to their own separate increments
rather than folded into this one, to keep this turn's diff focused on
the single clearest, most-explicitly-already-acknowledged gap
(`docs/design/0015`'s own six-increment-old note).
