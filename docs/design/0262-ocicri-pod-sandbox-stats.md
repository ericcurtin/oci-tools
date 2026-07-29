# Design note 0262: `ocicri PodSandboxStats`/`ListPodSandboxStats`/`StreamPodSandboxStats`

Status: implemented
Scope: `bin/ocicri/src/runtime_service.rs`, `tests/tests/ocicri_pod_sandbox.rs`.

## A genuinely different shape than the last two "absence" slices

`ListPodSandboxMetrics`/`StreamPodSandboxMetrics` (0255) and
`GetContainerEvents` (0258) both matched a real cri-o config default
that's *off* — a simple, clean "the real answer is empty, always."
`PodSandboxStats` doesn't have that same story: checked directly
against real cri-o's own `server/sandbox_stats.go`/`internal/lib/
statsserver/stats_server_linux.go`, it isn't gated behind any
disabled-by-default config flag at all — a real, ordinary cri-o
install genuinely computes and returns live cgroup/network numbers
here, because its own sandboxes always have a real infra ("pause")
container and its own cgroup to read from.

This project's own sandboxes deliberately have neither (`docs/design/
0233`'s own "no infra process" note) — a structural design difference
from real cri-o, not a configuration one. So the honest answer here
follows a different, already-established rule instead: `ContainerStats`
(0241) already draws exactly this line for containers with no live
cgroup ("absence over fabrication" — no live cgroup means no stats
row, never a fabricated zero one). This slice applies the identical
rule at the sandbox level: `Attributes` (id/metadata/labels/
annotations) are real and always available straight from the sandbox
record, so they're reported in full; `Linux` is always `None`, since
there is genuinely no live cgroup to read a single real number from.

## Composition

`pod_sandbox_stats_items` mirrors `container_stats_items` (0241)'s own
already-established pattern exactly: reuse the plain list RPC's own
filtered-resolution shape by mapping the stats filter's narrower
`id`/`label_selector` fields onto it (`PodSandboxStatsFilter` has no
`state` field at all, unlike `PodSandboxFilter` — the real proto's own
narrower filter message for this RPC family). `PodSandboxStats` itself
resolves the sandbox first (a real `NotFound` for an unknown one,
matching every other single-sandbox RPC's own identical rule);
`ListPodSandboxStats`/`StreamPodSandboxStats` share the one filtered
computation, the streaming variant through the same `crate::stream::
chunked` helper every other `CRIListStreaming` RPC already uses.

## Verified

Integration (`tests/tests/ocicri_pod_sandbox.rs`,
`pod_sandbox_stats_reports_real_attributes_with_no_linux_section`):

- An unknown sandbox is a real `NotFound`.
- A real, existing sandbox's `PodSandboxStats` reports real
  `Attributes` (id/labels) with `Linux` honestly absent.
- `ListPodSandboxStats` reports the one real sandbox; a label filter
  matching nothing returns an empty list, never an error.
- `StreamPodSandboxStats` reports the identical set to the plain list.

Full workspace: `cargo build`/`test --workspace` (108 test binaries),
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets --
-D warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh`.

## Still ahead

`RuntimeService`'s remaining real gaps: `Exec`/`Attach`/`PortForward`
(the streaming-server URL RPCs, needing a real HTTP streaming server
distinct from this gRPC server). A real per-sandbox cgroup/infra-
process concept (needed for `Linux` stats to ever report something
real here) is a materially bigger, separate feature.
