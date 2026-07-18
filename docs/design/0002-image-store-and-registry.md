# Design note 0002: image spec types, store, and registry client (milestone 2)

Status: implemented
Scope: `oci-spec-types`, `oci-store`, `oci-registry`; `ociman pull/images/inspect`.

## Goals

Milestone 2 gives `ociman` a working, read-only image lifecycle: parse a
reference the way podman/docker do, pull it (including multi-platform
indexes) from any OCI distribution-spec registry, land it in
content-addressed local storage, and read it back for listing/inspection.
Everything here is shared, unprivileged, rootless-friendly library code —
no container execution yet (that is milestone 3).

## `oci-spec-types`

Pure data, no I/O, per the crate's original charter.

* `digest`: `Digest`/`Algorithm` (`sha256` write path, `sha256`/`sha512`
  parse), a streaming `Sha256Writer` (`impl io::Write`) so hashing never
  requires buffering full content, and a `sha256(bytes)` convenience for
  small in-memory documents (manifests, configs).
* `image`: `Descriptor`, `Platform`, `ImageManifest`, `ImageIndex`,
  `ImageConfig`/`ContainerConfig`/`RootFs`/`HistoryEntry`, and the
  `MEDIA_TYPE_*` constants (OCI and legacy Docker v2). `Manifest::parse`
  disambiguates a manifest vs. an index from the registry's `Content-Type`
  header, falling back to sniffing the `manifests` field when the header is
  missing or generic (some registries reply `application/json`).
* `reference`: `Reference::parse` normalizes the way
  `github.com/distribution/reference` does — implicit `docker.io`/`library/`,
  the `index.docker.io` legacy alias, digest-wins-over-tag when both are
  given, `localhost`/dotted/ported hosts treated as explicit registries. Not
  the full upstream grammar (nested IPv6-literal hosts etc. are unhandled),
  just what registries and users actually produce.

## `oci-store`

Content-addressed blobs (`blobs/sha256/<hex>`) plus JSON pointer files
(`images/<sha256(reference)>.json`) mapping a reference string to the
manifest digest it currently resolves to.

* **Atomic, deduplicated ingest**: stream into a `NamedTempFile` in the same
  directory as the final path while hashing, then rename into place; if the
  digest already exists on disk the temp file is simply dropped. A crash
  mid-download never leaves a partial blob at its content-addressed path.
  `ingest_verified` additionally rejects (and discards) content that
  doesn't hash to a digest the caller already committed to trusting (a
  manifest descriptor) — the registry client always uses this path, never
  the unchecked `ingest`, for anything it downloads.
* **GC is mark-and-sweep, not incremental ref-counting**: every stored
  pointer's manifest is parsed and walked (following indexes to every
  child, recursively) to build the reachable blob set; anything in
  `blobs/sha256/` not in that set is deleted. Equivalent to ref-counting in
  outcome (a blob survives iff something live still reaches it) but immune
  to counter-drift from a crash between an increment and its matching
  decrement — there is no counter to drift.
* `describe.rs` reads a stored pointer's manifest/config back out for
  `ociman images`/`inspect`; oci-tools only ever stores the
  platform-resolved manifest under a pointer (never a raw index), so this
  is where `StoreError::UnexpectedIndex` would fire if that invariant were
  ever violated.

## `oci-registry`

Pull-only for now (push arrives with `ociman build`/`push`, milestone 4+).

* **HTTP client**: `ureq` (blocking; no async runtime to start up, which
  matters for a CLI that runs and exits). `http_status_as_error(false)` so
  4xx/5xx surface as normal responses the client inspects itself rather
  than as `ureq::Error`.
* **Auth**: the standard Docker/OAuth2 bearer-token dance — an
  unauthenticated request 401s with `WWW-Authenticate: Bearer
  realm=...,service=...,scope=...`; the client GETs the realm (with HTTP
  Basic credentials if configured for that host) and retries with
  `Authorization: Bearer <token>`. Tokens are cached per
  `(registry_host, scope)` for the client's lifetime. Credentials come from
  the standard podman/docker auth file locations
  (`$REGISTRY_AUTH_FILE`, `$XDG_RUNTIME_DIR/containers/auth.json`,
  `~/.config/containers/auth.json`, `~/.docker/config.json`); the `auth`
  field is already `base64(user:pass)` so it is forwarded verbatim as the
  `Authorization: Basic` header value, no decode/re-encode round trip.
* **Manifests are hashed locally, never re-serialized**: `pull_manifest`
  returns the registry's exact bytes plus a digest computed from them (and
  cross-checked against `Docker-Content-Digest` when the registry sends
  one), so what lands in the store is guaranteed byte-identical to what the
  registry actually returned.
* **Blobs stream**: `pull_blob` returns a `BlobReader: Read` wrapping
  ureq's body reader; callers pipe it straight into
  `Store::ingest_verified` without buffering full layers in memory.
* **`pull::pull`** is the one shared orchestration function (`ociman pull`
  today; `ocicri`'s ImageService and `ociboot upgrade`/`switch` reuse it
  later): fetch the top-level manifest/index, select the running
  platform's manifest out of an index if necessary (one more registry
  round trip, addressed by the selected child's own digest), skip blobs
  the store already has, and record the pointer.
* **Insecure registries**: `Client::with_options` accepts a set of
  `host[:port]` values to speak plain HTTP to instead of HTTPS — the same
  escape hatch every other engine has (`--tls-verify=false` /
  `insecure-registries`), off by default, and incidentally what makes the
  registry-client test suite possible without standing up TLS.

## `ociman pull`/`images`/`inspect`

Thin per the workspace's rules: parse the reference, open a `Store` rooted
at `oci_cli_common::storage::default_root()` (`$OCI_TOOLS_STORAGE_ROOT`,
else `/var/lib/oci-tools/storage` as root, else
`$XDG_DATA_HOME/oci-tools/storage` rootless), construct an
`oci_registry::Client`, and call into the shared crates. `--json` mode
serializes a small `ImageView`/`ImageConfig` rather than any internal
storage type directly.

## Decisions and risks

* **No push, no mirrors/retries yet.** Scoped out to keep the milestone
  reviewable; `oci-registry`'s doc comments already flag them as planned.
* **Single-platform storage only.** A pulled multi-platform index is
  resolved to one manifest and only that manifest is kept; re-pulling the
  same reference for a different platform (e.g. cross-building) would
  currently overwrite the pointer. Fine for milestone 2's scope
  (`ociman` on the host's own platform); revisit if/when cross-platform
  pulls become a real use case.
* **Manual HTTP/1.1 mock servers in tests, not a mock-server crate.** The
  registry test suite (`oci-registry`'s `client`/`pull` unit tests) hand-
  rolls a few dozen lines of `TcpListener` plus line-based request parsing
  rather than adding a dependency for it. It is deliberately minimal (exact-
  path routing, no persistent connections) and exercises the real bearer-
  token challenge/retry/cache code path end to end, including an actual
  `pull()` run against two mock registries to prove local blob dedup works
  without a second network fetch.
