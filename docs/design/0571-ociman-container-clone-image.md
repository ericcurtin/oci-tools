# Design note 0571: `ociman container clone`'s positional `IMAGE`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_clone.rs`.

## What this closes

`docs/design/0474`'s own doc comment (both the design note itself and
`Command::Clone`'s own source doc comment) explicitly named this as
still open: *"a positional `IMAGE` argument that pulls a genuinely
*different* image for the clone… is a real, deliberately deferred
gap, not yet accepted at all."* No later note closed it. This adds
it: `ociman container clone CONTAINER NAME IMAGE` now accepts the
third positional and genuinely extracts the clone's own fresh rootfs
from that image instead of the source container's own recorded one.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/clone.go:17,54-71`:
  `Use: "clone [options] CONTAINER NAME IMAGE"`, `Args:
  cobra.RangeArgs(1, 3)`; the exact positional-consumption rule —
  `switch len(args) { case 2: ctrClone.CreateOpts.Name = args[1];
  case 3: ctrClone.CreateOpts.Name = args[1]; ctrClone.Image = args[2]
  ... pullImage(...) }`. Critically, a *second* positional with no
  third is **always** the name, never an image, regardless of what it
  looks like — clap's own identical left-to-right optional-positional
  consumption matches this exactly with no special-casing needed.
- `~/git/podman/pkg/domain/infra/abi/containers.go:1765-1769`
  (`ContainerClone`): `spec := specgen.NewSpecGenerator(ctrCloneOpts.
  Image, ...)` seeds the new spec's image field *before*
  `generate.ConfigToSpec(ic.Libpod, spec, ctrCloneOpts.ID)` unmarshals
  the *source* container's own live config on top of it.
- `~/git/podman/pkg/specgen/generate/container.go:379-431`
  (`ConfigToSpec`): the source's own current config (`conf`) is
  marshaled to JSON and unmarshaled directly into `specg` — process
  args, env (explicitly re-derived from `conf.Spec.Process.Env` right
  after, `container.go:414-424`), resource limits, and everything
  else the source already has baked in. Nothing in this function ever
  reads the new image to override any of that — the new `Image` value
  only ever matters for where the *rootfs* itself comes from, later,
  when the container is actually created from this spec.
- `~/git/podman/cmd/podman/common/create_opts.go:73-90`
  (`DefineCreateDefaults`): `opts.Pull = policy()` — the exact same
  `missing` default this project's own `--pull` already uses
  elsewhere, confirming there's no separate pull-policy convention to
  port for this one positional.

## Real functional gap, not a no-op

Before this, a third positional was a hard clap "unexpected argument"
error — there was no way at all to clone onto a different image.
Live-verified by hand, side by side against a real installed `podman
4.9.3`: created a source container from `busybox` with an explicit
command (`sh -c "echo marker"`), cloned it onto `alpine` with an
explicit name, and confirmed **both** tools report the clone's own
image as `alpine` while its command stays the source's own
`sh -c echo marker` (never `alpine`'s own default `cmd`) — and that
starting the clone actually runs that command inside `alpine`'s own
real rootfs, not `busybox`'s.

## Why this is narrow and safe

Reuses this project's own already-established, already-tested image-
resolution machinery verbatim — the exact same "id first, then a
parsed reference through `resolve_or_pull`" ordering `cmd_create`
itself already established (`0179`-`0181`), just called from
`cmd_clone` instead. When `IMAGE` is given, only two things change
from the pre-existing behavior: which image's manifest/layers get
extracted into the clone's fresh rootfs, and which reference gets
recorded onto the clone's own `ANNOTATION_IMAGE` — the source's own
config-copying logic (byte-for-byte `config.json`, labels, resources)
is completely untouched, matching real podman's own `ConfigToSpec`
behavior exactly (see above). No cgroup, namespace, capability,
systemd, or mount code is anywhere near this change.

## Tests

Three new integration tests in `tests/tests/ociman_clone.rs`:
- `clone_with_a_different_image_extracts_its_rootfs_but_keeps_the_sources_own_config`
  — two distinguishably-seeded images (each with its own marker
  file and a different default `cmd`), proving the clone's own
  `image` field, real rootfs content (the new image's marker file
  present, the old one absent), and `command` field (still the
  source's explicit one) all end up exactly right, then actually
  starts the clone and confirms the source's own command really runs
  inside the new image's real rootfs.
- `clone_with_only_two_positionals_treats_the_second_as_a_name_never_an_image`
  — a tag-shaped (but valid-as-a-container-name) second positional
  with no third becomes the new container's own literal name, and the
  clone still uses the source's own recorded image — locking in real
  podman's own exact `case 2` vs `case 3` positional-consumption rule.
- `clone_with_an_unresolvable_image_is_a_clear_error` — an
  unreachable-host reference (the same `127.0.0.1:1` pattern
  `ociman_pull_policy.rs`'s own tests already establish, avoiding any
  real network dependency) is a real, immediate error, never a silent
  fallback to the source's own image.

Manually verified end to end beyond the automated tests: real `pull`
of two genuinely different public images, cloning across them, and a
direct side-by-side comparison against a real installed `podman
4.9.3` (see "Real functional gap" above) — not just source-reading.

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (129
test-result blocks, all passing on the first attempt with
`RUST_TEST_THREADS=2` — no new test file added, so the block count is
unchanged from `0570`), `python3 ci/guards.py` (clean), `cargo deny
check` (clean), `bash ci/native-ci.sh` (two isolated,
already-known-flaky `ocicri_container.rs` capabilities-test failures
under this host's own concurrent-session CPU contention on the first
two attempts, both confirmed transient — individually via an isolated
rerun, and wholesale by reproducing the exact same test passing
reliably against an unmodified `git stash`-ed checkout; the third
attempt, with `RUST_TEST_THREADS=2` — `ci/native-ci.sh`'s own `cargo
test` invocation doesn't set this itself, unlike this project's own
manual verification convention — ran completely clean), `bash
ci/build-deb.sh` (clean on the first attempt, real `dpkg -i`/
`--version`/`dpkg -r` round trip). No `ci/bench.sh` rerun needed:
`ociman container clone` is not exercised by it at all, the same
reasoning `0474` itself already established for this same command.

## Deliberately still out of scope

Real podman's own much larger `container clone` flag surface (every
`create`-time resource/health/etc. override) remains unaccepted,
exactly as `0474` originally scoped — this note only closes the one
specific gap `0474` named explicitly. `--platform`/`--arch`/`--os`
overrides for the new image (real podman's own `pullImage` reads
these from the same `CreateOpts` every other `create`-time flag
would; this project's own first slice always resolves for the host
platform) remain unaccepted too, a new, narrower restriction this note
introduces rather than one `0474` already flagged — left as a future
candidate if a real need for it ever surfaces.
