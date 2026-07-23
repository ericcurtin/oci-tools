# Design note 0212: `ocicri` — a real gRPC server, `RuntimeService.Version` implemented

Status: implemented (first real slice; every other CRI RPC deliberately
still ahead)
Scope: new `crates/oci-cri-types` (vendored CRI v1 protobuf, generated
`tonic` client/server stubs, shared build-time compilation);
`bin/ocicri/src/main.rs` (real gRPC server bootstrap over a Unix
socket); `bin/ocicri/src/runtime_service.rs` (`RuntimeService` impl);
`ci/guards.py`/`deny.toml` (new capability groups, one documented
transient version-skew skip); `tests/tests/ocicri_version.rs`; a fix
to `tests/tests/smoke.rs`.

## Milestone 7's other real gap

`ocibox` (0205-0211) now has a real command family. `ocicri` had none
at all — a milestone-1 skeleton that only ever printed "not
implemented yet." This is that gap's own first real slice: a genuine,
running gRPC server, not a placeholder, answering the one RPC every
real CRI client (kubelet, `crictl`) calls first.

## Why `Version` first

`RuntimeService.Version` is the simplest, most fundamental RPC in the
whole real CRI v1 protocol (`proto/api.proto`, 33 other `RuntimeService`
RPCs plus 6 more on `ImageService`): no state, no container/sandbox
lifecycle, just "are you alive, what version are you" — kubelet's own
first connectivity/compatibility check against any runtime. Every
other RPC in this first slice returns a real `Status::unimplemented`
naming itself (e.g. `"ocicri: RunPodSandbox is not implemented yet
..."`), not a silently-accepted request this project can't actually
act on — the same "narrow first slice, document the rest" pattern used
throughout this project (e.g. `ociboot build-image` before `install
to-disk`).

## Real, vendored CRI protobuf, not an invented approximation

`crates/oci-cri-types/proto/api.proto` is an unmodified copy of the
real `k8s.io/cri-api/pkg/apis/runtime/v1/api.proto`, vendored from real
`cri-o`'s own vendor tree (Apache-2.0, see the crate's own `proto/
README.md`) — the exact schema real `kubelet`/`crictl` speak, so
`ocicri` is a genuine drop-in CRI implementation, never a hand-rolled
protocol stand-in that could silently drift. One small, fully
documented deviation: four `[debug_redact = true]` field options on
`AuthConfig` are stripped, since this project's own build-time
`protoc` (3.21.12) predates the protobuf release that added
`debug_redact` as a real `google.protobuf.FieldOptions` field — this
changes nothing about the wire format, field numbers, or types (only a
debug/log-redaction hint), so every message stays byte-for-byte
wire-compatible with the real upstream schema either way.

## A new shared crate, not `ocicri`-private generated code

The protobuf compilation (`tonic-prost-build`, needing a real `protoc`
on `$PATH`) originally lived directly in `bin/ocicri`, but moved into
its own `crates/oci-cri-types` once it became clear `oci-tools-tests`
needed the identical generated *client* stubs too, to write a real,
socket-connecting integration test rather than only in-process unit
tests — matching this project's own "share as much code as possible"
pillar and its established "crates/ owns real, reusable capabilities"
convention exactly (the same reasoning `oci_registry::resolve_or_pull`
was extracted for in 0204).

## The one deliberate async-runtime exception

`tonic`/`tokio`/`prost` are new workspace dependencies, confined
entirely to `ocicri`/`oci-cri-types` — every other binary in the
workspace still links neither at all, so this project's own "beat
every benchmark, especially startup time" design pillar is unaffected
for `ocirun`/`ociman`/`ocibox`/`ociboot`. `ocicri` itself is the one
deliberate exception: a real, long-lived server process, not a
short-lived CLI invocation, so its own *serving* performance (not a
one-time process startup) is what actually matters. New capability
groups registered in `ci/guards.py` (`gRPC`, `protobuf codegen`,
`async runtime`) document this choice the same way every other
capability decision in this project already is.

## Socket path, matching real `cri-o`'s own model

`--listen <PATH>` (default: `oci_cli_common::runtime_root::default_
root("ocicri").join("ocicri.sock")` — the exact shared "runtime root"
helper whose own doc comment already anticipated this: `/run/ocicri`
for root, `$XDG_RUNTIME_DIR/ocicri` rootless, mirroring real `cri-o`'s
own `/var/run/crio/crio.sock` convention). A stale socket file from an
earlier, uncleanly-terminated run is removed before binding, matching
real `cri-o`'s own identical startup behavior.

## A real test-suite fix this surfaced

`tests/tests/smoke.rs` assumed *every* binary fails loudly on a bare,
argument-less invocation — true for every binary until now, but wrong
for `ocicri`: a bare `ocicri` (or `ocicri --json`) is real, valid
default behavior (start the server and block), matching real `cri-o`
itself (invoking it just *is* running the daemon). The naive
`Command::output()`-based smoke-test helper would otherwise hang
forever waiting for a process that's supposed to keep running — caught
directly (a real test hang during this same increment's own
verification, not simulated), fixed by excluding `ocicri` from the
"must error quickly" checks and adding its own dedicated test
(`ocicri_json_flag_is_accepted_and_the_server_still_starts`, which
confirms the opposite: `--json` parses and the server keeps running a
moment later, rather than exiting from an argument-parsing failure).

## Verified by hand

* `ocicri --listen <path>` binds a real Unix socket; a stale socket
  file from an earlier run doesn't block a fresh start.
* A real, generated `tonic` client (`oci_cri_types::runtime_service_
  client::RuntimeServiceClient`, connected over the real socket via the
  standard `tower::service_fn` + `hyper_util::rt::TokioIo` Unix-socket
  connector recipe) calls `Version` and gets back real, honest values:
  `version: "0.1.0"` (the historical CRI kubelet-API-version constant
  every real runtime returns, checked directly against real `cri-o`'s
  own `server/version.go`), `runtime_name: "ocicri"` (this project's
  own real name, not `"cri-o"`), `runtime_api_version: "v1"`, and a
  real, non-empty `runtime_version` build string.
* Every other RPC (checked directly: `ListPodSandbox`,
  `RunPodSandbox`) returns a real `Code::Unimplemented` status whose
  own message names the real RPC, over the real wire protocol, not
  just the in-process unit test.

## Tests

Two new unit tests in `bin/ocicri/src/runtime_service.rs` (in-process,
no real socket: `Version`'s own real values, one other RPC's own real
`Unimplemented` status). Two new real, socket-connecting integration
tests in `tests/tests/ocicri_version.rs` (a real client, a real running
server, a real Unix socket). One new smoke test
(`ocicri_json_flag_is_accepted_and_the_server_still_starts`) plus a fix
to two pre-existing ones (`bare_invocation_is_a_loud_error`/
`clap_bins_render_error_chain_format`/`json_flag_is_accepted_
globally`, all now correctly excluding `ocicri`).

Full `cargo build --workspace --locked`/`cargo test --workspace
--locked` (2 clean runs, 92/92 result blocks — three new test
binaries: `ocicri`'s own unit tests, `oci-cri-types`' own (currently
empty) unit tests, and `ocicri_version.rs`)/`cargo fmt --all --check`/
`cargo clippy --workspace --all-targets --locked -- -D warnings`/
`python3 ci/guards.py` (18 capability groups now, three new)/`cargo
deny check` (one new, documented transient-duplicate skip:
`hashbrown@0.15.5`, pulled in by `petgraph`/`prost-build`'s own
dependency-graph analysis, alongside `indexmap`'s newer `hashbrown`
via the `h2`/`hyper` stack — the same "ecosystem forces a transient
duplicate" case `deny.toml` already documents for `getrandom`/
`windows-sys`/`syn`)/`bash ci/native-ci.sh` all clean. One pre-existing,
already-documented, non-actionable `VerityFs` test-fixture stray mount
+ loop device found and cleaned up after a full test run (routine
habit, not a regression). No performance regression to any other
binary (`ociman run --rm`, ~75ms, within this project's own
previously-observed 60-80ms noise band — `ociman` links neither
`tokio` nor `tonic` at all).

## What this doesn't do yet

The other 33 `RuntimeService` RPCs (pod sandbox/container lifecycle,
exec/attach/port-forward, stats, events, checkpoint, runtime config)
and all of `ImageService` (not even registered on the server yet) are
real, substantial, still-ahead future increments — this slice is
deliberately just "the server exists, is reachable, and tells the
truth about what it can't do yet."
