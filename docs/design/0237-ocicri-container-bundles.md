# Design note 0237: `ocicri` `CreateContainer` prepares a real, launch-ready bundle

Status: implemented
Scope: `bin/ocicri/src/bundle.rs` (new), `bin/ocicri/src/runtime_service.rs`,
`bin/ocicri/src/main.rs`, `bin/ocicri/Cargo.toml`,
`tests/tests/ocicri_container.rs`.

## From records to bundles — the shared prerequisite for `StartContainer`

0236 made `CreateContainer` a real record with real CRI semantics but
nothing on disk beyond the record itself. Real cri-o prepares
everything else at create time too (checked directly,
`server/container_create.go`: container storage and the generated OCI
spec are both built inside `CreateContainer`, long before any start) —
and this project's own `StartContainer`, when it lands, will need a
bundle to launch. This increment closes that gap: `CreateContainer`
now prepares a real, complete, *verified launch-ready* OCI bundle at
`<storage-root>/cri-bundles/<container-id>/`.

Scoping note, considered explicitly rather than discovered later: a
full `StartContainer` is *not* this increment, and can't be a small
one — `oci_runtime_core::launch`'s fork-based entry points carry a
real "calling process must be single-threaded" safety contract
(`ociman run -d`'s own keeper forks while its CLI process is still
single-threaded), which `ocicri`'s multithreaded tokio server can
never satisfy directly. Launching from `ocicri` needs its own real
design (a per-container helper process in the spirit of real cri-o's
own conmon — respecting both the fork-safety contract and this
project's own strict no-shelling-out policy), which deserves its own
note rather than a rushed corner of this one.

## What the bundle contains

- `rootfs/` — every image layer extracted via the same shared
  `oci_layer::apply` every other binary uses, into a dedicated,
  writable, per-container copy (a CRI container is stateful; the same
  "never share a writable rootfs" reasoning `ocibox create` already
  established, deliberately not `oci_store`'s shared read-only
  `rootfs_cache`).
- `config.json` — a real generated spec: the same
  `Spec::example().into_rootless(euid, egid)` base + writable-root
  override + podman-default capability set + default seccomp profile
  every other container this project launches gets (`ociman`'s
  `synthesize_spec`, `ocibox`'s `enter_spec`), with the process half
  driven by the CRI and image configs:
  - **args** — real cri-o's own `SpecSetProcessArgs` merge, ported
    rule for rule (its own comment: "same as docker does today"): a
    non-empty CRI `command` ignores the image config entirely; an
    empty one inherits the image `Entrypoint`; an empty `args`
    additionally inherits the image `Cmd`; nothing anywhere is real
    cri-o's own verbatim `no command specified` error
    (`InvalidArgument`), which also deliberately costs no rootfs
    extraction (the spec is built first).
  - **env** — image env first, kubelet-supplied env after (a
    duplicate key wins by coming later), with the same real `PATH`
    fallback `ociman` applies when nothing declares any env at all
    (0194, checked there against real podman's own specgen).
  - **cwd** — CRI `working_dir`, else image `WorkingDir`, else `/`.

## Verified launch-ready, not just written

Before `prepare` ever declares success, it round-trips the result
through the exact same two calls every real launch in this project
starts with — `oci_runtime_core::Bundle::load` +
`oci_runtime_core::validate::validate` — so "created" genuinely means
"startable", and a spec-generation bug surfaces at `CreateContainer`
time rather than as a later mystery `StartContainer` failure.

Cleanup is symmetrical and complete: a failed `prepare` removes its
own directory before returning (verified: a rejected no-command
create leaves the bundle family untouched); a failed record write
removes the fresh bundle (never an orphan a record can't reach);
`RemoveContainer` and `RemovePodSandbox`'s container cascade both
remove the bundle alongside the record; and a record predating this
increment (no bundle at all) removes cleanly — `bundle::remove` is a
real, silent no-op for a missing directory.

## Deliberately out of scope (each a real, later increment)

Joining the sandbox's namespaces (none are pinned yet — 0233),
per-container `run_as_user`/security-context mapping, CRI
mounts/devices, resource limits, hostname/`/etc/hosts`/`resolv.conf`
wiring, the CRI log path — and `StartContainer` itself (see the
scoping note above).

## Verified

- New unit tests in `bundle.rs`: every branch of the cri-o merge
  table (both/command-only/args-only/neither/args-with-no-entrypoint/
  nothing), env/cwd precedence, the empty-env `PATH` fallback.
- `tests/tests/ocicri_container.rs`, over a real Unix socket: the
  lifecycle test now also proves the bundle exists with a real
  extracted `rootfs/bin/sh` and a real `config.json` carrying the CRI
  command (and a writable root), and that `RemoveContainer`/
  `RemovePodSandbox` both remove it; a new test drives the
  entrypoint+args merge end to end through a second seeded image with
  a real declared `Entrypoint`/env, and the `no command specified`
  rejection including its no-leftover guarantee.
- Full workspace: `cargo build`, `cargo test --workspace`,
  `cargo fmt --all --check`, `cargo clippy --workspace --all-targets
  -- -D warnings`, `python3 ci/guards.py`, `cargo deny check`,
  `bash ci/native-ci.sh`, `ci/build-deb.sh`, `ci/bench.sh` sanity.
