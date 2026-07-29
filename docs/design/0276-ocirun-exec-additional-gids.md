# Design note 0276: `ocirun exec -g`/`--additional-gids`

Status: implemented
Scope: `bin/ocirun/src/main.rs`, `tests/tests/ocirun_exec.rs`.

## Closing a real, low-risk CLI-compatibility gap

Real `runc exec --help` (checked directly against a real installed
`runc`) has `-g`/`--additional-gids <GID>` — repeatable, each
occurrence appending one supplementary GID to the exec'd process
(`~/git/runc/exec.go`'s own `append(p.User.AdditionalGids, ...)`).
`ocirun exec` had no equivalent flag at all, even though the
underlying primitive it needs — `oci_spec_types::runtime::User::
additional_gids` and `oci_runtime_core::identity::apply_supplementary_
groups` — already existed and is already wired into `oci_runtime_core::
exec::ExecRequest.user`. This was pure CLI plumbing: parse the flag,
extend the effective user's own `additional_gids` before building the
`ExecRequest`, nothing else needed changing anywhere.

Real `crun exec --help` has no equivalent flag at all (only `-u`/
`--user`'s single primary group) — checked directly, confirming this
is specifically a `runc`-compatibility gap, not a `crun` one.

Real `podman exec --help` was also checked directly and, like `crun`,
has no equivalent flag either — only `-u`/`--user`. So this same idea
does **not** apply to `ociman exec` (an earlier research pass had
assumed it would; checking directly first avoided adding a flag to
`ociman` that wouldn't actually match any real tool's own CLI at all).
Real podman's own supplementary-group support instead lives on `podman
run`/`create --group-add` (a different flag, at container-creation
time rather than exec time, with richer name-or-GID/`keep-groups`
semantics) — a real, separately-scoped candidate noted below.

## Semantics, checked directly against `~/git/runc/exec.go`

- Repeatable: each `-g <GID>` occurrence appends one GID.
- Numeric only (matching `ocirun exec --user`'s own existing numeric-
  only convention — named-group resolution is a higher-level-tool
  concern, same reasoning `ocirun exec --user`'s own doc comment
  already gives for why `ociman exec --user` supports names and this
  doesn't).
- **Appends to**, never replaces, the container's own already-declared
  supplementary groups (`p.User.AdditionalGids = append(...)`) — the
  same append, not overwrite, semantics implemented here.
- Composes with `--user`: independent flags, both applied to the same
  effective `User` before the exec'd process is spawned.

## Verified

Integration (`tests/tests/ocirun_exec.rs`, one new test): `-g 100 -g
200` combined with `--user 0:0` is accepted and the exec succeeds.
Deliberately does **not** assert on the resulting group list itself:
this sandboxed rootless test environment's own `/proc/self/setgroups`
already reads `deny` (confirmed directly, `cat /proc/self/setgroups`),
a real, environment-dependent unprivileged-user-namespace kernel
restriction real `runc`/`crun` are equally subject to (see
`identity::apply_supplementary_groups`'s own doc comment) — not
something either tool, or this one, can bypass, and not a real gap in
this feature's own implementation.

Full workspace: `cargo build`/`test --workspace`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`python3 ci/guards.py`, `cargo deny check`, `ci/native-ci.sh`,
`ci/build-deb.sh`.

## Still ahead

`ociman run`/`create --group-add` (real podman's own actual
supplementary-group flag) is implemented in `0278`, apart from its own
`keep-groups` special value, deliberately left for its own future
increment (needs annotation-driven runtime-level support).
</content>
</invoke>
