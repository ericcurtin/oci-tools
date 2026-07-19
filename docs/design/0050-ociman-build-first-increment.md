# Design note 0050: `ociman build`, first working increment (milestone 4)

Status: implemented (single-stage, metadata-only builds; see "Scope"
below for exactly what's rejected and why)
Scope: `bin/ociman/src/build.rs`, `bin/ociman/src/main.rs` (CLI wiring),
`tests/tests/ociman_build.rs`.

0039 through 0049 built one piece of a Dockerfile build pipeline at a
time — parser, shell expansion, stage grouping, dependency resolution,
rootfs diffing, layer export/compression, and the `commit_layer`/
`record_layer` store-recording glue — and every one of those eleven
design notes ended with some form of the same sentence: *"not yet
wired into anything ... no `ociman build` command exists yet."* This
increment is that wiring: `ociman build` is now a real, working command
for the first time.

## Why this increment exists now, not another isolated primitive

By 0049, five consecutive increments (0045-0049) had each shipped a
well-tested, well-scoped library primitive with *zero* runtime callers
— a real, deliberate pattern (each one individually the smallest safe
step), but one that risked never actually delivering milestone 4's own
headline feature if continued indefinitely. This increment breaks that
pattern: rather than adding a sixth unwired primitive, it wires
everything built so far into an actual CLI command a user can run,
even though (see "Scope") it only covers a real subset of Dockerfiles
so far. A working, honestly-scoped command today is worth more than
an eventually-complete one that never ships.

## Scope: what's supported, what's rejected, and why

**Supported** (applied to a working copy of the `FROM` base image's own
config, one real `HistoryEntry` per instruction, `empty_layer: true`
since none of these produces a layer):
`ENV` (merge-by-key, matching real Docker: an existing key is updated
in place, not duplicated or reordered), `LABEL`, `WORKDIR` (relative
paths resolved against the previously-set working directory and
normalized, matching real `dispatchWorkdir`), `USER`, `ENTRYPOINT`/
`CMD` (shell form wrapped as `/bin/sh -c "..."`, exec form used
verbatim — real Docker's own convention), `EXPOSE`, `VOLUME`,
`STOPSIGNAL`, `MAINTAINER` (sets the config's own `author` field,
distinct from `ContainerConfig`). `ARG`/`SHELL` are accepted as no-ops
(already fully resolved by `expand_stage`/`expand_meta_args`; `SHELL`
only affects a future shell-form `RUN`, not supported yet either).

**Rejected, with a clear error, matching this project's own
established convention** (e.g. `ONBUILD`/`HEALTHCHECK` at parse time,
0039) **rather than silently skipped or misexecuted:**
* `RUN`/`COPY`/`ADD` — each needs real machinery this increment
  doesn't set up (`RUN`: a container-namespace execution loop via
  `oci_runtime_core`, diffed via `oci_layer::changes` and committed via
  `oci_dockerfile::commit_layer`; `COPY`/`ADD`: real build-context file
  access). Since nothing can produce a new layer yet, a built image's
  own layer list is always byte-identical to its base image's — proven
  directly by a dedicated test, not just assumed.
* **Multi-stage Dockerfiles** (more than one `FROM`) — `oci-dockerfile`
  already computes the dependency-ordered build plan a multi-stage
  build needs (`resolve_dependencies`/`stages_needed_for`, 0043), but
  nothing drives that plan yet.
* **`FROM scratch`** — no base image to extend from at all; producing
  a genuinely empty rootfs is its own future increment.
* **No tag (`-t`/`--tag`)** — real `podman build` without `-t` still
  records an untagged, ID-only image; `oci_store::Store` has no
  "anonymous image" concept yet (every `ImageRecord` is keyed by a
  reference string), so this increment requires a tag rather than
  inventing that plumbing under time pressure.

## `Containerfile` before `Dockerfile`, matching real `podman build`

When `-f`/`--file` isn't given, the context directory's own
`Containerfile` is preferred over `Dockerfile` — real `podman build`'s
own documented default preference (unlike real `docker build`, which
only ever looks for `Dockerfile`) — verified with a dedicated test
(both files present, different `LABEL`s, confirms `Containerfile`
wins).

## Real, manual end-to-end verification before writing automated tests

Built the release binary and ran a real `ociman build` against a real
`docker.io/library/busybox:latest` pull (this project's own real
network access, not a mock), with a Containerfile exercising every
supported instruction at once, then `ociman inspect`ed the result:
`ENV` correctly merged (kept the base image's own `PATH`, appended new
vars), two successive `WORKDIR`s resolved correctly (`/app` then
`/app/sub`), `USER`/`LABEL`/`CMD` all set, real RFC 3339-timestamped
history entries recorded for every instruction, `rootfs.diff_ids`
carried over unchanged from the base image. Also manually verified
every rejection path (`RUN`, multi-stage, `FROM scratch`, missing
`-t`) produces the intended clear error and leaves no partial image
tagged.

## Real, automated tests (offline — no registry access, matching `ociman_run.rs`/`ociman_ps.rs`'s own established approach)

8 integration tests in `tests/tests/ociman_build.rs`, using the same
`seed_image` helper (a synthetic-but-structurally-real image built and
stored directly, no network) every other `ociman` lifecycle test suite
already relies on: the full-instruction-set build described above,
`ENV` update-in-place (not duplicated/reordered), `RUN` rejection
(with a check that the failed build left nothing tagged), multi-stage
rejection, `FROM scratch` rejection, missing-tag rejection,
`Containerfile`-over-`Dockerfile` preference, and an explicit `-f`
override.

## Performance

`ociman build` is a brand new command, invoked by nothing else and
touching none of `ocirun run`/`ociman run`'s own hot-path code
(`oci-runtime-core` untouched this increment) — no benchmark
re-verification needed by the same reasoning used for every earlier
increment that didn't touch shared runtime code.

## What's still not here

* `RUN`/`COPY`/`ADD` — the next real increment: a `RUN`-step executor
  (capture a `Snapshot` before, run the command via `oci_runtime_core`
  against a prepared rootfs, diff after, `commit_layer`/`record_layer`
  the result) is the natural next piece, since every other primitive
  it needs already exists.
* Multi-stage builds (`COPY --from=<stage>`, dependency-ordered
  execution across stages).
* `--build-arg`, the build cache, `ONBUILD`/`HEALTHCHECK`, an
  anonymous/untagged build mode.
