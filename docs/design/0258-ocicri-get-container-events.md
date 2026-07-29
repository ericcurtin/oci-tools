# Design note 0258: `ocicri GetContainerEvents`

Status: implemented
Scope: `bin/ocicri/src/runtime_service.rs`, `tests/tests/ocicri_version.rs`.

## Real cri-o's own actual default, traced through

`GetContainerEvents` is the newer, event-driven alternative to
kubelet's own PLEG polling. Checked directly against real cri-o's own
`server/container_events.go` before writing anything: the entire RPC
is gated behind its own `enable_pod_events` config toggle
(`pkg/config/config.go`) — a plain `bool` with no explicit default
assignment anywhere in `DefaultConfig()`, so Go's own zero value
(`false`) applies: a real, unconfigured `cri-o` install has this
feature off. When disabled, real cri-o's own implementation is exactly
one line — `return nil` — before ever touching its own event
broadcaster: a real, clean stream that closes with zero messages
immediately, never blocking to wait for an event at all.

This project has no event-generation machinery anywhere (no per-
container lifecycle broadcaster, and no `enable_pod_events`-equivalent
toggle to even gate one behind) — so the honest, real-cri-o-default-
matching answer is that identical immediately-closed, empty stream,
not a hard `Status::unimplemented` (which real cri-o's own default
install would never actually return here). The same "absence over
fabrication" reasoning `ListPodSandboxMetrics`/`StreamPodSandboxMetrics`
already established (`docs/design/0255`) applies again: matching a
real tool's own default behavior is more correct than either
fabricating events that don't exist or refusing the RPC outright.

## Verified

Integration (`tests/tests/ocicri_version.rs`,
`get_container_events_closes_immediately_with_no_messages`): the
stream ends with zero messages, matching every other
already-empty-by-design RPC's own identical behavior in this project.

Full workspace: `cargo build`/`test --workspace` (107 test binaries),
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets --
-D warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh`.

## Still ahead

A real per-container event bus (needed the moment this project ever
wants `enable_pod_events`-style behavior turned *on*, not just its
honest off-default) is a materially bigger feature — every container
lifecycle mutation point (`CreateContainer`/`StartContainer`/
`StopContainer`/`RemoveContainer`) would need to publish onto a shared
broadcast channel every currently-connected `GetContainerEvents`
stream forwards from, deliberately left for its own future increment.
`RuntimeService`'s other remaining real gaps: `Exec`/`Attach`/
`PortForward` (the streaming-server URL RPCs), `PodSandboxStats`/
`ListPodSandboxStats`/`StreamPodSandboxStats` (need a real
per-sandbox cgroup concept), and `CheckpointContainer` (needs CRIU).
