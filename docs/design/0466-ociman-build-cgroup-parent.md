# Design note 0466: `ociman build --cgroup-parent`

Status: implemented
Scope: `bin/ociman/src/build.rs`, `bin/ociman/src/main.rs`, `tests/
tests/ociman_build.rs`.

## What this closes

`ociman build` had no `--cgroup-parent` flag at all — real `podman
build --cgroup-parent`'s way of placing every `RUN` step's own
transient cgroup under a specific parent, closing the one flag `0465`
found genuinely still absent from `ociman build` (unlike every other
resource flag in the `0453`-`0464` series, `--cgroup-parent` really
does exist on real `podman build` too, confirmed directly at the
time, deliberately deferred out of that increment to keep its own
diff focused).

## Real, checked-directly confirmation

`~/git/podman/vendor/go.podman.io/buildah/pkg/cli/common.go:152,431`:
`CgroupParent string`/`fs.StringVar(&flags.CgroupParent, "cgroup-
parent", "", "optional parent cgroup for the container")` — a real,
documented `CommonBuildOptions` field. `~/git/podman/vendor/
go.podman.io/buildah/run_linux.go:673-674`: `if commonOpts.
CgroupParent != "" { g.SetLinuxCgroupsPath(commonOpts.CgroupParent) }`
— real buildah writes this straight onto the OCI spec's own
`cgroupsPath` field (the raw-cgroupfs-string convention, since
buildah's own `RUN` steps go through a real `crun`/`runc` subprocess,
not an in-process systemd D-Bus call). This project's own `ociman
build`'s `RUN` steps instead reuse the exact same in-process systemd-
scope mechanism `ociman run/create --cgroup-parent` (`0465`) already
built — the same architectural divergence `0465` itself already
established and cited (real crun's own `Slice=` D-Bus property is the
correct thing to check directly here, not buildah's own `cgroupsPath`
string, which this project's build RUN steps never interpret at all).

## A real gap found while wiring this up (not just plumbing)

The existing `cgroup_setup` construction in `run_instruction` (built
by `0455` for `--memory`) only ever chose the systemd-scope path when
`resources.is_some()` — i.e., some *other* resource flag (`--memory`/
`--cpu-shares`/etc.) was also given. A `RUN` step given **only**
`--cgroup-parent`, with no other resource flag at all, would have
fallen through to the lighter `CgroupSetup::FromSpec` path (no real
cgroup created at all), silently dropping the `Slice=` property
entirely — caught directly by this increment's own first test
attempt (which deliberately gives `--cgroup-parent` alone, matching
`ociman run --cgroup-parent`'s own identical test shape, to prove the
flag alone is sufficient to create a real scope). Fixed by widening
the condition to `resources.is_some() || cgroup_parent.is_some()`.

## Implementation

- `cmd_build` gains a new `cgroup_parent: Option<&str>` parameter.
- `StageContext<'a>` gains a new `cgroup_parent: Option<&'a str>`
  field, carried the same way `resources`/`http_proxy`/`omit_history`
  already are, threaded through `run_instruction`'s own already-
  existing `cgroup_setup`-selecting logic.
- The `cgroup_setup` construction itself: `resources: resources.
  clone().map(Box::new)` (previously only reachable inside the
  `Some(resources) =>` arm; now correctly produces `None` too, for
  the "only `--cgroup-parent` given" case) and a new `parent_slice:
  cgroup_parent.map(str::to_string)`.
- `Command::Build` gains `cgroup_parent: Option<String>`
  (`--cgroup-parent`), inserted after `omit_history`, before `quiet`.

## Tests

One new integration test in `tests/tests/ociman_build.rs`
(`build_cgroup_parent_sets_the_real_systemd_scopes_own_slice_
property` — the same real, live `systemctl --user show <scope> -p
Slice` verification `ociman run --cgroup-parent`'s own test already
established, against the same always-present `app.slice` default,
given **alone** with no other resource flag, directly proving the
real gap found above is actually fixed). All 139 prior tests in the
file pass unmodified (140/140 total) — run three consecutive times to
confirm the shared `lock_build_scope_tests` serialization (`0458`)
correctly prevents any race with the other two build-scope tests now
sharing the same `ociman-build-*.scope` discovery pattern.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (120 test-result
blocks, 0 failures, clean on the first full run), `python3 ci/
guards.py`, `cargo deny check`, `bash ci/native-ci.sh` (clean, 120/120,
clean on the first run too), `bash ci/build-deb.sh` (real `dpkg -i`/
`--version`/`dpkg -r` round trip). No benchmark re-run needed: the
widened `resources.is_some() || cgroup_parent.is_some()` condition
evaluates identically to the old `resources.is_some()` check whenever
`cgroup_parent` is `None` (the default, overwhelmingly common case
`ci/bench.sh` exercises) — behavior and cost for every build not
using this new flag are provably unchanged.

## Deliberately still out of scope

`ocicri`'s own `LinuxPodSandboxConfig.cgroup_parent` (real CRI field,
currently never read at all — `0465`'s own other still-open note)
remains the one other real, reachable follow-up reusing this same
`parent_slice` primitive.
