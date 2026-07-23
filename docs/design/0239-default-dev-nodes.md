# Design note 0239: populate `/dev` with the spec's default devices and symlinks

Status: implemented
Scope: `crates/oci-runtime-core/src/rootfs.rs`, `crates/oci-runtime-core/
src/launch.rs`, `tests/tests/ociman_run.rs`.

## The gap 0238 found

Every container this project launches mounts a fresh tmpfs at `/dev`
(via its spec's own mounts) — and until now left it empty except for
the `pts`/`shm`/`mqueue` sub-mounts. No `/dev/null`, no `/dev/zero`,
none of the OCI runtime spec's own mandated "Default Devices"
(`config-linux.md`), and none of the standard symlinks. 0238's own
test-writing surfaced it: busybox `sh` couldn't open `/dev/null` for a
backgrounded job's stdin. Worse, the gap self-camouflaged: a shell's
own `> /dev/null` redirect *creates a regular file* on the writable
tmpfs, so writes appeared to work while accumulating actual content —
`cat /dev/null` returned it back — and reads/opens of anything never
written first simply failed. Real runc/crun containers have real
device nodes; a drop-in replacement must too. This is shared
runtime-core code, so one fix covers `ocirun`/`ociman`/`ocibox`/
`ocicri` alike.

## What real crun does, checked directly (`src/libcrun/linux.c`)

- `needed_devs`: `/dev/null` c 1:3, `/dev/zero` c 1:5, `/dev/full`
  c 1:7, `/dev/tty` c 5:0, `/dev/random` c 1:8, `/dev/urandom` c 1:9 —
  all mode `0666`. Created only when *missing*.
- Under a user namespace (where even mapped root can't `mknod` device
  nodes), each is bind-mounted from the host's own same-named node
  instead — the standard rootless fallback runc documents too.
- `symlinks`: `fd -> /proc/self/fd`, `stdin/stdout/stderr ->
  /proc/self/fd/{0,1,2}`, `core -> /proc/kcore` (dangling when the
  target doesn't exist, e.g. rootless — matching real podman
  containers), and `ptmx -> pts/ptmx` (the only *forced* one,
  replacing whatever's there).
- `/dev/console` only when `process.terminal` is set — this project
  has no terminal allocation at this layer yet, so it's deliberately
  absent here, not forgotten.

## The implementation

A new `RootfsAction::PopulateDev`, planned only when the bundle
itself mounts a **tmpfs** at `/dev` (a bind-mounted host `/dev`, or
no `/dev` mount at all, is left completely alone — matching real
runc/crun), ordered after every `spec.mounts` entry (so `dev/pts`
exists for the `ptmx` symlink) and before `pivot_root` (so the host's
own `/dev/*` nodes are still reachable as bind sources — the same
pre-pivot trick `MaskPath`'s own `/dev/null` bind already relies on).

`populate_dev` tries a real `mknod(2)` first (plus an explicit
re-`chmod` to `0666`, since `mknod` honors the caller's umask — the
same explicit re-chmod crun does), and falls back to the host
bind-mount on `EPERM` specifically — so one code path serves real
root (`mknod` succeeds) and rootless (bind fallback) without probing
anything up front. Anything the image already provides is never
second-guessed.

## Verified

- New unit tests: the plan gains `PopulateDev` exactly between the
  last mount and `PivotRoot` for a tmpfs `/dev`; no-`/dev`-mount and
  bind-mounted-`/dev` bundles get none; `populate_dev`'s symlink half
  (skip-if-provided vs forced `ptmx`) asserted directly against a
  plain directory.
- `tests/tests/ociman_run.rs`: a real rootless container asserts all
  six are genuine character devices (`test -c`), all six symlinks
  exist with the right targets, and — the actual point — the
  read-back semantics are real: `echo x > /dev/null` then
  `cat /dev/null` is empty (a regular-file "null" would return the
  content), and `/dev/zero` yields real NUL bytes. The 0238
  background-job case (`sleep 1 & wait`) confirmed working by hand
  too, with `ls -l /dev/null` showing a real `crw-rw-rw- 1,3` node.
- The `mknod` (real-root) path can't be exercised by this rootless
  dev host or the rootless test suite; the x86_64 VM CI cells run the
  suite as root and cover it. The fallback trigger is `EPERM` alone,
  so a root environment simply never reaches the bind path.
- Full workspace: `cargo build`, `cargo test --workspace`,
  `cargo fmt --all --check`, `cargo clippy --workspace --all-targets
  -- -D warnings`, `python3 ci/guards.py`, `cargo deny check`,
  `bash ci/native-ci.sh`, `ci/build-deb.sh`.
- **Perf** — this is the one increment in a while genuinely on the
  hot startup path (6 nodes + 6 symlinks per container start, work
  real crun/runc also do): `ci/bench.sh` re-run, results in the
  commit/README as usual; the comparison stays fair by construction
  (both sides now do the same setup) and the measured gap holds.
