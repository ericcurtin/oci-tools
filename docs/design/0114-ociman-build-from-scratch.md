# Design note 0114: `ociman build` supports `FROM scratch`

Status: implemented
Scope: `bin/ociman/src/build.rs` (`cmd_build`'s own base-layer
resolution match, `scratch_base_config` new); `tests/tests/
ociman_build.rs` (`rejects_from_scratch_with_a_clear_error` replaced
with two real positive tests).

## Why this, now

`FROM scratch` was explicitly rejected since the very first `ociman
build` increment (0050): "no base image to extend from at all;
producing a genuinely empty rootfs is its own future increment" — one
of the few remaining real, common Dockerfile patterns this project's
own module doc comment still flagged as unsupported (the rest of that
list — `ONBUILD`, `HEALTHCHECK`, heredocs, several BuildKit-only-flag
extensions — are all real BuildKit/Docker-specific features with no
real `podman build` equivalent either, much lower value to chase next).
`FROM scratch` itself is a genuinely common real-world pattern (minimal
static-binary images), and — unlike those — supporting it is small,
low-risk, and directly closes a real, frequently-hit gap.

## What real `docker build`/`podman build` actually do, checked directly

Not assumed from documentation — a real `FROM scratch` + `COPY` build
against both real installed tools on this host, then `docker inspect`/
`podman inspect` on the result:

* Zero layers to start (obviously — there is no base image).
* `Config.Env` still gets a default `PATH=/usr/local/sbin:/usr/local/
  bin:/usr/sbin:/usr/bin:/sbin:/bin` baked in by *both* tools, even
  though there's no base image to have inherited it from — not an
  empty `Config` the way one might assume.
* `Architecture`/`Os` are the real build host's own platform (`arm64`/
  `linux` on this session's aarch64 host) — there's no base manifest
  to read them from either.
* Real `docker inspect` additionally reports `WorkingDir: "/"`; real
  `podman inspect` does not set it at all. `ociman`'s own closer real
  equivalent throughout this project has always been `podman` (see
  every prior design doc's own benchmark framing), so this increment
  matches `podman`'s behavior here (no `WorkingDir` set) rather than
  `docker`'s.

## Implementation

`cmd_build`'s own base-layer-resolution `match` gained one more arm,
ahead of the existing external-image-pull one: `None if stage
.base_name.eq_ignore_ascii_case("scratch")` produces
`scratch_base_config()` (a new small function building exactly the
`ImageConfig` described above, using `Platform::host()` for the real
platform fields — the same primitive `oci-registry` already uses to
pick a manifest out of a multi-arch index) paired with an empty layer
vec and no manifest digest (there is nothing to cache — this path never
touches the rootfs cache, extraction, or a registry at all). Every
downstream step (`build_stage`'s own base-layer setup, the local build
cache, `RUN`/`COPY`/`ADD`, layer commit) already handled "zero base
layers, no manifest digest" correctly without any change: `build_stage`
already falls back to a plain, harmless zero-iteration extraction loop
whenever `base_manifest_digest` is `None` (the same code path an
earlier-in-memory-stage base already used before this increment), and
an empty `layers: Vec::new()` starting point composes with everything
the rest of the pipeline (layer commit, cache matching, multi-stage
`FROM`/`COPY --from=`) already does for a normal image's own layer
list — no other file needed a change at all.

## Real, automated tests

`rejects_from_scratch_with_a_clear_error` (asserted the old hard
rejection) replaced with two real positive tests:

* `from_scratch_builds_a_real_zero_base_layer_image_matching_real_
  docker_podman`: `FROM scratch` + `COPY busybox /bin/busybox`,
  asserts exactly one layer (the one real `COPY`, no base layers to
  add to), the real host's own `Platform::host()` architecture/`os:
  "linux"`, the default `PATH` env — and, not just metadata, actually
  `ociman run --rm`s the result (`/bin/busybox echo "hello from
  scratch"`), proving the copied binary really executes in a rootfs
  that started with nothing in it at all.
* `from_scratch_with_no_filesystem_touching_instructions_still_
  builds_a_real_empty_image`: `FROM scratch` + only a `LABEL`,
  matching real `docker build`/`podman build`'s own behavior for this
  case too — a real, valid, zero-layer image, not rejected just
  because there's nothing to extract into a rootfs for in the first
  place.

All other 43 pre-existing `ociman build` integration tests still pass
unmodified. Full `cargo test --workspace --locked` and `cargo clippy
--workspace --all-targets --locked -- -D warnings` both clean.

## What this doesn't do yet

* `docker inspect`'s own additional `WorkingDir: "/"` default for
  `FROM scratch` is deliberately not matched — `podman` (this
  project's own consistently-used real comparison point) doesn't set
  it either, and `ociman run`'s own existing default-`cwd`-when-unset
  handling already produces the right runtime behavior regardless of
  whether the image config states it explicitly.
* `ONBUILD`, `HEALTHCHECK`, heredocs, and the BuildKit-only `RUN
  --mount=`/`COPY --link`/`--parents`/`--exclude=`/`ADD --link`/
  `--keep-git-dir`/`--checksum=`/`--unpack` flags remain unsupported,
  as documented since 0050 — `FROM scratch` was specifically pulled out
  of that list because of its real-world commonality; the rest stay
  lower priority.
