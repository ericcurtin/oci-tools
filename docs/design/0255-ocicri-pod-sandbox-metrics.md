# Design note 0255: `ocicri ListPodSandboxMetrics`/`StreamPodSandboxMetrics`

Status: implemented
Scope: `bin/ocicri/src/runtime_service.rs`, `tests/tests/ocicri_version.rs`.

## Real cri-o's own actual unconfigured behavior, traced through

`ListMetricDescriptors` (0231) already established that a real,
unconfigured `cri-o` install answers with almost nothing (`crio.conf`'s
own `included_pod_metrics` defaults to empty) — but at the time, its
own doc comment reasoned that its two metric-*value* siblings should
stay `Status::unimplemented` rather than also answering honestly,
since advertising even one descriptor with no RPC to back it up would
be worse than refusing outright.

Revisiting that reasoning by actually tracing through real cri-o's own
`server/sandbox_metrics_list.go` (`listPodSandboxMetrics`, the shared
helper behind both `ListPodSandboxMetrics` and its `StreamPodSandbox
Metrics` streaming sibling): it walks every real sandbox and asks the
stats subsystem for that sandbox's own already-computed metric — but
with no `included_pod_metrics` configured (this project's own only
real point of comparison, and real cri-o's own default too), that
computed metric is always genuinely absent for every sandbox, and the
function's own loop contributes nothing to its result for any of them.
Real cri-o's own real, unconfigured answer to this RPC is a plain,
successful, empty list — never an error, and never a per-sandbox entry
with empty fields either.

So the earlier `Status::unimplemented` wasn't actually the honest
answer — it was more conservative than real cri-o's own real behavior
warranted. This slice corrects that: both RPCs now report the same
real, unconditional empty answer `ListMetricDescriptors` already does,
for the identical underlying reason (no metrics-collection machinery
anywhere in this project), matching real cri-o's own unconfigured
install exactly rather than diverging from it.

## Verified

Integration (`tests/tests/ocicri_version.rs`,
`pod_sandbox_metrics_rpcs_report_a_real_honest_empty_answer`):
`ListPodSandboxMetrics` returns a real, empty `pod_metrics`, and its
`CRIListStreaming` sibling streams zero messages before a clean EOF —
matching every other `CRIListStreaming` RPC's own identical
empty-input behavior.

Full workspace: `cargo build`/`test --workspace` (107 test binaries),
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets --
-D warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh`.

## Still ahead

`RuntimeService`'s remaining real gaps: `Exec`/`Attach`/`PortForward`
(the streaming-server URL RPCs, a materially bigger feature needing a
real HTTP streaming server distinct from this gRPC server),
`PodSandboxStats`/`ListPodSandboxStats`/`StreamPodSandboxStats` (need
a real per-sandbox cgroup concept this project doesn't have yet),
`CheckpointContainer` (needs CRIU), and `GetContainerEvents` (needs a
real event-broadcast mechanism across every container lifecycle
transition point).
