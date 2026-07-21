# Design note 0127: `ociman push`

Status: implemented
Scope: `crates/oci-registry/src/client.rs` (`request_with_auth`
generalized to accept a caller-supplied send closure and an explicit
token scope; `blob_exists`/`upload_blob`/`push_manifest` new);
`crates/oci-registry/src/push.rs` (new, mirrors `pull.rs`); `crates/
oci-registry/src/lib.rs` (module doc, exports); `bin/ociman/src/
main.rs` (`Command::Push`, `cmd_push`); `tests/tests/ociman_push.rs`
(new, 2 tests).

## A real, significant gap

`oci-registry`'s own Cargo.toml description already said "pull/push",
but nothing in the crate — or `ociman`'s own CLI — ever implemented
push at all. This is a much bigger gap than the recent `login`/
`inspect`/`rmi`/`tag`-by-ID increments: without push, a build-then-
publish workflow (the single most common real CI/CD use of `docker
build`/`podman build` + `docker push`/`podman push`) simply isn't
possible with `ociman` at all.

## `request_with_auth` generalized, existing pull behavior unchanged

The existing bearer-token challenge/retry orchestration
(`request_with_auth`) was hardcoded to a GET with a `"pull"` token
scope. Push needs `HEAD`/`POST`/`PUT` and a `"pull,push"` scope
(checked directly: a real registry's own challenge for a push
operation asks for both actions, not `"push"` alone). Refactored to
take a caller-supplied `send: impl Fn(&Client, Option<&str>) -> ...`
closure (building and issuing whatever the specific request actually
is) plus an explicit `scope_actions: &str`, instead of a fixed
`url`/`headers` pair — the auth orchestration itself (cache lookup,
challenge parsing, token fetch, retry) is now shared verbatim by both
pull and push call sites. `pull_manifest_at`/`pull_blob` were updated
to build their own closures; their own observable behavior (and every
pre-existing test) is unchanged.

## The real push protocol, checked step by step against a real local registry, not assumed

Every piece verified directly against a real, local `docker.io/
library/registry:2` instance (`docker run -d -p 15000:5000
registry:2`), not assumed from the OCI Distribution Spec text alone:

* **`blob_exists`**: `HEAD .../blobs/<digest>` — `200` (already there,
  skip re-uploading — the same real cross-push deduplication a real
  `docker push`/`podman push` also relies on) or `404` (needs
  uploading).
* **`upload_blob`**: `POST .../blobs/uploads/` (`202 Accepted`, a real
  `Location` header naming the actual upload URL — confirmed the real
  registry sends an absolute path here, `resolve_location` also
  handles a full URL for registries that send one instead), then `PUT
  <location>?digest=<digest>` with the real blob bytes — streamed from
  a real, already-open `File` (never fully read into memory first,
  matching `oci_store::Store::open_blob`'s own established convention
  for potentially-large layer content, unlike the smaller manifest/
  config path). A `RefCell`-wrapped file lets the same `Fn` closure
  re-seek to the start if `request_with_auth` needs a second attempt
  after a `401` (rare in practice — the token was just validated
  moments earlier by the `POST` — but real and worth being correct
  about, not assumed away).
* **`push_manifest`**: `PUT .../manifests/<ref>` with the real,
  already-stored manifest bytes (via `oci_store::Store::read_blob`,
  never re-serialized — a re-serialization could produce different
  bytes for the same logical content, a real, if subtle, way to end up
  with a manifest whose own digest doesn't match what `ociman`'s own
  local record still believes it pushed) and the correct `Content-
  Type` (real registries reject a manifest `PUT` with the wrong or
  missing one, confirmed directly).

## Manually verified end to end, including a full round trip through an entirely independent tool

Before writing any automated test: pulled `docker.io/library/busybox:
latest` into a real `ociman` store, tagged it at
`localhost:15000/test/busybox:latest`, pushed it via a small scratch
program exercising the exact same `oci_registry::push_image` function
`cmd_push` calls — then, critically, pulled it back with a completely
independent tool (`docker pull localhost:15000/test/busybox:latest`),
confirming the digest matched byte-for-byte
(`sha256:8f2ffdcb...`), that a second push correctly skipped both
already-uploaded blobs, and — for a real `ociman build`-produced image
(`FROM busybox` + a real `RUN` layer, not just a straight re-push of a
pulled image) — that `docker run` against the pushed-then-pulled-back
image actually produces the real, correct file content. Real, working,
end-to-end, cross-tool interoperability, not just "the mock test
passed."

## `ociman push`, narrower than real `podman push`

Real `podman push IMAGE [DESTINATION]` supports an optional, separate
destination (including alternate transports like `oci-archive:`/
`dir:`). This increment supports only the common, single-argument
form: `ociman push <reference>` resolves `reference` locally (by tag
*or* image ID, reusing `resolve_image_by_reference_or_id`, 0122) and
pushes it back to its own already-tagged registry/repository/tag —
matching the most common real workflow (`ociman build -t registry.
example.com/app:v1 . && ociman push registry.example.com/app:v1`)
without the added complexity of alternate transports or an explicit,
possibly-different destination argument.

## Real, automated tests

Two new mock-registry tests in `crates/oci-registry/src/push.rs`
(a hand-rolled HTTP/1.1 mock implementing `HEAD`/`POST`/`PUT`,
verifying an uploaded blob's body really hashes to the digest the
`PUT` claimed — the same real check a real registry performs, not
just "did the mock not crash"): every missing blob plus the manifest
get uploaded with the exact right content; a blob the registry already
has is never re-uploaded. Two new `ociman_push` CLI integration tests
(no network needed: pushing an unknown reference or image ID is a
clear, immediate error, before any real network attempt). All 20
pre-existing `oci-registry` tests still pass unmodified (the
`request_with_auth` refactor changed no observable pull behavior).
Full `cargo build --workspace --locked`/`cargo test --workspace
--locked` (2 clean runs)/`cargo fmt --all --check`/`cargo clippy
--workspace --all-targets --locked -- -D warnings` all clean.

## What this doesn't do yet

* **No insecure (plain HTTP) registry support in the `ociman` CLI
  itself** — `oci_registry::Client::with_options`'s own
  `insecure_hosts` parameter already exists and is exactly what a
  local/private HTTP-only registry needs (used directly for this
  increment's own manual verification against a real local
  `registry:2`), but nothing in `ociman`'s own CLI wires a `--tls-
  verify`-equivalent flag to it yet — `Client::new()` always assumes
  HTTPS. A real, pre-existing gap (equally affects `ociman pull`
  today, not new to this increment), left for its own focused future
  increment rather than folded into this one.
* No `DESTINATION` argument/alternate transports (see above).
* No chunked/resumable uploads — every blob upload is one real,
  monolithic `PUT`, matching what every real registry this project
  has actually tested against (`registry:2`, and transitively
  Docker Hub/quay.io/GHCR via the existing pull path) already
  supports; a genuinely enormous single layer could be real, future
  work if it ever becomes a real, measured problem.
