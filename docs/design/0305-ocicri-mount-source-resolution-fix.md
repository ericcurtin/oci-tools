# Design note 0305: `ocicri` mount-source resolution fix (existing files + symlinks)

Status: implemented
Scope: `bin/ocicri/src/runtime_service.rs`, `tests/tests/ocicri_container.rs`.

## The bug

`0304`'s own `build_cri_bind_mounts` called `std::fs::create_dir_all`
on *every* `Mount.host_path` unconditionally, before ever bind-mounting
it. Confirmed with a live repro (not assumed): `create_dir_all` on a
path that already exists **as a regular file** fails outright with
`EEXIST` ("File exists"). A single existing file as a mount source is
a common, ordinary real kubelet case — `/etc/localtime`, a single
ConfigMap key, `/etc/machine-id` — so `CreateContainer` would hard-fail
for exactly the case real cri-o handles most routinely. Caught while
researching 0304's own "still ahead" list (symlink-following), not
merely a documentation gap: a real, currently-shipped regression, with
no existing test covering it (0304's own tests only exercised a
missing directory and an already-existing directory via `/tmp`).

## The fix, matched against real cri-o exactly

Read `~/git/cri-o/server/container_create.go`'s own
`resolveSymbolicLink` directly, plus its call site in
`container_create_linux.go`'s `addOCIBindMounts`, rather than
re-guessing the right behavior:

1. `Lstat` the path first.
2. If it exists and is **not** a symlink: use it exactly as given —
   file or directory, never touched, never auto-created.
3. If it exists and **is** a symlink: resolve it to its real target
   (real cri-o's own `securejoin.SecureJoin`-based confinement isn't
   ported here — that exists specifically for cri-o's own
   `BindMountPrefix`-style host-path redirection, a concept this
   project has no equivalent of at all, so a plain
   `fs::canonicalize` is the faithful match for this project's own
   architecture).
4. Only if the path is genuinely **missing** (`ErrorKind::NotFound`):
   `create_dir_all` it — matching real cri-o's own `os.IsNotExist`
   branch exactly, the same behavior `0304` already correctly
   identified real kubelet `HostPath` volumes of type
   `DirectoryOrCreate` depend on.
5. Any other I/O error (e.g. permission denied) is a real, surfaced
   `Status::internal` rather than silently swallowed.

A new `resolve_mount_source` implements exactly this, replacing the
previous unconditional `create_dir_all` call in
`build_cri_bind_mounts`. This also closes `0304`'s own separately-named
"still ahead" item (symlink-following) as a side effect, since both
fixes live in the same handful of lines.

## Verified

Manual: reproduced the original bug directly with a standalone Rust
program confirming `create_dir_all` on an existing file returns
`EEXIST` before writing any fix, then confirmed the fixed code no
longer hits it.

Integration (`tests/tests/ocicri_container.rs`, 2 new tests):
`create_container_bind_mounts_an_already_existing_single_file` — a
real container is created with a `Mount` whose `host_path` is an
existing file (not a directory); `CreateContainer` now succeeds (it
used to fail), the host file is confirmed completely untouched
afterward (still a real file, unchanged content — proving it was never
routed through `create_dir_all` at all), and a real started
container's `ExecSync` reads the file's own real content back through
the mount. `create_container_bind_mount_follows_a_symlinked_host_path`
— a `host_path` that's a real symlink to a separate file is followed
to its actual target's content, verified the same way (a real started
container, real `ExecSync` read-back).

Regression: all 25 `ocicri_container.rs` tests pass (23 pre-existing +
2 new, including the 3 tests `0304` itself added last turn); full
`cargo test --workspace --locked` (111 test result blocks, 0
failures).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).

Performance: no impact on any benchmarked hot path — `ocicri
CreateContainer` isn't tracked in `docs/benchmarks.md`, and this
change replaces one filesystem call (`create_dir_all`) with an
equivalent-cost `symlink_metadata` check plus a conditional
`create_dir_all`/`canonicalize` — no new work on the common
no-mounts-given path at all.

## Still ahead

Real cri-o's own richer mount handling — image volume mounts, non-
private propagation modes (confirmed, independently, to genuinely need
new `/proc/self/mountinfo`-parsing and rootfs-propagation
infrastructure this project has none of, not merely a smaller
oversight), `selinux_relabel` (this project implements no SELinux
concept anywhere), `recursive_read_only` (needs `mount_setattr(2)`-
based `rro` support `crates/oci-mount` doesn't have yet either), and
UID/GID-mapped mounts — all remain genuinely separately-scoped, larger
increments, unchanged from `0304`'s own assessment.
