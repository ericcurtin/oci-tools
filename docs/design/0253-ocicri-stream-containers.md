# Design note 0253: `ocicri StreamContainers`

Status: implemented
Scope: `bin/ocicri/src/runtime_service.rs`, `tests/tests/ocicri_container.rs`.

## Completing the `CRIListStreaming` family

`docs/design/0234` implemented `StreamPodSandboxes` and `StreamImages`
— the two other `CRIListStreaming` variants of this project's own
already-existing plain list RPCs — leaving `StreamContainers` as the
one remaining real gap explicitly noted at the time ("`StreamContainers`
stays honestly unimplemented alongside its own still-unimplemented
`ListContainers`"). `ListContainers` itself landed shortly after
(0236); this slice closes the loop, completing the family exactly the
way `0234`'s own note anticipated.

## Pure composition, again

Identical shape to `stream_pod_sandboxes`/`stream_images`: the exact
same filtered-list computation `list_containers` already uses
(`container_list_items`), streamed through the one shared
`crate::stream::chunked` helper (`STREAM_CHUNK_SIZE = 3000`, real
cri-o's own `server/server.go` value, verbatim) that already backs
both of its siblings. No new logic of any kind — literally the same
five-line shape copied a third time, which is exactly the point: one
implementation of "filter, then chunk" shared three ways rather than
three near-identical hand-rolled loops.

## Verified

Integration (`tests/tests/ocicri_container.rs`,
`stream_containers_matches_list_and_streams_nothing_when_empty`,
mirroring `stream_pod_sandboxes`'s own equivalent test exactly):

- An empty sandbox streams zero messages before a clean EOF.
- Unfiltered, the stream reports the exact same containers
  `ListContainers` does, in one message (far fewer than the real
  3000-item chunk size).
- A label-selector filter behaves identically to the plain list RPC's
  own.

Full workspace: `cargo build`/`test --workspace` (108 test binaries),
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets --
-D warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh`.

## Still ahead

`RuntimeService`'s remaining real gaps: `Exec`/`Attach`/`PortForward`
(the streaming-server URL RPCs), `PodSandboxStats`/
`ListPodSandboxStats`/`StreamPodSandboxStats`, `CheckpointContainer`,
`GetContainerEvents`, `ListPodSandboxMetrics`/
`StreamPodSandboxMetrics`, and `UpdatePodSandboxResources`.
