# Design note 0228: `ocicri` `RuntimeService.Status`

Status: implemented
Scope: `bin/ocicri/src/runtime_service.rs`.

## The second real `RuntimeService` RPC

0212 implemented `Version` (kubelet's own first connectivity check)
and left every one of the other 33 `RuntimeService` RPCs as a real,
honest `Status::unimplemented`. `Status` is the natural next
candidate: checked directly against real `cri-o`'s own implementation
(`server/runtime_status.go`), it's almost entirely a static/near-
static response — no real pod-sandbox/container-lifecycle machinery
needed at all, matching the same "narrow, safe, mostly-bookkeeping"
shape `ImageFsInfo` (0223) already established for `ImageService`.

## What real `cri-o` reports, checked directly

- `RuntimeCondition{type: "RuntimeReady", status: true}` — hard-coded
  unconditionally; there is no code path in real `cri-o` that can ever
  set this `false`. Answering the RPC at all is the only "proof"
  either implementation uses.
- `RuntimeCondition{type: "NetworkReady", ...}` — a real, live check:
  `CNIManager.ReadyOrError()`, backed by an actual, continuously
  polled CNI plugin `STATUS` call.
- `runtime_handlers`: one real entry per *configured* OCI runtime
  (`crio.conf`), each with real, probed `RuntimeHandlerFeatures`
  (recursive read-only mounts, user namespaces).
- `features` (`RuntimeFeatures`): both bits hard-coded `true` — a
  genuine, backed capability claim.
- `info` (verbose only): substantial real data — the configured pause
  image and the entire live `crio.conf`, JSON-marshaled.
- Never fails in practice (the only theoretical error path,
  `json.Marshal` of the verbose info blob, is unreachable).

## What `ocicri` reports today, and why each value is what it is

- `RuntimeReady = true` — matches real `cri-o` exactly, same
  reasoning: the server answering at all is the proof.
- `NetworkReady = false`, with a real, honest `reason`/`message` —
  unlike real `cri-o`'s live CNI poll, this project sets up no
  container networking of its own at all yet (no bridge, no pasta, no
  CNI — already documented, `docs/design/0147`). Reporting readiness
  here would be a real, false claim; a reasoned `false` (matching real
  `cri-o`'s own "NetworkPluginNotReady" shape when its own CNI plugin
  genuinely isn't ready) is the honest answer.
- `runtime_handlers`: exactly one entry, `name: ""` (the proto's own
  "empty string denotes the default handler" convention), both real
  feature bits `false` — this project has no configurable runtime-
  handler concept and hasn't implemented recursive-read-only-mounts or
  user-namespace support anywhere, so `false` is the truthful default,
  not real `cri-o`'s own per-runtime probed values (which this project
  has nothing analogous to probe).
- `features`: both bits `false` — neither `SupplementalGroupsPolicy`
  nor simultaneous host-network-plus-user-namespace support exists
  anywhere in this project yet, unlike real `cri-o`'s own backed `true`
  claims.
- `info` (verbose only): the same real, already-known `runtimeName`/
  `runtimeVersion` values `Version` itself already reports — never
  fabricated CNI/runtime config data this project doesn't have.
- Always succeeds, matching real `cri-o` — no real failure condition
  for a response this static.

## Verified

- Two new integration tests in `tests/tests/ocicri_version.rs` (same
  file `Version`'s own tests already live in — this is `Version`'s own
  natural sibling, not a separate RPC deserving a new file): a real
  connection over a real Unix socket confirms `RuntimeReady`/
  `NetworkReady` conditions, the single default runtime handler, and
  an empty `info` map when not verbose; a second test confirms
  `verbose: true` populates `info` with the real, honest values named
  above.
- Full workspace: `cargo build`, `cargo test --workspace` (95/95
  result blocks — `ocicri_version`'s own block grew 2→4, everything
  else unchanged — 0 failures), `cargo fmt --check`, `cargo clippy
  --all-targets -- -D warnings`, `python3 ci/guards.py` (18 capability
  groups, unaffected), `cargo deny check` (only the pre-existing
  benign warning), `bash ci/native-ci.sh`, hyperfine perf sanity on
  `ociman run --rm` (no regression — this change is entirely within
  `ocicri`, nowhere near `ociman`/`ocirun`'s own hot path).

## What's still not here

Every `RuntimeService` pod-sandbox/container-lifecycle RPC remains a
real, honest `Status::unimplemented` — `RunPodSandbox` in particular
genuinely requires real namespace/mount/infra-container work to be
meaningful at all (checked directly against real `cri-o`'s own
`sandbox_run_linux.go`, a real ~1600-line implementation), unlike
`Version`/`Status`/`ImageService`'s own already-implemented RPCs,
which are all safely, honestly answerable without it.
