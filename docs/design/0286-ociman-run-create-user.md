# Design note 0286: `ociman run`/`create -u`/`--user`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_run.rs`.

## The one remaining CLI surface `resolve_user` never got

`ociman exec -u`/`--user` (0024/0028) and an image's own declared
`USER` config both already resolve through the exact same
`user_resolve::resolve` + `resolve_user` mapping-validation pair.
`ociman run`/`create --group-add` (0278) reuses the identical
`user_resolve.rs` infrastructure for a *different* CLI surface at the
exact same container-creation time `-u`/`--user` needs. Checked
directly against a real installed `podman run --help`: `-u, --user
string   Username or UID (format: <name|uid>[:<group|gid>])` — a very
high-frequency real-world flag (`-u $(id -u):$(id -g)` is one of the
most common `docker run`/`podman run` invocations in the wild) that
`ociman run`/`create` simply never had at all, despite every piece of
machinery it needs already existing and already being exercised by
three other features.

## Implementation

Pure plumbing, no new resolution logic: `RunArgs` gained one new
`user: Option<String>` field (`-u`/`--user`, shared by `run` and
`create` since both already share `RunArgs`); `synthesize_spec` gained
a matching `user: Option<&str>` parameter, resolved with the same
"override if given, else fall back to the image's own config" pattern
already used for `--workdir`/`--hostname`/`--entrypoint` in the same
function:

```rust
let effective_user = user.unwrap_or(container_config.user.as_deref().unwrap_or(""));
let (uid, gid) = resolve_user(rootfs, effective_user)?;
```

`resolve_user` itself needed no changes to support the new caller —
only the doc comment's own "an image's `USER`" framing was widened to
"a `USER` string, from any of resolve_user's now-three real callers".

## A real bug found and fixed along the way: `resolve_user` never checked the resolved *gid*

Writing an end-to-end test for `--user user:group` (`ociman-test
--user 0:staff`) surfaced a real, previously-unnoticed gap:
`resolve_user` only ever validated the resolved **uid** against this
rootless runtime's single-entry `uid_mappings` (container uid 0 →
host euid, see `Spec::into_rootless`) — it never validated the
resolved **gid** the identical way, even though `gid_mappings` is
exactly as narrow (container gid 0 → host egid, one entry, no
subordinate range). A resolved `(uid=0, gid=50)` — reachable even
*before* this change, via an image that declares `USER root:staff` —
sailed straight through `resolve_user`'s check and only failed much
later, deep inside `identity::apply`'s own `setresgid(2)`, as a bare,
confusing `Invalid argument (os error 22)` with no indication at all
of what was wrong or why.

Fixed by adding the exact same shape of check for `gid` that already
existed for `uid`, giving the identical clear, actionable error
instead of a raw `EINVAL` surfacing three layers away from where the
real problem (an unmappable gid) was actually decided. Verified this
was a real, previously-live gap (not hypothetical) by writing the
`--user 0:staff` test *before* the fix and watching it produce the
confusing raw `setresgid` failure, then confirming the fix turns it
into the same "cannot map... gid" error the uid case already gives.

## Verified

Integration (`tests/tests/ociman_run.rs`, five new tests):

- `--user root` overrides an image's own declared non-root `USER app`
  back to root, actually running successfully where the un-overridden
  image would have hit the usual non-root-uid rejection.
- `--user 1000` is rejected the exact same way a non-root image `USER`
  already is (same clear "cannot map" error, proving the CLI override
  goes through the identical validation path).
- `--user 0:root` (an explicit, mappable group half) resolves both the
  uid and gid correctly in the real synthesized spec.
- `--user 0:staff` (an explicit, *unmappable* non-zero group) is now
  rejected with a clear, gid-specific "cannot map" error — and
  explicitly asserts the raw `setresgid`/`Invalid argument` failure
  text is *not* what surfaces, locking in the bug fix above as a
  regression test.

Full workspace: `cargo build`/`test --workspace` (110 test binaries),
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets --
-D warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh`.

## Still ahead

Same underlying limitation `0276`/`0278` already document: only
container uid 0 and gid 0 are ever mappable in this project's current
single-uid/single-gid rootless setup — a real subordinate uid/gid
range via `/etc/subuid`/`/etc/subgid` would be needed to support
`--user` (or an image's own `USER`) resolving to any other uid/gid at
all. Not attempted here; a materially larger feature of its own.
</content>
