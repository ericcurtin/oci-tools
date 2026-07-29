# Design note 0304: `ocicri` `CreateContainer` support for `ContainerConfig.mounts`

Status: implemented
Scope: `bin/ocicri/src/runtime_service.rs`, `bin/ocicri/src/bundle.rs`,
`tests/tests/ocicri_container.rs`.

## The gap

`docs/design/0237`'s own module doc comment explicitly named "CRI
mounts/devices" as deliberately out of scope for `bundle.rs`'s first
slice, and it was never picked up since. Without this, no volume,
ConfigMap, Secret, or hostPath a real kubelet mounts into a pod could
ever reach the container at all — a materially more consequential gap
than any single `ociman` CLI flag, since `ContainerConfig.mounts` is
the one generic mechanism all of those flow through in the real CRI
protocol.

(0296/0297's own "still ahead" notes separately named Kubernetes'
`PodSpec.HostAliases` as a related gap — checked directly against the
vendored `api.proto` and real cri-o's own `server/container_create.go`
this session: that premise doesn't actually hold. `PodSandboxConfig`
has no `HostAliases` field at all; the kubelet resolves `HostAliases`
entirely out-of-band and hands the runtime a plain bind mount via this
same `ContainerConfig.mounts` field instead — so closing this note
also, incidentally, makes `HostAliases` support reachable as a future
plain-mount case, not a separate mechanism of its own.)

## Scope: a real, deliberately narrow first slice

Matching this project's own established "narrow first slice, clear
errors for the rest" convention (`ocicri`'s module doc comment cites
`ociboot build-image` before `install to-disk` as the precedent):

**Implemented** — a plain OCI bind mount, translated the exact same
way `ociman run -v`'s own `synthesize_spec` already does
(`Mount{Type: "bind", Options: ["rbind", ...]}`):
- `container_path`/`host_path`/`readonly`.
- The private (default) propagation mode only, mapped to real cri-o's
  own exact `["rbind", "rprivate"]` option pair (checked directly,
  `~/git/cri-o/server/container_create_linux.go`'s own
  `addOCIBindMounts`).
- A missing `host_path` is auto-created as a directory
  (`fs::create_dir_all`) — a real, checked-directly cri-o behavior
  (`os.MkdirAll`), *not* what the proto's own doc comment alone
  suggests ("if the host path doesn't exist, runtimes should report
  an error"): the actual installed/cloned cri-o source never errors
  here, since real kubelet `HostPath` volumes of type
  `DirectoryOrCreate` depend on exactly this runtime behavior. Checked
  directly rather than assumed from the proto's own comments alone,
  matching this project's own "verify against real source/binaries,
  not documentation" standard.

**Deliberately out of scope** (each a real, honest
`Status::unimplemented` rather than a silent misinterpretation):
image volume mounts (`Mount.image`, the separate Image Volume Source
KEP mechanism — not a bind mount at all); any propagation mode other
than the private default (`HOST_TO_CONTAINER`/`BIDIRECTIONAL` both
need a real shared-mount-namespace setup this project has none of);
`selinux_relabel` (this project implements no SELinux concept
anywhere at all, matching `ociman run -v`'s own already-established
identical narrowing); `recursive_read_only`; and any UID/GID mapping
(no user-namespace-remapped mount concept for CRI containers).
Symlink-following for `host_path` (the proto's own documented
contract) is also not implemented yet — a real, smaller gap of its
own.

An empty `container_path`/`host_path` (when `image` isn't set) are
real client-input errors, matching real cri-o's own exact validation
strings verbatim (`"mount.ContainerPath is empty"`/`"mount.HostPath is
empty"`).

Unlike real cri-o's own richer `addOCIBindMounts` (which removes/
overrides a default mount at the same destination when a CRI mount
targets it, e.g. overriding `/dev`), this slice simply appends CRI
mounts after the standard set — matching `ociman run -v`'s own
identical simpler convention, not a full port of that override logic
yet.

## Implementation

`build_cri_bind_mounts` (new, `runtime_service.rs`): validates every
`cri::Mount` and translates the supported case into an
`oci_spec_types::runtime::Mount`, called from `create_container`
*before* `bundle::prepare` ever extracts a single layer — a
config-shaped client error should never cost a real, wasted rootfs
extraction, the same reasoning `PrepareError::NoCommand` already
established for a missing command. The resulting `Vec<Mount>` is
threaded through a new `CriProcessConfig::mounts` field;
`build_spec` (`bundle.rs`) extends `spec.mounts` with it, appended
after the standard proc/sys/dev set.

## Verified

Manual: none needed beyond the integration tests below — this feature
is fully exercisable through the real gRPC surface `tests/tests/
ocicri_container.rs` already drives against a real spawned `ocicri`
server.

Integration (`tests/tests/ocicri_container.rs`, 3 new tests):
`create_container_translates_a_plain_bind_mount_into_the_generated_spec`
— two mounts (read-write and read-only), one with a genuinely missing
host path, verified against the real generated `config.json`: correct
`destination`/`source`/`type`/`options` (`rbind`+`rprivate`, plus `ro`
for the read-only one), and the missing host path is confirmed
auto-created as a real directory afterward.
`create_container_rejects_unsupported_mount_fields_clearly` — every
deliberately-out-of-scope field (`image`, `selinux_relabel`, non-
private `propagation`, `recursive_read_only`, non-empty
`uid_mappings`) is a real `Status::Unimplemented`; empty
`container_path`/`host_path` are real `Status::InvalidArgument` with
cri-o's own exact message text.
`create_container_bind_mount_is_genuinely_live_at_runtime` — the
strongest proof available: a real container is actually started, a
file written on the host side of the mount is read back correctly via
a real `ExecSync` running *inside* the container, and a file the
container itself writes through the mount is confirmed back on the
host side afterward — proving the mount is genuinely live at the
kernel level, not merely declared in JSON.

Regression: all 23 `ocicri_container.rs` tests pass (20 pre-existing +
3 new); all 28 `ocicri` unit tests pass (2 pre-existing `bundle.rs`
tests updated for the new required `CriProcessConfig::mounts` field);
full `cargo test --workspace --locked` (111 test result blocks, 0
failures).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).

Performance: no impact on any benchmarked hot path — `ocicri
CreateContainer` isn't part of `docs/benchmarks.md`'s tracked
comparisons, and this slice adds work only when `ContainerConfig.
mounts` is actually non-empty (the overwhelmingly common `crictl`-
driven test case gives none at all). A fresh, real `ci/bench.sh` run
this same session (checking for any drift since `0288`'s own recorded
table, 16 increments stale) confirmed every measurable comparison
still decisively winning, no regression found.

## Still ahead

Real cri-o's own richer mount handling: image volume mounts, non-
private propagation modes, SELinux relabeling, recursive read-only,
UID/GID-mapped mounts, symlink-following for `host_path`, and
overriding (rather than merely appending after) a default mount at
the same destination. Each is a real, separately-scoped increment, not
a silent gap — see this note's own "deliberately out of scope" list
above for exactly which `Status::unimplemented` covers which case
today.
