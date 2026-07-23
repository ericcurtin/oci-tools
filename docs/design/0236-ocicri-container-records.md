# Design note 0236: `ocicri` container lifecycle, record slice

Status: implemented
Scope: `bin/ocicri/src/records.rs` (new), `bin/ocicri/src/container.rs`
(new), `bin/ocicri/src/sandbox.rs`, `bin/ocicri/src/runtime_service.rs`,
`bin/ocicri/src/main.rs`, `tests/tests/ocicri_container.rs` (new).

## The container half begins

With the pod-sandbox lifecycle landed (0233-0234), the next front is
the CRI container family — the RPCs kubelet drives between
`RunPodSandbox` and an actually-running pod. This increment implements
the four that need no process machinery at all:

- `CreateContainer` — a real, persistent record with real CRI
  semantics, state `CONTAINER_CREATED`.
- `ContainerStatus`, `ListContainers` — real readers of that state.
- `RemoveContainer` — idempotent, forceful removal.
- Plus the now-reachable half of an existing RPC: `RemovePodSandbox`
  forcibly removes the sandbox's own containers too (the proto's own
  contract; real cri-o's own `removePodSandbox` loop — previously
  moot here since no container could exist at all).

`StartContainer`/`StopContainer` (and exec/attach/logs/stats) stay
real, honest `Status::unimplemented`s: actually launching the process
— via the same shared `oci_runtime_core::launch` machinery
`ociman`/`ocirun`/`ocibox` already use — is its own bigger, later
increment. That's also what keeps this slice honest: every record it
can produce is `CONTAINER_CREATED`, and a container that can never
have been started also never needs `RUNNING`/`EXITED`, `started_at`,
or an exit code — the only state this slice writes is the only one
that can truthfully exist. (Real cri-o's own `CreateContainer` does
more — storage + a runtime `create` — but the CRI *contract* visible
to a kubelet at this stage is the state machine, the same
narrow-slice reasoning 0233 already applied to sandboxes and their
never-yet-spawned infra process.)

## Semantics, checked directly against real cri-o (not guessed)

From `~/git/cri-o` (`server/container_create.go`, `container_status.
go`, `container_list.go`, `container_remove.go`,
`internal/factory/container/container.go`):

**CreateContainer**

- Validation, real cri-o's own order and error strings: `config` /
  `config.image` / `sandbox_config` / `sandbox_config.metadata` all
  required; then the sandbox must resolve ("specified sandbox not
  found" otherwise) and must not be stopped ("CreateContainer failed
  as the sandbox was stopped", its own `sb.Stopped()` check —
  `FailedPrecondition` here); then `metadata` present and
  `metadata.name` non-empty (`SetConfig`'s own checks).
- The unique container name is exactly `SetNameAndID`'s own join:
  `k8s_<ctrName>_<podName>_<podNamespace>_<podUid>_<ctrAttempt>` —
  with the pod half taken from the *request's* `sandbox_config`
  (never cross-checked against the stored sandbox), matching real
  cri-o exactly.
- A duplicate request (same name) returns the existing container's
  ID as a success — the same duplicate-request branch
  `RunPodSandbox`/0233 already ported.
- The image must already be present locally (kubelet always pulls
  first, per its own pull policy; this RPC has no pull-policy input
  at all) — resolved via the same shared
  `oci_store::resolve_by_reference_or_id` the `ImageService` RPCs
  use; an unpulled image is a clear `NotFound` telling the caller to
  `PullImage` first. The resolved manifest digest is recorded as
  `image_ref`/`image_id`.

**RemoveContainer** — "must not return an error if the container has
already been removed" (the proto; real cri-o's own
`truncindex.ErrNotExist -> empty response` branch): unknown ID is a
silent success; empty ID is a real error; removal never requires a
prior stop.

**ContainerStatus** — unknown/empty ID is a real gRPC `NotFound`
("could not find container %q"), the same asymmetry-with-remove the
sandbox family already mirrors; echoes metadata/labels/annotations
verbatim, reports the requested image name (`image`), the resolved
digest (`image_ref`/`image_id`), `created_at`, and the state; verbose
fills the single-`"info"`-key JSON blob (the stored record — there is
no runtime spec or pid to marshal until `StartContainer` exists).

**ListContainers** — filters AND together (`filterContainerList`/
`filterContainer`, checked directly): an `id` filter resolves by
prefix and a miss/ambiguity is an empty list, never an error; `id` +
`pod_sandbox_id` together require the resolved container's sandbox to
*prefix-match* the given sandbox ID (cri-o's own
`strings.HasPrefix(c.Sandbox(), ...)`); `pod_sandbox_id` alone
resolves the sandbox by prefix and yields its containers;
`state`/`label_selector` filter the remainder.

Ambiguous ID prefixes are `InvalidArgument` where a caller named one
container directly (status/remove), continuing 0233's own documented
divergence-in-code-only from cri-o's coarser error wrapping.

## Shared record mechanics: `records.rs`

The moment a second record family needed the identical
save/load/prefix-resolve/remove mechanics `sandbox.rs` built in 0233,
they moved to one generic module (`records.rs`, a `Record` trait with
`id()`/`created_at_nanos()`) rather than being duplicated —
`sandbox.rs`'s public API and its tests are unchanged (now thin
delegations), `container.rs` is the second user, and the 64-hex ID
generator is shared too.

## Verified

- New unit tests: `container.rs` round-trip/prefix/name/remove;
  `runtime_service.rs` request-shape validations (no config, empty
  remove ID); the pre-existing sandbox store tests pass unmodified
  against the factored-out mechanics.
- `tests/tests/ocicri_container.rs`, over a real Unix socket against
  the real spawned binary with a real seeded image store: the full
  create/duplicate/new-attempt/list/status(+verbose,+prefix)/remove/
  idempotent-remove/NotFound lifecycle; validation and precondition
  rejections (unknown sandbox, stopped sandbox, unpulled image) each
  with cri-o's own message shape; every `ListContainers` filter rule
  including the id+sandbox cross-check; `RemovePodSandbox` cascading
  to the sandbox's containers; records surviving a real server
  kill/restart.
- The "unimplemented sample" tests (in-process and over-the-wire)
  moved off the now-implemented `CreateContainer`/`ListContainers` to
  the still-unimplemented `StartContainer`/`ExecSync`.
- Full workspace: `cargo build`, `cargo test --workspace`,
  `cargo fmt --all --check`, `cargo clippy --workspace --all-targets
  -- -D warnings`, `python3 ci/guards.py`, `cargo deny check`,
  `bash ci/native-ci.sh`, `ci/build-deb.sh`.
- Perf: confined to `ocicri` (the deliberate long-lived-server
  exception); no shared crate touched, no other binary changed.
