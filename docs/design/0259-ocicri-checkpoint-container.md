# Design note 0259: `ocicri CheckpointContainer`

Status: implemented
Scope: `bin/ocicri/src/runtime_service.rs`, `tests/tests/ocicri_version.rs`.

## A real error, not a fabricated checkpoint — but the *right* real error

Checked directly against real cri-o's own `server/container_checkpoint.go`
before writing anything, which turned up something less obvious than
`GetContainerEvents`'/`ListPodSandboxMetrics`' own simple "config
defaults to off" story (`docs/design/0255`/`0258`): real cri-o's own
config actually *defaults* `EnableCriuSupport` to `true`
(`pkg/config/config.go`'s own `DefaultConfig`) — but at daemon
startup it's force-disabled again unless a real `criu` binary is
actually found on `$PATH` (`validateCriuInPath`). Checkpoint/restore is
a niche, opt-in Linux capability essentially no host has installed by
default, so the practical, overwhelmingly common real behavior is
still disabled either way, just reached via a runtime binary check
rather than a static config bool.

When disabled, real cri-o's own implementation is a bare Go
`errors.New("checkpoint/restore support not available")` — critically,
**never wrapped in a `status.Error`**, so real gRPC's own default error
interceptor surfaces it as `codes.Unknown`, not some more specific code
(`InvalidArgument`/`Unimplemented`/etc.) — before ever resolving the
container or touching anything else.

This project has no CRIU/checkpoint-restore integration at all: a real
container checkpoint needs matching podman/cri-o's own checkpoint
archive format field for field (metadata, rootfs diff, CRIU's own
process-state dump) — a materially large feature, deliberately out of
scope, and a structurally *different* reason than real cri-o's own
"usually-missing binary" one. But the honest, observable answer is
identical either way: a real error, not a silent success or a
fabricated checkpoint archive. This slice uses real cri-o's own exact
message and status code rather than the generic `Status::unimplemented`
every other still-missing RPC uses, since `codes.Unknown` +
"checkpoint/restore support not available" *is* what a real,
unconfigured (or CRIU-less) `cri-o` install actually returns here too —
a genuine fidelity improvement for a CRI client that specifically
checks for this real, known error shape.

## Verified

Integration (`tests/tests/ocicri_version.rs`,
`checkpoint_container_reports_the_real_disabled_error`): a real
`codes.Unknown` status with the exact message
`"checkpoint/restore support not available"`.

Full workspace: `cargo build`/`test --workspace` (107 test binaries),
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets --
-D warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh`.

## Still ahead

`RuntimeService`'s remaining real gaps: `Exec`/`Attach`/`PortForward`
(the streaming-server URL RPCs, needing a real HTTP streaming server
distinct from this gRPC server) and `PodSandboxStats`/
`ListPodSandboxStats`/`StreamPodSandboxStats` (need a real
per-sandbox cgroup concept this project doesn't have yet). A real
CRIU integration for `CheckpointContainer` itself remains a
deliberately out-of-scope, materially larger feature.
