# Design note 0214: `ocicri` `ImageService.PullImage`

Status: implemented (first real slice — no inline `AuthConfig`
support yet, `RemoveImage`/`ImageFsInfo`/`StreamImages` remain
unimplemented)
Scope: `bin/ocicri/src/image_service.rs` (`pull_image`,
`pull_image_blocking`); `bin/ocicri/Cargo.toml` (new `oci-registry`
dependency); `tests/tests/ocicri_pull_image.rs`.

## Continuing `ImageService`'s own build-out

0213 gave `ImageService` its two read-only RPCs. This increment adds
the one write RPC that maps cleanly onto an already-shared primitive:
`PullImage`, via the exact same `oci_registry::pull_unconditionally`
`ociman pull`/`ocibox create` already use — no new pull logic written
at all.

## Blocking work runs off the async runtime's own worker threads

`oci_registry`'s own client (`ureq`) is a plain, synchronous, blocking
HTTP client, shared unchanged with every other binary in this
workspace (0212's own module doc comment already explains why:
`tokio`/`tonic` are confined to `ocicri` alone, and this project has
no reason to introduce a second, async HTTP stack just for one RPC).
Calling a real, possibly multi-second network pull directly inside an
`async fn` would tie up one of this server's own tokio worker threads
for the whole round trip, degrading (or, with enough concurrent pulls,
starving) every other RPC the server is supposed to keep answering in
the meantime. `pull_image_blocking`'s own real work runs on
`tokio::task::spawn_blocking` instead — verified directly: a real,
slow (if doomed) pull attempt against an unreachable host is started,
and `RuntimeService.Version` on a wholly separate connection to the
very same server still answers promptly while that pull is still in
flight.

## What's deliberately not wired up yet

Real CRI's own `PullImageRequest.auth` (inline, kubelet-supplied
per-pull credentials, e.g. from a Kubernetes `imagePullSecret` —
distinct from the on-disk auth file `ociman login` populates) isn't
honored: `oci_registry::Credentials` only supports the on-disk-auth-
file shape today (`Credentials::load()`), with no public constructor
for an explicit, in-memory username/password pair — adding one is real,
additional scope, deliberately deferred rather than half-built. A pull
via `ocicri` always falls back to whatever `ociman login` has already
stored on disk, same as every other pull in this project.

`tls_verify` is hardcoded `true` (secure by default) — the real CRI
protocol has no equivalent per-request flag at all (unlike `ociman
pull --tls-verify=false`); which registries are "insecure" is a real,
cluster-level configuration decision (`/etc/containers/registries.conf`
in real `cri-o`) this project doesn't read yet.

## Verified by hand against the real, live registry

Since production always uses `tls_verify: true`, and this project's
own local mock-HTTP-registry test helper
(`ociman_pull_policy.rs`/`ociman_tls_verify.rs`'s own `MockRegistry`)
can't be reached with TLS verification on, a genuinely successful
pull-and-store round trip was verified by hand instead of as part of
the always-offline automated suite: a real `ocicri` server, a real
generated `tonic` client, a real `PullImage` RPC against the real,
live `docker.io/library/busybox:latest` — returned a real
`sha256:...` `image_ref`, and a follow-up `ListImages` call on the
same server confirmed the image really is in the store now, with the
correct id/tag/size/annotations. Matches this project's own
established convention for real-registry-touching verifications that
aren't part of the hermetic automated suite (e.g. push/pull round
trips against real `docker`/`podman`, 0122).

## Tests

Automated (always-offline, no live registry dependency): four real,
socket-connecting integration tests in `tests/tests/
ocicri_pull_image.rs` — an unreachable-host pull is a real
`Code::Unavailable` error (the same "prove a real network attempt
happened" technique `ociman_pull_policy.rs`'s own tests already
establish); no image specified is `Code::InvalidArgument`; an
uppercase (hence unparseable — `Reference::parse` rejects non-
lowercase paths) reference is `Code::InvalidArgument` before any
network attempt at all; and the blocking-pool proof described above.
One new in-process unit test (`pull_image`'s own "no image specified"
argument check, which needs no store or network access at all).

Full `cargo build --workspace --locked`/`cargo test --workspace
--locked` (2 clean runs, 94/94 result blocks — one new test binary;
`ocicri` now 5 unit tests up from 4)/`cargo fmt --all --check`/`cargo
clippy --workspace --all-targets --locked -- -D warnings`/`python3
ci/guards.py`/`cargo deny check`/`bash ci/native-ci.sh` all clean.
Two pre-existing, already-documented, non-actionable `VerityFs`
test-fixture stray mounts + loop devices found and cleaned up across
the two full test runs (routine habit, not a regression). No
performance regression to any other binary (`ociman run --rm`, ~68ms,
within this project's own previously-observed noise band).

## What this doesn't do yet

Inline `AuthConfig` (kubelet-supplied per-pull credentials),
`RemoveImage`, `ImageFsInfo`, and `StreamImages` remain real,
substantial, still-ahead future increments.
