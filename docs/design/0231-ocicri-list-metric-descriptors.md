# Design note 0231: `ocicri` `RuntimeService.ListMetricDescriptors`

Status: implemented
Scope: `bin/ocicri/src/runtime_service.rs`.

## The fifth genuinely implemented `RuntimeService` RPC

Continuing the exact trajectory 0212/0228/0229 already established:
implement only RPCs safely answerable without real pod-sandbox/
container-lifecycle machinery, checked line-by-line against real
`cri-o`'s own implementation first.

## What real `cri-o` actually does

`server/metric_descriptors_list.go`, checked directly: entirely
static/config-driven, never touches any real container/sandbox state.
Its own descriptor table (`internal/lib/statsserver/descriptors.go`)
is a hand-written Go map keyed by metric category (`cpu`, `memory`,
`network`, ...), gated by `crio.conf`'s own `included_pod_metrics`
setting — which **defaults to empty**. So a real, unconfigured `cri-o`
install already answers this RPC with almost nothing: exactly one
always-on descriptor, `container_last_seen`.

## Why `ocicri` reports a real, honest empty list — not even that one descriptor

`ocicri` has no metrics-collection machinery of its own anywhere yet —
no RPC in `RuntimeService`/`ImageService` populates any real
per-container metric value at all (`ListPodSandboxMetrics`/
`StreamPodSandboxMetrics` remain real, honest `Status::unimplemented`,
matching every pod-sandbox/container-lifecycle RPC). Advertising even
real `cri-o`'s own one always-on descriptor here would be a real,
false claim: a caller could reasonably expect a following
`ListPodSandboxMetrics` call to actually return a value for whatever
this RPC just told it exists. An empty list is genuinely the most
honest possible answer, not a placeholder or a simplification —
real `cri-o`'s own architecture already establishes that returning
nothing here is a normal, valid, unconfigured-install response, not an
error condition a real kubelet needs to special-case (the CRI spec/
client code treats this purely as an optional, alpha-ish "CRI-native
pod/container stats" path, never required for core pod
scheduling/running).

## Verified

- One new integration test in `tests/tests/ocicri_version.rs` (this
  RPC's own natural sibling alongside `Version`/`Status`/
  `RuntimeConfig`/`UpdateRuntimeConfig`): a real connection over a
  real Unix socket confirms `ListMetricDescriptors` returns a real,
  empty `descriptors` list.
- Full workspace: `cargo build`, `cargo test --workspace` (96/96
  result blocks — `ocicri_version`'s own block grew 6→7, everything
  else unchanged — 0 failures), `cargo fmt --check`, `cargo clippy
  --all-targets -- -D warnings`, `python3 ci/guards.py` (18 capability
  groups, unaffected), `cargo deny check` (only the pre-existing
  benign warning), `bash ci/native-ci.sh`, hyperfine perf sanity on
  `ociman run --rm` (no regression — this change is entirely within
  `ocicri`, nowhere near `ociman`/`ocirun`'s own hot path).

## What's still not here

`RuntimeService` now has 5 of 34 RPCs genuinely implemented (`Version`,
`Status`, `RuntimeConfig`, `UpdateRuntimeConfig`,
`ListMetricDescriptors`). Every pod-sandbox/container-lifecycle RPC —
the large, still-ahead core of this milestone — remains a real,
honest `Status::unimplemented`, along with `ListPodSandboxMetrics`/
`StreamPodSandboxMetrics` (this RPC's own natural, still-deferred
follow-ups, once real container/sandbox tracking exists to report
values for).
