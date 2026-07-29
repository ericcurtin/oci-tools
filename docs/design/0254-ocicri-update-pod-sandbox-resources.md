# Design note 0254: `ocicri UpdatePodSandboxResources`

Status: implemented
Scope: `bin/ocicri/src/runtime_service.rs`, `tests/tests/ocicri_pod_sandbox.rs`.

## `UpdateContainerResources`'s pod-level sibling

`UpdatePodSandboxResources` is the pod-level counterpart of
`UpdateContainerResources` (0251) — part of the same in-place
pod-resize CRI surface. Checked directly against real cri-o's own
`server/sandbox_update_resources.go` before writing anything: beyond
resolving the sandbox, real cri-o's own implementation does *nothing*
to the sandbox's own cgroup directly at all. Every actual resource
change is delegated entirely to its own optional NRI (Node Resource
Interface) plugin framework (`s.nri.updatePodSandbox`), which is a
real, honest no-op with no plugins configured — the default, and the
only configuration either project's own CI ever runs.

This project has no NRI concept at all (a materially bigger, separate
plugin framework, entirely out of scope) — so this slice is honestly
exactly that same no-op once the sandbox is confirmed to exist, not a
fabricated cgroup write this project has nowhere to route anyway:
unlike `UpdateContainerResources`, `ocicri` has no per-sandbox cgroup
of its own at all (`docs/design/0233`'s own "no infra process" note —
an ordinary sandbox here has no live process, let alone a cgroup, to
write resource limits into).

## The one real behavior

- An unknown `pod_sandbox_id` is a real gRPC `NotFound`, matching real
  cri-o's own identical `getPodSandboxFromRequest` error wrapping.
- A real, existing sandbox is a real, honest success — regardless of
  whatever `overhead`/`resources` the request carries, since there is
  genuinely nothing further to do with them (matching real cri-o's own
  default, no-NRI behavior exactly, not a divergence).

## Verified

Integration (`tests/tests/ocicri_pod_sandbox.rs`,
`update_pod_sandbox_resources_resolves_the_sandbox_or_reports_not_found`):
a real, already-`RunPodSandbox`-created sandbox succeeds; an unknown
64-hex ID is a real `NotFound`.

Full workspace: `cargo build`/`test --workspace`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`python3 ci/guards.py`, `cargo deny check`, `ci/native-ci.sh`,
`ci/build-deb.sh`.

## Still ahead

`RuntimeService`'s remaining real gaps: `Exec`/`Attach`/`PortForward`
(the streaming-server URL RPCs), `PodSandboxStats`/
`ListPodSandboxStats`/`StreamPodSandboxStats` (need a real per-sandbox
cgroup concept this project doesn't have yet), `CheckpointContainer`,
`GetContainerEvents`, and `ListPodSandboxMetrics`/
`StreamPodSandboxMetrics`.
