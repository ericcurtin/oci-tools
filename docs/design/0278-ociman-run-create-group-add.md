# Design note 0278: `ociman run`/`create --group-add`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `bin/ociman/src/user_resolve.rs`,
`tests/tests/ociman_run.rs`.

## Closing the real gap noted in `0276`

`0276` (`ocirun exec -g`/`--additional-gids`) checked directly and
found real `podman exec` has no equivalent flag at all — its own
supplementary-group support instead lives on `podman run`/`create
--group-add`, a different flag at container-creation time, with
richer name-or-GID resolution. This note closes that real, checked-
directly gap.

## Real semantics, checked directly against real podman's own source

`~/git/podman/docs/source/markdown/options/group-add.md`: "Assign
additional groups to the primary user running within the container
process." `~/git/podman/vendor/github.com/moby/sys/user/user.go`'s own
`GetAdditionalGroups` (the actual resolution function, vendored into
real podman as `pkg/lookup.GetContainerGroups`) gives the precise
rule:

- Each value is either a **numeric GID** (used as-is even without a
  matching `/etc/group` entry) or a **name** (must resolve against the
  container's *own* `/etc/group` — not the host's — a clear error if
  it doesn't).
- Resolved gids are deduplicated (a `map[int]struct{}` in real podman;
  a `BTreeSet<u32>` here, functionally identical — order isn't
  semantically meaningful for a supplementary-group list either way).
- The special value `keep-groups` (pass the *host* user's own real
  supplementary groups through unchanged) is handled entirely
  separately at the CLI layer in real podman
  (`cmd/podman/containers/create.go`): mutually exclusive with any
  other `--group-add` value in the same call, translated into a
  `RunOCIKeepOriginalGroups=1` annotation real podman's own docs
  describe as "Currently only available with the crun OCI runtime".

## Scope: `keep-groups` deliberately not implemented

`keep-groups` needs genuinely different, annotation-driven, runtime-
level support — real crun reads that specific annotation and swaps in
the *calling* rootless user's own real `/proc/self/status` supplementary
groups instead of the container's declared ones. This project's own
`ocirun` has no equivalent mechanism (or annotation-reading convention)
for that at all, and building one is a real, separately-scoped runtime
feature, not a small CLI addition. Rather than silently ignore
`keep-groups` or approximate it incorrectly, it's a clear, honest "not
yet supported" error.

## Implementation

Reuses `user_resolve.rs`'s own already-established, already-tested
`/etc/group`/`/etc/passwd`-reading infrastructure (symlink-escape-safe
via `RESOLVE_IN_ROOT`, the same protection `resolve_user`/`ociman exec
--user` already rely on) via one new function, `resolve_group_add`,
built from the exact same private `find_group_gid` helper `resolve`
itself already used internally — no duplicated parsing logic. Resolved
gids are set on `process.user.additionalGids` in `synthesize_spec`,
the same OCI runtime-spec field `ocirun exec -g` (`0276`) already
populates for its own, narrower `exec`-time case — this is the first
place `ociman run`/`create` itself populates it.

Like `0276`'s own real, honestly-noted observation: whether this
actually takes visible effect (a real `setgroups(2)` succeeding)
depends on the same environment-dependent rootless `/proc/self/
setgroups` restriction `identity::apply_supplementary_groups` already
documents — not a gap in this feature, a real kernel restriction real
crun/runc are equally subject to.

## Verified

Integration (`tests/tests/ociman_run.rs`, four new tests):

- Multiple numeric `--group-add` values, including a duplicate,
  collapse to one sorted, deduplicated entry each in the real
  synthesized `config.json`'s own `process.user.additionalGids`.
- A named group resolves correctly against a seeded image's own
  `/etc/group`.
- An unresolvable name is a clear error naming the group.
- `--group-add keep-groups` is a clear, honest "not yet supported"
  error.

Unit (`bin/ociman/src/user_resolve.rs`, five new tests):
`resolve_group_add`'s own numeric/named/unknown/no-`/etc/group`-at-all
cases, mirroring the exact coverage `resolve`'s own existing tests
already established for user resolution.

Full workspace: `cargo build`/`test --workspace`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`python3 ci/guards.py`, `cargo deny check`, `ci/native-ci.sh`,
`ci/build-deb.sh`.

## Still ahead

`--group-add keep-groups` remains a real, separately-scoped candidate
needing dedicated `ocirun`-level annotation-driven runtime support.
</content>
</invoke>
