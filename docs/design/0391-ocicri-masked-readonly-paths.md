# Design note 0391: `ocicri CreateContainer` honors `masked_paths`/`readonly_paths`

Status: implemented
Scope: `bin/ocicri/src/bundle.rs`, `bin/ocicri/src/runtime_service.rs`,
`tests/tests/ocicri_container.rs`, `README.md`.

## What this closes

`ContainerConfig.linux.security_context.masked_paths`/`.readonly_paths`
were never read anywhere in `ocicri` — a pod's own explicit extra
masked/read-only paths (a real, security-conscious request some pods
make, e.g. hiding an additional sensitive `/proc`/`/sys` entry beyond
this project's own already-existing default list) were silently
dropped, an easy-to-miss but real divergence from the pod's own
declared intent.

## Real, checked-directly confirmation

- `crates/oci-cri-types/proto/api.proto` lines 1071-1076:
  `LinuxContainerSecurityContext.masked_paths`/`.readonly_paths`
  (fields 13/14), documented as "can be passed directly to the OCI
  spec" — the simplest possible mapping, no translation needed.
- `~/git/cri-o/internal/factory/container/container.go`'s own
  `SpecSetPrivileges` (lines ~845-855): `for _, path := range
  securityContext.GetMaskedPaths() { specgen.AddLinuxMaskedPaths(path)
  }` (same shape for `ReadonlyPaths`) — only reached when
  `!c.Privileged()`, which this project has no separate gate for at
  all since `privileged: true` is already a hard, earlier
  `Status::unimplemented` (`0389`) well before `build_spec` is ever
  reached.
- `~/git/moby/vendor/github.com/opencontainers/runtime-tools/generate/
  generate.go`'s own `AddLinuxMaskedPaths`/`AddLinuxReadonlyPaths`
  (the real generator library cri-o vendors and calls): a plain
  `append(...)`, confirming real cri-o's own behavior is "append onto
  whatever the spec generator already seeded as defaults," never
  "replace" — this project's own `Spec::example()`'s `default_masked_
  paths()`/`default_readonly_paths()` are the equivalent default seed
  here.

## Implementation

- `bundle::CriProcessConfig` gains `pub masked_paths: &'a [String]`
  and `pub readonly_paths: &'a [String]`.
- `build_spec` appends them onto `linux.masked_paths`/
  `linux.readonly_paths` right after writing `linux.resources` (0390):
  `linux.masked_paths.extend(cri.masked_paths.iter().cloned())` (and
  the identical line for `readonly_paths`) — the smallest possible
  change, since `Vec::extend` already provides the exact "append,
  never replace" semantics real cri-o's own `AddLinux*` calls do.
- `runtime_service.rs`'s `create_container` resolves both slices from
  `config.linux.security_context`, defaulting to an empty `Vec`
  (matching every other field's "absent security context, or a
  privileged-rejected request, means nothing extra" convention) right
  next to the existing `readonly_rootfs` resolution.

## Tests

One new unit test in `bin/ocicri/src/bundle.rs`:
`build_spec_appends_extra_masked_and_readonly_paths_onto_the_existing_
defaults` — confirms both this project's own existing default entries
survive *and* the new one is added, not a replacement. The three
pre-existing `build_spec` tests updated with the two new struct
fields.

One new integration test in `tests/tests/ocicri_container.rs`:
`create_container_masked_paths_genuinely_masks_a_real_file_inside_the_
running_container` — unlike `readonly_rootfs`'s own spec-only check
(`0388`, chosen specifically because a real write-rejection assertion
can silently no-op under this project's rootless model on some
hosts), this one is verified genuinely end to end: a real started
container with an extra `masked_paths: ["/etc/hosts"]` (a real file
this project's own `CreateContainer` already writes into the extracted
rootfs before the container ever starts, `0296`) reads back as a real,
empty `/dev/null` via a real `ExecSync`. This is safe to check
end-to-end (unlike the read-only cases) because masking a file is a
fresh, brand-new bind mount entirely within this project's own
unprivileged user namespace's own authority over its own private mount
namespace — it doesn't need `CAP_SYS_ADMIN` in the namespace that owns
some pre-existing superblock the way remounting an existing mount
read-only does. All existing tests across `ocicri_container.rs` (31
pre-existing) and `bundle.rs`'s own module tests continue to pass
unmodified.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures), `python3
ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh`, `bash
ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg -r` round trip).
This change touches only `bin/ocicri`, which `ci/bench.sh` doesn't
measure at all — no benchmark re-run needed.

## Deliberately still out of scope

`readonly_paths`'s own end-to-end write-rejection behavior is *not*
independently re-verified the way `masked_paths` is here — it shares
the exact same `RootfsAction::RemountReadonly` mechanism (and its own
tolerated-`PermissionDenied` fallback) `readonly_rootfs` already
established can silently no-op on some hosts, so a real write-attempt
assertion for it would carry the identical host-dependent risk
`0388`'s own doc comment already documents; the unit test's own spec-
level check is the appropriate level of confidence here, matching that
same precedent. Every other `LinuxContainerSecurityContext` field
surveyed alongside `readonly_rootfs`/`privileged`/`resources`
(`0388`/`0389`/`0390`'s own "deliberately still out of scope"
sections) remains a real, separate, unrelated gap.
