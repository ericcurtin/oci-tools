# Design note 0234: `ocicri` `StreamPodSandboxes`/`StreamImages`

Status: implemented
Scope: `bin/ocicri/src/stream.rs` (new), `bin/ocicri/src/runtime_service.rs`,
`bin/ocicri/src/image_service.rs`, `bin/ocicri/src/main.rs`,
`tests/tests/ocicri_pod_sandbox.rs`, `tests/tests/ocicri_image_service.rs`.

## The streaming list variants, now that their list forms exist

The CRI's `CRIListStreaming` feature gate (KEP-5825) adds a
server-streaming sibling to each bulk list RPC — `StreamPodSandboxes`,
`StreamContainers`, `StreamImages` — so a kubelet syncing a very large
node doesn't need one enormous response message. The proto's own
contract (its doc comment, identical on all three): every item appears
in exactly one response, never duplicated across responses in one
stream, and the server closes the stream with EOF after all items.

0213's `image_service.rs` module doc guessed real `cri-o` "may not
even implement" these. Re-checked directly against the current tree
(`~/git/cri-o`): it genuinely implements all three
(`server/image_list.go`'s `StreamImages`, `server/sandbox_list.go`'s
`StreamPodSandboxes`, `server/container_list.go`'s
`StreamContainers`), each the same trivial shape — run the *exact*
same filtered-list computation the plain list RPC uses, then send it
in chunks of `streamChunkSize = 3000` (`server/server.go`). An empty
result streams zero messages and closes immediately (the chunking
loop simply never iterates); a filter behaves identically to the list
RPC's own.

With 0233's `ListPodSandbox` landed, both `StreamPodSandboxes` and
`StreamImages` (whose list form has existed since 0213) now have real
list computations to share — so this increment implements both, the
same way real cri-o does:

- The filtered-list bodies of `list_pod_sandbox`/`list_images` are
  factored into plain helpers (`sandbox_list_items`/
  `image_list_items`); the list RPCs are now thin wrappers around
  them, verified unchanged by the existing tests passing unmodified.
- A new, tiny shared `stream.rs` owns the one piece both services
  need: `chunked()`, turning `Vec<T>` into a real `BoxStream` of
  chunk responses of at most `STREAM_CHUNK_SIZE = 3000` items
  (matching real cri-o's own constant), zero messages for zero items.
  Its boundary arithmetic (0 -> 0 chunks, 1 -> 1, exactly 3000 -> 1,
  3001 -> 2 sized [3000, 1]) is unit-tested directly — the one part a
  socket-level integration test can't practically exercise without
  fabricating 3001 real sandboxes.

`StreamContainers` stays a real, honest `Status::unimplemented`: there
is no container list of any kind here yet (`ListContainers` itself is
still unimplemented — the container lifecycle is its own, bigger
increment), so a streaming variant of it has nothing to stream.
`StreamContainerStats`/`StreamPodSandboxStats`/
`StreamPodSandboxMetrics` likewise remain unimplemented alongside
their own unimplemented non-streaming forms.

## Verified

- New unit tests in `stream.rs` for the chunk-boundary arithmetic
  (0/1/exactly-3000/3001 items).
- `tests/tests/ocicri_pod_sandbox.rs`: `StreamPodSandboxes` over a
  real Unix socket returns the same two sandboxes `ListPodSandbox`
  reports (one message, since 2 < 3000), honors a state filter
  identically to the list RPC, and streams zero messages (EOF
  immediately) for an empty store.
- `tests/tests/ocicri_image_service.rs`: `StreamImages` over a real
  Unix socket returns the same seeded image `ListImages` reports, and
  streams zero messages for an empty store.
- Existing list-RPC tests pass unmodified — the factored-out helpers
  are a pure, behavior-preserving move.
- Full workspace: `cargo build`, `cargo test --workspace`,
  `cargo fmt --all --check`, `cargo clippy --workspace --all-targets
  -- -D warnings`, `python3 ci/guards.py`, `cargo deny check`,
  `bash ci/native-ci.sh`, `ci/build-deb.sh`.
- Perf: confined to `ocicri` (the one deliberate long-lived-server
  exception to the startup-time pillar); no shared crate touched, no
  other binary's code changed at all.
