# Design note 0460: real `/etc/hostname` (`ociman run`/`create`/`ocicri`) + `ociman build --no-hostname`

Status: implemented
Scope: `crates/oci-runtime-core/src/etc_hosts.rs`, `bin/ociman/src/
main.rs`, `bin/ociman/src/build.rs`, `bin/ocicri/src/bundle.rs`,
`tests/tests/ociman_run.rs`, `tests/tests/ociman_build.rs`, `tests/
tests/ocicri_container.rs`.

## What this closes

Found while researching `ociman build --no-hostname` (`0459`'s own
"deliberately still out of scope" note): **every container this
project has ever started — not just `ociman build`'s own `RUN`
steps — has been missing a real `/etc/hostname` file.** `ociman run`/
`create` already set the UTS namespace's own hostname (`spec.
hostname`, applied via a real `sethostname(2)` inside the container),
but never wrote the *separate* `/etc/hostname` file real `docker`/
`podman` containers always have too — a program that reads that file
directly (rather than calling `gethostname(2)`) would see whatever
the base image's own rootfs happened to ship (or nothing at all)
instead of the container's real hostname. `ocicri` had the identical
gap for the same reason (it shares `oci_runtime_core::etc_hosts`'s
`write_etc_hosts` already, for the exact same underlying "no real
container networking" reason `0296`/`0297` document).

## Real, checked-directly confirmation

`~/git/podman/libpod/container_internal_linux.go:602-609`: `if _, ok
:= c.state.BindMounts["/etc/hostname"]; !ok { hostnamePath, err :=
c.writeStringToRundir("hostname", c.Hostname()+"\n") ... }` — the
exact same value passed to `sethostname(2)` (`c.Hostname()`), written
with a trailing newline, bind-mounted at container start. This
project writes files directly into the effective rootfs instead of
bind-mounting from a separate rundir (the same simpler approach
already established for `/etc/hosts`/`/etc/resolv.conf`), so the new
primitive follows that same shape rather than podman's own
bind-mount mechanics.

## Implementation

- New `oci_runtime_core::etc_hosts::write_etc_hostname(root: &Path,
  hostname: &str)`: writes `root/etc/hostname` containing
  `format!("{hostname}\n")`, creating `root/etc` first if missing —
  the same "effective, currently-writable root" convention (plain
  extraction or rootless-overlay `upper/`) `write_etc_hosts` already
  uses, right next to it in the same module (three new unit tests).
- `ociman run`/`create`'s own shared container-preparation code
  (where `effective_hostname`/`write_root` are already computed for
  `write_etc_hosts`'s own call) now also calls `write_etc_hostname
  (&write_root, effective_hostname)` right after — the exact same
  value `synthesize_spec`'s own `spec.hostname = Some(hostname.
  unwrap_or(id).to_string())` computes, recomputed identically here
  rather than threaded back out, since this call site already
  independently needs it for `own_names`.
- `ocicri`'s own `bundle.rs` (`prepare_bundle`, the same function
  `write_etc_hosts` already lives in) gained the identical call right
  after its own `write_etc_hosts`, using `cri.hostname` — the same
  value already passed to `spec.hostname` a few lines below.
- `ociman build`'s own `build_stage` gained a new `no_hostname: bool`
  parameter (inserted after `no_hosts`, the same shape `0459`
  established) guarding a new `write_etc_hostname(&rootfs_dir, "")`
  call, right after the existing (now also-conditional) `write_etc_
  hosts` call. Always an empty hostname: this project's own
  `ImageConfig`/`ContainerConfig` model no persisted-across-`FROM`
  hostname field at all (a real, separately-scoped gap — no
  Containerfile instruction or flag could ever set one even if it
  did) — the same literal value real buildah's own default resolves
  to for the overwhelming majority of real Containerfiles too (a
  base image's own `Config.Hostname` is essentially never set in
  practice).
- `Command::Build` gains `no_hostname: bool` (`--no-hostname`),
  inserted after `no_hosts`, before `quiet` — closing `0459`'s own
  "still out of scope" note in the very next increment, as promised
  there.

## Tests

Two new tests in `tests/tests/ociman_run.rs`
(`run_writes_a_real_etc_hostname_matching_the_uts_namespaces_own_
hostname`, `run_without_hostname_flag_etc_hostname_defaults_to_the_
containers_own_id`), one new test in `tests/tests/ocicri_container.rs`
(`create_container_writes_a_real_etc_hostname_matching_the_sandboxs_
own_hostname`), and two new tests in `tests/tests/ociman_build.rs`
(`build_without_no_hostname_writes_a_real_empty_etc_hostname_for_run_
steps`, `build_no_hostname_leaves_the_base_images_own_etc_hostname_
completely_untouched` — the latter seeding a base image with its own
distinctive `/etc/hostname`, the same `seed_image_with_files` pattern
`0459`'s own `--no-hosts` test established). All prior tests in every
touched file pass unmodified (108/108 in `ociman_run.rs`, 37/37 in
`ocicri_container.rs`, 138/138 in `ociman_build.rs`).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (120 test-result
blocks, 0 failures, clean on the first full run), `python3 ci/
guards.py`, `cargo deny check`, `bash ci/native-ci.sh` (three
transient, known-flaky failures across three consecutive attempts —
`ocicri_container`'s own `create_container_oom_score_adj_sets_a_real_
value`, `ociman_exec`'s own `exec_joins_a_still_running_ociman_run_
container`, then `ocicri_container`'s own `create_container_applies_
the_sandboxs_own_sysctls_to_a_real_running_container`/`create_
container_masked_paths_genuinely_masks_a_real_file_inside_the_
running_container` — every one confirmed unrelated to this change and
passing instantly in isolation; this dev host had unusually heavy
concurrent load from several other running sessions during this
particular turn, consistent with (if more pronounced than usual) the
already-documented, accepted environmental flakiness from the
long-running CPU-spinning background process this host has carried
since before this session; the full script finally passed clean
120/120 on the fourth attempt), `bash ci/build-deb.sh` (real `dpkg
-i`/`--version`/`dpkg -r` round trip). Unlike `0459` (build-only, no
hot-path impact), this genuinely adds one small file write to every
`ociman run`/`create`/`ocicri create_container` call — ran the full
`ci/bench.sh` suite to confirm no regression: all 9 categories show
speedups consistent with or better than previously-recorded baselines
(`run --rm` 6.41x/`run -d` 4.31x/`rm` 46.45x/`commit` 37.08x faster
than podman, `build --no-cache` 16.70x faster than docker, `build`
(cached) 21.93x faster than podman), the new write's cost lost
entirely in ordinary run-to-run noise.

## Deliberately still out of scope

`--omit-history` remains buildah's own one other non-resource
`CommonBuildOptions` boolean not yet ported. `ociman build --volume`/
`--secret` (BuildKit-/buildah-style mounts) also remain a larger,
differently-shaped gap. This project's own `ImageConfig`/
`ContainerConfig` still model no persisted-across-`FROM` hostname
field at all — a real, separately-scoped gap noted above but not
fixed here (would need real Containerfile/image-config support for
a `Config.Hostname`-equivalent field, matching real Docker's own
`ENV`/`LABEL`-adjacent but distinct config surface, not just a CLI
flag).
