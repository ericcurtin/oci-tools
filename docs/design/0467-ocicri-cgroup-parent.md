# Design note 0467: `ocicri` `LinuxPodSandboxConfig.cgroup_parent`

Status: implemented
Scope: `bin/ocicri/src/container.rs`, `bin/ocicri/src/runtime_
service.rs`, `bin/ocicri/src/launcher.rs`, `tests/tests/
ocicri_container.rs`.

## What this closes

`ocicri` never read `LinuxPodSandboxConfig.cgroup_parent` at all —
closing the last real gap `0465`/`0466` left documented: the CRI
spec's own comment on this field, *"the container runtime can convert
it to systemd semantics if needed,"* describes exactly the `Slice=`
translation that same series already built for `ociman run`/`create`/
`build`.

## Real, checked-directly confirmation

`crates/oci-cri-types/proto/api.proto:527`: `string cgroup_parent =
1;` inside `LinuxPodSandboxConfig` — a real, always-present (proto3
scalar, never wrapped in its own `Option`) field on every real
`RunPodSandbox`/`CreateContainer` request. Unlike `--cgroup-parent`
on `ociman run`/`create`/`build` (a CLI flag the *caller* gives),
this is a value kubelet (or `crictl`) sets in the request itself —
the mechanism is otherwise identical: a plain string, passed straight
through to the real systemd `Slice=` unit property with no
transformation of its own (see `0465`'s own doc comment for the
exact real crun-based citation this reuses verbatim).

## A real architectural wrinkle found while wiring this up

`CreateContainerRequest.sandbox_config` (`api.proto:1376`'s own
comment: *"passed again here just for easy reference"*) carries the
*full* `PodSandboxConfig` again on every `CreateContainer` call — so
reading `cgroup_parent` at create time needed no new sandbox-record
persistence at all. But the value is only actually *needed* much
later, inside `launcher.rs`'s own re-exec'd process (spawned by a
*separate* `StartContainer` RPC, which the CRI proto never re-sends
`sandbox_config` to at all — `StartContainerRequest` carries only a
bare `container_id`). So `cgroup_parent` had to be captured onto the
container's own **persisted record** at `CreateContainer` time (a new
`ContainerRecord::cgroup_parent` field, the same shape `log_path`
already established for the identical "value needed by a much later,
separate re-exec" problem), then threaded from the record into the
launcher subprocess's own argv.

## Implementation

- `ContainerRecord` (`container.rs`) gains `cgroup_parent:
  Option<String>` (`#[serde(default)]`), populated at `create_
  container` from `sandbox_config.linux.as_ref().map(|l| l.
  cgroup_parent.clone()).filter(|s| !s.is_empty())`.
- `launcher.rs`'s own tiny re-exec protocol (previously `[bundle_dir,
  container_id]` or `[bundle_dir, container_id, log_path]`) is
  reshaped to always carry exactly four positional slots: `[bundle_
  dir, container_id, log_path, cgroup_parent]`, an empty string
  meaning "not given" in either optional slot (this is `ocicri`'s own
  private re-exec protocol between `StartContainer` and this same
  binary — never a public CLI surface, freely reshaped with every
  increment; a real bundle directory/container id/log path/systemd
  slice name is never legitimately empty, so the sentinel is
  unambiguous). `run()`'s own `CgroupSetup::Systemd` construction
  gains `parent_slice: cgroup_parent.map(str::to_string)`.
- `start_container` (`runtime_service.rs`) updated to always pass
  both slots (`record.log_path.as_deref().unwrap_or("")`/`record.
  cgroup_parent.as_deref().unwrap_or("")`) when spawning the launcher
  subprocess.

## Tests

One new integration test in `tests/tests/ocicri_container.rs`
(`create_container_cgroup_parent_sets_the_real_systemd_scopes_own_
slice_property` — the same real, live `systemctl --user show <scope>
-p Slice` verification `ociman run`/`build --cgroup-parent`'s own
tests already established; unlike those, `ocicri`'s own scope name is
fully deterministic (`ocicri-<container_id>.scope`, `launcher.rs`'s
own fixed convention) so this queries it directly rather than
discovering it by pattern). All 37 prior tests in the file pass
unmodified (38/38 total).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (120 test-result
blocks, 0 failures — the first full run hit one transient, known-
flaky failure in `ocicri_container.rs`'s own `create_container_bind_
mount_follows_a_symlinked_host_path`, exit code 126 "process exited
before exec", confirmed unrelated and passing instantly in isolation;
`ci/native-ci.sh`'s own first attempt hit a second, similarly
transient failure in the same file, `create_container_oom_score_adj_
sets_a_real_value`, also confirmed unrelated and passing instantly in
isolation — both consistent with the already-documented, accepted
environmental flakiness from this dev host's long-running CPU-
spinning background process; both scripts passed clean 120/120 on an
immediate retry), `python3 ci/guards.py`, `cargo deny check`, `bash
ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg -r` round trip). No
benchmark re-run needed: `ci/bench.sh` never exercises `ocicri` at
all, and the reshaped launcher argv protocol costs nothing extra for
a container using neither `log_path` nor `cgroup_parent` (an empty
string argument instead of an omitted one).

## Deliberately still out of scope

This closes the entire `--cgroup-parent`/`cgroup_parent` surface this
short series (`0465`-`0467`) has been tracking, across `ociman run`/
`create`/`build` and now `ocicri` alike. No further follow-ups
identified in this specific area.
