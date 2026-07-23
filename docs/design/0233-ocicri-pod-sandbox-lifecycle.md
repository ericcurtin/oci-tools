# Design note 0233: `ocicri` pod sandbox lifecycle (record-keeping slice)

Status: implemented
Scope: `bin/ocicri/src/sandbox.rs` (new), `bin/ocicri/src/runtime_service.rs`,
`bin/ocicri/src/main.rs`, `tests/tests/ocicri_pod_sandbox.rs` (new).

## The first pod-sandbox slice

After `Version`/`Status`/`RuntimeConfig`/`UpdateRuntimeConfig`/
`ListMetricDescriptors` (0212, 0228, 0229, 0231), every remaining
`RuntimeService` RPC was still a real, honest `Status::unimplemented` —
including the entire pod-sandbox family, which is the actual front
door to everything else `RuntimeService` does: a real kubelet's own
pod sync loop starts with `RunPodSandbox`, and no `CreateContainer`
can ever be attempted against a runtime that can't hold a sandbox at
all.

This increment implements the five pod-sandbox lifecycle RPCs —
`RunPodSandbox`, `StopPodSandbox`, `RemovePodSandbox`,
`PodSandboxStatus`, `ListPodSandbox` — as a real, persistent,
record-keeping state machine with real CRI semantics, checked
directly against real `cri-o`'s own source, and deliberately **not**
yet a real infra ("pause") process or any pinned namespaces:

- This project sets up no container networking at all yet (no bridge,
  no pasta, no CNI — `docs/design/0147`; `Status` itself already
  reports a reasoned `NetworkReady=false`). A pod network namespace
  is the main thing a real sandbox's own namespace pinning exists to
  hold.
- Real `cri-o` itself, with its own `drop_infra_ctr` default (checked
  directly: `pkg/config/config.go`'s `DefaultConfig` sets
  `DropInfraCtr: true`), runs **no infra process at all** for an
  ordinary sandbox — it pins the shared namespaces via `pinns` bind
  mounts instead and spoofs the infra container as pure bookkeeping
  (`sb.InfraContainer().Spoofed()`). So "a sandbox with no live
  process of its own" is real cri-o's own normal shape too; the part
  this slice genuinely doesn't have yet is the namespace pinning,
  which is deferred until this project grows real pod networking
  (and a real `CreateContainer` that would join those namespaces —
  itself still a real `Status::unimplemented` today, so nothing can
  even ask to join yet).

What *is* real here is the full CRI state machine kubelet drives —
creation with real name/ID semantics, `READY` -> `NOTREADY` on stop,
removal, status, filtered listing — persisted on disk so a restarted
`ocicri` still knows its sandboxes, exactly like real cri-o restores
its own sandbox state from `containers/storage` rather than starting
amnesiac.

## Semantics, checked directly against real cri-o (not guessed)

All from `~/git/cri-o` (`server/sandbox_run{,_linux}.go`,
`sandbox_stop.go`, `sandbox_remove.go`, `sandbox_status.go`,
`sandbox_list.go`, `internal/lib/sandbox/builder.go`):

**RunPodSandbox**

- Validation, in the same order real cri-o's own
  `sandboxBuilder.SetConfig`/`GenerateNameAndID` applies: `config`
  must be present; `metadata` must be present; `metadata.name`,
  `metadata.namespace` and `metadata.uid` must all be non-empty
  (`attempt` genuinely defaults to 0, per the proto).
- The sandbox's own unique *name* is exactly cri-o's own
  `k8s_<name>_<namespace>_<uid>_<attempt>` join; its *ID* is 64 random
  hex chars (`stringid.GenerateNonCryptoID`'s own shape — here a
  sha256 over time/pid, the same dependency-free technique `ociman`'s
  own `short_id`/`ocibox ephemeral` already use, just untruncated).
- Labels kubelet always sets but `crictl` doesn't are populated if
  missing, matching `populateSandboxLabels` exactly:
  `io.kubernetes.pod.name`, `io.kubernetes.pod.namespace`,
  `io.kubernetes.pod.uid` (checked against the real
  `kubeletTypes.KubernetesPod*Label` constants).
- A duplicate request (same generated name, i.e. same
  name/namespace/uid/attempt) returns the **existing** sandbox's ID
  as a success, matching `reservePodNameOrGetExisting`'s own "if
  we're able to find the sandbox, and it's created, this is actually
  a duplicate request. Just return that sandbox" branch — real
  kubelet retries after a lost response depend on this.
- A non-empty `runtime_handler` is rejected: real cri-o validates the
  handler against its own configured runtime table; `ocicri` has no
  configurable runtime-handler concept at all (its own `Status` RPC
  already reports exactly one default handler, `name: ""`), so the
  only honest behavior for an unknown-by-definition handler is the
  same rejection the proto itself demands ("If the runtime handler is
  unknown, this request should be rejected").

**StopPodSandbox**

- An empty ID is a real error (cri-o's own `sandbox.ErrIDEmpty`).
- An unknown ID is a **silent, empty success** — real cri-o's own
  explicit comment: "If the sandbox isn't found we just return an
  empty response to adhere the CRI interface which expects to not
  error out in not found cases."
- Stopping an already-stopped sandbox is idempotent (cri-o's own
  `stopPodSandbox` checks `sb.Stopped()` first). State goes
  `SANDBOX_READY` -> `SANDBOX_NOTREADY`, persisted atomically.

**RemovePodSandbox**

- Same empty-ID error and same silent not-found success as stop
  (checked: `sandbox_remove.go` has the identical comment).
- Removal is unconditional/forceful — the proto: "If there are any
  running containers in the sandbox, they must be forcibly
  terminated and removed"; real cri-o's `removePodSandbox` never
  requires a prior stop. Here that means deleting the record whether
  `READY` or `NOTREADY`.

**PodSandboxStatus**

- An unknown (or empty) ID is a real gRPC `NotFound` — real cri-o
  wraps *every* lookup failure here in `codes.NotFound` ("could not
  find pod %q"), unlike stop/remove. (One deliberate divergence in
  *code* only, applied uniformly across stop/remove/status: a
  genuinely ambiguous ID prefix is `InvalidArgument` — "which one did
  you mean?" is a client-input problem, the same precedent
  `RemoveImage` (0215) set for an ambiguous image ID — where real
  cri-o's coarser wrapping would report its truncindex's ambiguity
  error as `NotFound`/`Unknown`.)
- The response echoes the stored metadata/labels/annotations
  (annotations "MUST be identical to that of the corresponding
  PodSandboxConfig", per the proto — so they're stored verbatim and
  never synthesized), the real recorded `created_at` (nanoseconds),
  the current state, an empty `PodSandboxNetworkStatus` (real cri-o
  always sets `Network: &types.PodSandboxNetworkStatus{}` and only
  fills `ip` when a CNI actually provided one — `ocicri` has no CNI,
  so an empty message is both shape-identical and honest), and
  `linux.namespaces.options` echoing the namespace options the
  request itself declared (real cri-o's own status echoes
  `sb.NamespaceOptions()`, which is likewise the *requested* config,
  not a live probe).
- `verbose` fills `info["info"]` with a small JSON blob (the stored
  record itself) — matching real cri-o's own single-`"info"`-key
  shape (`createSandboxInfo`), with honestly less inside it (cri-o
  marshals the infra container's runtime spec; there is no runtime
  spec here to report, and fabricating one would be a false claim).

**ListPodSandbox**

- Filters combine with AND, matching `filterSandboxList`: an `id`
  filter resolves by unambiguous prefix (real cri-o goes through its
  own `PodIDIndex()` truncindex, which is prefix-based); an id that
  matches nothing yields an **empty list, never an error** (cri-o:
  "Not finding an ID in a filtered list should not be considered an
  error"); a `state` filter compares exactly; `label_selector`
  requires every given key/value pair to match
  (`fields.SelectorFromSet` over match-labels only, ANDed).

Where this project's gRPC *codes* for validation errors differ from
real cri-o's (cri-o returns plain wrapped errors for a nil config /
empty metadata, which gRPC surfaces as `Unknown`), `ocicri` uses
`InvalidArgument` — deliberately: same rejection, more precise code,
consistent with the precedent `ImageStatus`/`PullImage`/`RemoveImage`
(0213–0215) already set for "no image specified".

## Persistence

One JSON file per sandbox at `<storage-root>/cri-sandboxes/<id>.json`
(under `oci_cli_common::storage::default_root()`, the same root every
other piece of on-disk state in this project already lives under),
written atomically via the same temp-file-plus-rename technique
`oci_store`'s own pointer files use. Records survive an `ocicri`
restart — verified by a real integration test that kills the server
and spawns a fresh one against the same storage root.

Mutating RPCs serialize on one `std::sync::Mutex` inside
`RuntimeServiceImpl` so two concurrent `RunPodSandbox` calls with the
same metadata can't both miss the duplicate-name check and write two
records for one pod. Reads (`status`/`list`) stay lock-free plain
file reads, the same model `ImageService` already uses against
`oci_store`.

This lives in `bin/ocicri/src/sandbox.rs`, not a `crates/` library:
the repo rule is that *shared* logic lives in `crates/`, and no other
binary has any concept of a CRI pod sandbox (exactly like
`image_service.rs`'s own CRI-specific mapping code, which also lives
in the binary while everything genuinely shared sits in
`oci_store`/`oci_registry`).

## What this deliberately doesn't do yet

- No infra/pause process, no pinned namespaces, no pod network, no
  hostname/`/etc/hosts`/shm setup, no cgroup parent creation — all
  documented above; the sandbox is real bookkeeping with real CRI
  semantics, which is also all a kubelet can *observe* until
  `CreateContainer` exists.
- No port mappings, DNS config handling, or log-directory creation.
- `CreateContainer`/`StartContainer`/... remain real, honest
  `Status::unimplemented`s — the container half of the lifecycle is
  its own, bigger increment (it's where `oci_runtime_core::launch`
  reuse actually starts).

## Verified

- Unit tests in `sandbox.rs` cover the record store directly
  (tempdir-rooted, no process-global env mutation): save/load
  round-trip, atomic overwrite, prefix resolution (exact, unique
  prefix, ambiguous prefix, no match), name lookup, removal.
- `tests/tests/ocicri_pod_sandbox.rs` drives the full lifecycle over
  a real Unix socket against the real spawned binary: run -> list
  (one `READY`) -> status -> stop -> status (`NOTREADY`) -> stop
  again (idempotent) -> remove -> list (empty) -> remove again
  (idempotent) -> status (`NotFound`); duplicate `RunPodSandbox`
  returns the same ID; validation rejections (no config, no
  metadata, empty uid, unknown runtime handler); stop/remove of an
  unknown ID succeed silently; state and label filters; records
  survive a real server kill/restart.
- Full workspace: `cargo build`, `cargo test --workspace` (1408
  passed, 0 failed), `cargo fmt --all --check`, `cargo clippy
  --workspace --all-targets -- -D warnings`, `python3 ci/guards.py`
  (18 capability groups, unaffected), `cargo deny check` (only the
  pre-existing benign license-allowance warning),
  `bash ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/
  `--version`/`dpkg -r` round trip).
- `ci/bench.sh` perf sanity, expected unaffected (this change is
  confined to `ocicri`, the one deliberate long-lived-server
  exception to the startup-time pillar; no shared crate was touched)
  and confirmed so: `ocirun run` 2.9ms vs crun 6.6ms/runc 20.6ms,
  `ociman run --rm` 33.4ms vs podman 190.9ms/docker 286.5ms,
  `ociman rm` 1.3ms vs `podman rm` 68.4ms — every headline
  comparison still a decisive win.
- One pre-existing test needed its example updated, not a behavior
  change: `tests/tests/ocicri_version.rs`'s own
  `an_unimplemented_rpc_is_a_real_honest_status_over_the_wire` used
  `ListPodSandbox` as its sample unimplemented RPC (as did
  `runtime_service.rs`'s own in-process equivalent, with
  `RunPodSandbox`); both now use `CreateContainer`/`ListContainers`,
  which remain genuinely unimplemented.
