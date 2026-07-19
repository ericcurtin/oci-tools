# Design note 0075: `ociman build` `ADD` remote URL sources (milestone 4)

Status: implemented
Scope: new `crates/oci-dockerfile/src/download.rs` (+ `ureq` dependency
on `oci-dockerfile`), `bin/ociman/src/build.rs` (`add_instruction`,
new `is_remote_url`/`write_new_file` helpers), `tests/tests/
ociman_build.rs`.

`ADD`'s own scope list (`add_instruction`'s own doc comment, since
0068) has carried exactly one remaining item since local sources,
archive auto-extraction, multi-source, and glob patterns were all
closed out (0068/0072/0073): a remote URL source (`http://`/
`https://`), previously rejected outright with a clear error. This
increment closes it.

## Checked directly against real BuildKit

`~/git/moby/daemon/builder/dockerfile/copy.go`'s own `downloadSource`/
`getFilenameForDownload` is the exact, real implementation `docker
build`/`podman build` both still rely on for this. Three things
carried over deliberately, checked directly against that source:

* **Never decompressed**, even when the body is a real archive —
  `noDecompress = true // data from http shouldn't be extracted even
  on ADD`, explicitly distinguishing this source kind from a *local*
  archive `ADD` source (which this project's own `oci_layer::
  detect_archive`, 0068, does auto-extract).
* **File name determination priority**: the URL's own path's final
  segment first (unless the path is empty or ends in `/`), then the
  response's own `Content-Disposition` header's `filename=` parameter,
  `None` if neither gives one — in which case a directory-like
  destination (ending `/`, or already existing as a directory) is a
  real, clear error (`"cannot determine a file name for source"`,
  this project's own wording for real BuildKit's own `"cannot
  determine filename for source"`). An explicit, non-`/`-ending
  destination never needs a derived name at all — same as any other
  `ADD`/`COPY` single-file destination.
* **File mode `0o600`** — real BuildKit's own `os.OpenFile(tmpFileName,
  os.O_RDWR|os.O_CREATE|os.O_EXCL, 0o600)` for exactly this source
  kind, unlike a locally-copied file (which keeps its own original
  permission bits via `copy_path_recursive`/`std::fs::copy`, this
  project's own already-documented stance). Verified this actually
  lands as `-rw-------` in a real built image during manual testing
  below.

Two deliberate simplifications, called out directly in `download.rs`'s
own module doc comment and `add_instruction`'s own doc comment rather
than silently diverging:

* `Content-Disposition` parsing only recognizes the plain, common
  `filename=...` form (quoted or not) — not the full RFC 6266/2183
  grammar real Go's `mime.ParseMediaType` handles (escaped quotes, the
  extended `filename*=UTF-8''...` form). This is only ever a
  *fallback*, reached solely when the URL's own path gives no usable
  name at all.
* The downloaded file's mtime is never set from the response's own
  `Last-Modified` header (real BuildKit does, via `system.Chtimes`) —
  a cosmetic difference with no effect on the built image's own
  content or correctness, left at "the time the file was written"
  instead.
* Response bytes are read in full into memory (`ureq`'s own
  `.read_to_vec()`, bounded at 512 MiB — see below), rather than
  streamed straight to a temp file the way real BuildKit does. Typical
  `ADD <url>` use (a config file, a small script) is nowhere near this
  bound; a real streaming-to-disk implementation is a reasonable, if
  unlikely-to-matter-in-practice, future refinement.

## New capability: an HTTP client for `oci-dockerfile`

`ci/guards.py`'s "one crate per capability" HTTP-client group already
names `ureq` as this project's one sanctioned choice (only
`oci-registry` depended on it before this increment, for the registry
protocol) — the guard only forbids *competing* HTTP client crates
being present simultaneously, not which/how-many workspace crates
depend on the sanctioned one. Added `ureq` as an `oci-dockerfile`
dependency directly (confirmed clean via `ci/guards.py` after the
change) rather than routing through `oci-registry` (a registry-
protocol-specific client, not a general-purpose one — reusing it here
would be a layering violation, not a simplification).

`agent.get(url).call()`'s API usage mirrors `oci-registry::client`'s
own conventions exactly (`Agent::config_builder()...timeout_global(...)
.http_status_as_error(false)` — required so a real 4xx/5xx response
comes back as an `Ok` response to inspect rather than an opaque `Err`,
confirmed the hard way when a first draft of the new test for a 404
response panicked instead of matching `DownloadError::Status`;
`resp.headers().get(...)`, `resp.body_mut().with_config().limit(N)
.read_to_vec()`, the same `MAX_MANIFEST_BYTES`-style defensive size
bound `oci-registry::client` already established for exactly this
"don't let a hostile/misbehaving server exhaust memory" concern, no
real docker-documented limit exists for this — this project's own
choice).

## Wiring into `add_instruction`

A URL source never enters `resolve_sources`'s own glob-expansion path
— `contains_wildcards` would otherwise misfire on a URL's own
`?query=string` (a `?` is a real single-character wildcard in the
glob syntax `resolve_sources` uses for everything else). Sources are
now split into local and URL sources first; only the local ones go
through the existing glob-expansion/context-relative resolution
unchanged, and the two counts are recombined only for the existing
"more than one source needs a trailing `/` destination" rule (real
BuildKit's own rule, checked against the *total* expanded count either
way).

## Real, manual end-to-end verification before writing a single automated test

Ran a real local Python `http.server`, wrote a real Containerfile
(`ADD http://127.0.0.1:PORT/hello.txt /app/downloaded.txt`) against a
real, freshly-pulled `docker.io/library/busybox:latest`, built it with
the real release binary, then `ociman run --rm ... -- /bin/cat
/app/downloaded.txt` — the real downloaded content came back correctly,
and `/bin/ls -la` on the same file inside the container confirmed
`-rw-------` (mode `0o600`), exactly matching real BuildKit's own
documented behavior for this source kind.

## Real, automated tests

`oci-dockerfile::download`'s own unit tests spin up a tiny,
single-response HTTP/1.1 mock over a real loopback `TcpListener` — the
same established pattern `oci-registry::client`'s own test module
already uses for a real server rather than a fake transport — covering
a path-derived filename, a `Content-Disposition`-derived fallback, and
a real HTTP error status. `tests/tests/ociman_build.rs` adds the same
mock-server pattern for a full `ociman build` → `ociman run`
round trip: an explicit-destination download, a directory-destination
download deriving its filename from the URL path, and a directory
destination with no derivable name at all failing with the documented
error — replacing the old `add_rejects_a_remote_url_source` test,
which no longer describes real behavior.

## Performance

Touches only `bin/ociman/src/build.rs`'s own `ADD` instruction handling
and a new, self-contained `oci-dockerfile` module — not
`oci-runtime-core`, `main.rs`'s `synthesize_spec`/`resources_from_cli`,
or either cgroup driver (confirmed via `git diff --stat`), and none of
this is on the `ociman run`/`ocirun run` startup/destroy hot path this
project's own benchmarks measure. No benchmark re-verification needed,
consistent with every prior build-only increment this session.

## What's still not here

* The build cache — still nothing actually caches a previous build's
  own result yet, unchanged by this increment.
* `ONBUILD`/`HEALTHCHECK`, `--target`, anonymous/untagged build mode —
  unchanged, tracked on `cmd_build`'s own module doc comment.
* Streaming a downloaded `ADD` source straight to disk instead of
  buffering it in memory first (see "deliberate simplifications"
  above) — a possible future refinement if a real use case needs
  downloads larger than the 512 MiB bound.
