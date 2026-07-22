# Design note 0178: `ADD --checksum=<algo>:<hex>`

Status: implemented
Scope: `crates/oci-dockerfile/src/instruction.rs` (`AddFlags::checksum`,
`parse_add`); `crates/oci-dockerfile/src/expand_stage.rs` (`$VAR`
expansion for the new field); `bin/ociman/src/build.rs`
(`add_instruction`'s new structural validation and download-time
verification); `tests/tests/ociman_build.rs`.

## Closing a real, explicitly-named deferred gap

`ADD --checksum=` used to be listed, alongside every other BuildKit-
only flag this project doesn't parse at all, in both `instruction.rs`'s
and `lib.rs`'s own "deliberately not handled yet" doc comments. This
increment closes it — narrowly, matching this project's own already-
established pattern of picking one well-scoped BuildKit flag at a time
(0134-0136, 0140-0141) rather than a large batch.

## Checked directly: real BuildKit's and buildah's own restrictions,
not guessed

Before writing any code, `~/git/moby/vendor/github.com/moby/buildkit/
dockerfile2llb/convert_copy.go`'s own `dispatchCopy` and `~/git/podman/
vendor/go.podman.io/buildah/add.go`'s own `Add()`/`getURL()` were read
directly. Three real, structural restrictions came out of that reading,
all matched here exactly:

1. `--checksum` is `ADD`-only, never `COPY` (moot for this crate:
   `CopyFlags` never gained the field at all, so `COPY --checksum=`
   keeps failing with the pre-existing generic "unsupported flag"
   error — already close enough in spirit to BuildKit's own dedicated
   "checksum can't be specified for COPY" message).
2. Legal only with **exactly one** source on the instruction — an
   error, not silently applied to just the first one, if there's more
   than one (checked structurally, before any network access at all,
   matching BuildKit's own fail-fast ordering).
3. Legal only when that one source is a remote URL — a hard error for
   a local build-context source (buildah's own `getURL` is only ever
   reached for a URL/git source in the first place; a local source hits
   a dedicated `"checksum flag is not supported for local sources"`
   check instead).

Buildah's own `getURL` tees the digester over the raw downloaded body
before any archive-detection/extraction happens, and aborts the whole
call on a mismatch — no partial content ever reaches the destination,
no layer ever gets committed. Matched here identically: `add_instruction`
verifies the digest against `downloaded.bytes` immediately after
`oci_dockerfile::download` returns, strictly before any
`create_dir_all`/`write_new_file`, so a mismatch surfaces as a plain
`anyhow::Result::Err` propagating straight out of `add_instruction`
with nothing written and no `commit_layer` ever reached.

## `sha256` only — checked against real Docker's own public
documentation, not just the vendored source

`oci_spec_types::digest::Digest::parse` already structurally accepts
`sha512:...` (needed elsewhere for registry interoperability — some
registries/tools emit `sha512` digests), but that same module's own doc
comment says outright: "oci-tools never *produces*" one — there is no
`Sha512Writer`/`sha512()` hashing helper in this crate at all. Rather
than add one purely to unblock this one flag (a real, if small,
departure from that module's own established scope), `add_instruction`
explicitly restricts `--checksum` to `sha256`, matching real Docker's
own public documentation ("currently only sha256 is supported")
exactly. A `--checksum=sha512:...` (or anything else `Digest::parse`
would otherwise accept) is a clear, immediate
`"only sha256 is supported"` error instead of a silently-unenforceable
checksum.

## Cache interaction: none, by construction

`add_instruction`'s own cache lookup was already unconditionally
skipped whenever any URL source is present (`created_by`/
`content_digest` only ever get computed for local-source-only `ADD`s,
predating this change entirely — fetching a remote source just to hash
it for a cache key would defeat the point of a cache hit). Since
`--checksum` is only ever legal alongside exactly one URL source, it
never interacts with the build cache at all; no changes were needed
anywhere in `build_cache.rs`.

## Tests

`crates/oci-dockerfile`: 2 new parser unit tests (`--checksum` alone
and combined with `--chown`/`--chmod`) plus 1 new `expand_stage` test
(`$VAR` substitution inside a `--checksum` value, matching `--chown`'s
own already-tested identical treatment). `tests/tests/ociman_build.rs`
gained 6 integration tests, each against a real local HTTP mock
(`serve_one_response`, the same pattern every other `ADD`-from-URL test
in this file already uses) and a real running container where
relevant: a matching checksum succeeds and the downloaded content is
correct; a mismatching checksum is a hard error with no image tagged;
`--checksum` combined with a local source, or with more than one
source, is a clear structural error raised before any network access;
malformed checksum syntax fails fast; and `--checksum=sha512:...` (a
structurally valid digest this crate simply doesn't hash) is a clear,
immediate "only sha256 is supported" error. Full `cargo build
--workspace --locked`/`cargo test --workspace --locked` (2 clean
runs)/`cargo fmt --all --check`/`cargo clippy --workspace --all-
targets --locked -- -D warnings`/`python3 ci/guards.py`/`cargo deny
check`/`bash ci/native-ci.sh` all clean.

## What this doesn't do yet

* `sha512`/`sha384` verification — a real, deliberate narrowing (see
  above), matching real Docker's own public documentation rather than
  real BuildKit's own slightly more permissive vendored `digest.Parse`
  call.
* Git sources (`ADD --checksum=... some.git#branch`) — this project
  has no git-source `ADD` support at all yet, unrelated to this
  increment.
* `COPY --checksum=` — never a real flag on `COPY` in the first place
  (see above).
