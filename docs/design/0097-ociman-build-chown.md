# Design note 0097: `COPY`/`ADD --chown` (milestone 4)

Status: implemented
Scope: `bin/ociman/src/build.rs` (`copy_instruction`/`add_instruction`
resolve and apply `--chown`, `copy_path_recursive`'s new `chown`
parameter, new `set_owner`/`chown_recursive`), `tests/tests/
ociman_build.rs` (new tests, one existing test updated).

`ociman build` gained `COPY --chown=<user>[:<group>]`/`ADD
--chown=...`, the last of the two rejected `COPY`/`ADD` flags
(`--chmod` landed in 0079). Originally surveyed as "medium" — the
initial plan (override the committed layer's own tar header at
`commit_layer` time) would have needed new plumbing threaded through
three function signatures — but a simpler design turned out to need
none of that at all.

## The simpler design: a real `chown(2)`/`lchown(2)`, not a tar-header override

`oci_layer::export`'s own `write_entry` (the code that actually writes
each committed layer's tar headers) builds every header from the
*real, on-disk* file's own live metadata (`fs::symlink_metadata`/
`fs::metadata`) at commit time — not from any separately-tracked
value. This means a real `chown`/`lchown` applied directly to the
copied file, mirroring exactly how `--chmod` already applies a real
`chmod(2)`, is automatically and correctly reflected in the committed
layer's own tar header with **zero changes** to `commit_layer`/
`oci_layer::export`/`write_entry` at all — confirmed directly: verified
against a real, freshly-committed layer's own tar bytes (`tar tvzf`)
showing the exact requested uid/gid, no new data plumbing through the
diff/commit path needed.

## `lchown`, not `chown`, and why that specifically matters here

`--chown` is applied via `fchownat(..., AT_SYMLINK_NOFOLLOW)` (an
`lchown(2)` equivalent, via `rustix::fs::chownat`) rather than an
ordinary symlink-following `chown(2)` — deliberately, and for a real
reason specific to this project's own established design: real
Docker/BuildKit's own `COPY`/`ADD` *dereferences* a symlink source
outright (copying the target file's own content under the destination
name), but this project's `copy_path_recursive` already made a
different, established choice: preserve a copied symlink as a real
symlink (0048's own doc comment). Given that choice, an ordinary
`chown` (which follows the link) would silently `chown` whatever
arbitrary file the link happens to point at instead of the symlink
entry `COPY` actually created — including, for an absolute or
`..`-escaping link target, a file entirely outside the copy's own
scope. `lchown` avoids that unconditionally.

## The rootless limitation is real, but already established elsewhere — not a new one

Changing a file's owner to an arbitrary `uid`/`gid` that isn't the
calling process's own needs real `CAP_CHOWN`. `set_owner` attempts the
real syscall and tolerates `EPERM` (logged at `warn`, never fails the
build) — the exact same "tolerate known rootless limitations" pattern
this project already applies to `-v`/`--volume`'s own bind-mount
ownership and `oci_layer::apply`'s own extraction-time ownership (both
already documented). A rootless build (the common case) therefore
still succeeds and still produces a byte-correct, portable committed
layer whenever the requested `uid` happens to match the calling
process's own (the common real-world case: building as the same user
the eventual container is meant to run as) or whenever the build
itself runs as real root (where any `chown` succeeds unconditionally).

## A real distinction from `--chmod`, found by testing against real Docker, not assumed

`--chmod` is deliberately *not* applied to `ADD`'s auto-extracted
archive contents (0079: flattening a real archive's own varied,
per-entry permissions would be destructive). Naturally assuming
`--chown` worked the same way turned out to be **wrong** — checked
directly against a real Docker daemon on this host before writing any
code: `ADD --chown=2000:2000 some.tar.gz /dest` genuinely **does**
override the archive's own recorded per-entry ownership throughout
`/dest`, overriding it even when the archive itself already recorded
some other real uid/gid. `add_instruction`'s own archive-extraction
branch therefore walks the just-extracted tree afterward
(`chown_recursive`) and applies `--chown` there too, unlike `--chmod`.

## Real, manual verification against a real, freshly-pulled busybox

Built the release binary and exercised every real scenario before
writing any automated test: `COPY --chown=<own-uid>:<own-uid>`
correctly recorded in the committed layer's own tar header (`tar
tvzf`, directly on the blob); an arbitrary different uid correctly
tolerated with a logged warning, build still succeeding; `ADD --chown`
on a local archive source correctly overriding the archive's own
per-entry ownership throughout the extracted tree; `COPY --chown` on a
symlink source correctly using `lchown` semantics (the symlink entry
itself, not its target, ends up owned by the requested uid/gid in the
committed layer).

## Real, automated tests

Three new tests in `tests/tests/ociman_build.rs`, using a new
`last_layer_tar_entries` helper (reads a committed layer's own real
gzip-compressed tar bytes back via the `Store` API, since running the
*built* container itself would never show `--chown`'s effect — that's
`oci_layer::apply`'s own already-documented, unrelated extraction-time
limitation): `COPY --chown` reflected correctly in the committed
layer, using the calling test process's own real uid/gid (the only
value guaranteed to succeed regardless of whether the test suite
happens to run rootless or as real root); an unprivileged `--chown` to
a genuinely different uid tolerated, not fatal (skipped outright when
running as real root, where it would simply succeed); and `ADD
--chown` correctly overriding a local archive's own auto-extracted
contents. One pre-existing test
(`copy_rejects_unsupported_flags_and_bad_glob_patterns`) updated to
remove its now-stale `--chown`-is-rejected case.

## Not a hot-path change — build-only `COPY`/`ADD` handling is explicitly exempt

Confirmed by this project's own established rule: "build-only changes
to `bin/ociman/src/build.rs`'s COPY/ADD instruction handling do not
require" A/B perf re-verification — `run_step_spec` (the one
`build.rs` function that *is* named) is completely untouched.

## What's still not here

* `ociman run -d`/`--detach`, `ocirun update`/`pause`/`resume`, the
  build cache, `ONBUILD`/`HEALTHCHECK`, a symbolic `--chmod` mode (a
  genuinely large BSD `setmode(3)`-style grammar, re-assessed this
  session as much bigger than its own earlier "tiny" estimate — real
  BuildKit vendors a 548-line library for it) — all still exactly as
  earlier increments left them, unrelated to this increment's own
  scope.
