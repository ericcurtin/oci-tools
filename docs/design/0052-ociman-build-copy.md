# Design note 0052: `ociman build`'s `COPY` instruction (milestone 4)

Status: implemented (single-source, no glob/`--from`/`--chown`/
`--chmod` yet — see "Scope"; `ADD` still entirely unimplemented)
Scope: `bin/ociman/src/build.rs`, `tests/tests/ociman_build.rs`.

0051's own "what's still not here" named this exact next piece:
*"`COPY`/`ADD` (real build-context file access — the next natural
increment, needing no container execution at all, only tar/copy
logic)."* This increment ships `COPY` (`ADD` still isn't — see
"Scope").

## The pipeline: same commit path as `RUN`, a plain copy instead of a command

`COPY` reuses the exact same "materialize a scratch rootfs, snapshot,
mutate, diff, `commit_layer`/`record_layer`" shape 0051 built for
`RUN` — the *only* difference is what happens between the snapshot and
the diff: a plain recursive file copy (`copy_path_recursive`) instead
of `oci_runtime_core::launch::run`. `needs_rootfs` (the check that
decides whether to pay for a tempdir + base-layer extraction at all)
now also fires on `Instruction::Copy`, not just `Instruction::Run`.

## Scope: what's supported, what's rejected, and why

**Supported:** a single source (file or directory) from the build
context, copied to a destination resolved against the working
directory currently in effect (reusing `resolve_workdir`'s own join-
then-normalize logic — an in-container path is an in-container path,
whether it's a process's `cwd` or a `COPY` destination), matching real
Docker/BuildKit's own destination-inference rules exactly:
* A directory source's own **contents** land inside `dest` (the
  directory itself is never renamed into `dest/<name>`).
* A file source is renamed to `dest` outright, *unless* `dest` is
  written with a trailing `/` or already exists as a directory in the
  rootfs, in which case it's copied into `dest` under its own
  basename instead (matching `cp`'s own long-standing convention,
  which real Docker deliberately mirrors).

**Rejected, with a clear error** (matching the same "reject rather than
silently misbehave" convention `RUN`/multi-stage/`FROM scratch` already
established):
* More than one source at once, and glob patterns in a source (both
  add real matching/ordering complexity better scoped as their own
  later increment than rushed into this one).
* `--from=<stage-or-image>` — only single-stage builds exist so far
  anyway (0050); the "copy from another build stage" case has nothing
  to resolve against yet.
* `--chown`/`--chmod` — this project's own rootless single-uid-mapping
  design (only container uid 0 is ever mapped) and `oci_layer::apply`'s
  own already-documented "doesn't chown" scope limit apply equally
  here; `copy_path_recursive` only ever preserves a source file's own
  permission bits (via `std::fs::copy`'s own documented behavior), the
  same read/write-side consistency `oci_layer` itself already
  maintains.
* A source path that doesn't exist in the context, or that tries to
  escape it with a `..` component.

**`ADD` is not implemented at all yet** (still a hard parse-time-
adjacent rejection) — it needs everything `COPY` does *plus* remote-URL
fetching and local-archive auto-extraction, neither of which exists.

## A source (or destination) can't escape its own root — checked directly, not assumed

`safe_join(base, relative)` rejects any `..` component outright (not
"try to clamp it," matching `oci_layer::apply`'s own existing
`safe_join` precedent for the identical concern on the *extraction*
side) — used for both directions: a `COPY` source can't read outside
the build context, and a destination can't write outside the rootfs.
A leading `/` in either a source or a destination is treated as
context-rooted/rootfs-rooted, **not** host-absolute — real, checked-
directly Docker/BuildKit behavior (`COPY /foo /bar` copies
`<context>/foo`, never a host-absolute `/foo`), not a guess: a naive
implementation that let a leading `/` reach `Path::join` unmodified
would have silently replaced the intended base entirely (`PathBuf::
join` discards everything before an absolute operand) — this was
caught and fixed during manual verification, not left to chance.

## Real, manual end-to-end verification before writing automated tests

Built the release binary and ran a real multi-`COPY` build against a
real `docker.io/library/busybox:latest` pull: a plain file copy to an
absolute destination, a directory copy to a not-yet-existing
destination (confirming contents-not-directory semantics), and a
relative destination after `WORKDIR` — then actually `ociman run` the
built image and `cat`'d every file back out to confirm real content
survived the whole diff/export/compress/ingest round trip. Separately
verified the "copy into an already-existing directory, no trailing
slash" case, and every rejection path (`..` escape, multiple sources,
a glob, `--chown`, a missing source) by hand before encoding any of
them as an automated test.

## Real, automated tests

5 new tests in `tests/tests/ociman_build.rs`: the full multi-`COPY`
scenario above (file, directory-into-new-destination, `WORKDIR`-
relative destination — asserting three new layers on top of the base
image's own, then actually running the built image to confirm real
file content); copying into an already-existing directory keeping the
source's own basename; a context-escaping `..` source; a missing
source (with the same "no partial image tagged" check every other
rejection path in this file already uses); and a table-driven test
covering `--chown`/`--chmod`/`--from`/multiple-sources/a glob in one
pass.

## Performance

`COPY` only materializes a rootfs when a stage actually contains one
(same lazy-`needs_rootfs` reasoning 0051 already established for
`RUN`) — a metadata-only build's own cost is unchanged. This increment
touches only `bin/ociman/src/build.rs` and its own test file —
`oci-runtime-core`/`ocirun`/`ociman run`'s own hot paths are untouched
(confirmed via `git diff --stat` before finishing), so no benchmark
re-verification was needed this time (unlike 0051, which did touch
shared runtime code).

## What's still not here

* `ADD` (remote URLs, local-archive auto-extraction).
* Multiple `COPY` sources, glob patterns, `--from`, `--chown`,
  `--chmod`.
* Multi-stage builds, `--build-arg`, the build cache, `ONBUILD`/
  `HEALTHCHECK`, an anonymous/untagged build mode — all still exactly
  as 0050/0051 left them.
