# Design note 0126: `ociman login`/`ociman logout`

Status: implemented
Scope: `crates/oci-registry/src/credentials.rs` (`set`/`unset`,
`base64_encode`, `read_or_default`, `write_atomic` new); `crates/
oci-registry/Cargo.toml` (`tempfile` promoted from dev-dependency to a
real one); `bin/ociman/src/main.rs` (`Command::Login`/`Logout`,
`cmd_login`/`cmd_logout`, `default_auth_file_write_path`); `tests/
tests/ociman_login.rs` (new, 5 tests).

## A real, checked-directly gap

`oci_registry::Credentials` has always been able to *read* the real
`docker login`/`podman login` auth file format (bearer/basic auth
already worked for pulls) — but nothing ever *wrote* one. Real users
populate that file with `podman login`/`docker login`; `ociman` had no
equivalent at all, meaning a private registry was only reachable by
hand-editing JSON. A real, meaningful "drop-in replacement" gap, not a
cosmetic one.

## Real write-path priority, checked directly against real podman source, not assumed

`Credentials::load`'s own existing read-side `candidate_paths` checks
four locations (for read-time compatibility with whatever other tool's
file happens to already be there): `$REGISTRY_AUTH_FILE`, `$XDG_
RUNTIME_DIR/containers/auth.json`, `~/.config/containers/auth.json`,
`~/.docker/config.json`. Assuming `login` should just write to the
*first* of these would have been wrong: real podman's own `getPathToAuthWithOS`
(`~/git/container-libs/image/pkg/docker/config/config.go`, read
directly before writing any code) never falls back to either `$HOME`-
based path when *writing* by default — only `$REGISTRY_AUTH_FILE`,
then `$XDG_RUNTIME_DIR/containers/auth.json`, then a real, computed
`/run/user/<uid>/containers/auth.json` (not `$HOME`-based at all).
`default_auth_file_write_path` matches this exactly, reusing `oci_cli_
common::identity::effective_uid_gid` for the real uid.

## Deliberately no network verification — an honest, explicit scope narrowing

Real `podman login`/`docker login` both call a real `CheckAuth`/
`docker.CheckAuth` HTTP round trip against the target registry before
saving anything, refusing to save invalid credentials (checked
directly in `~/git/container-libs/common/pkg/auth/auth.go`). This
increment deliberately does not: getting this right for *every* real
registry's own token-scope conventions (Docker Hub, quay.io, GHCR,
GitLab all differ in what a registry-wide "just checking auth, not
pulling anything specific" token scope looks like) is real, separate
work with a real risk of silently getting some registry's own
convention wrong — a correctness bug that would be hard to notice
(rejecting valid credentials, or worse, "succeeding" with invalid
ones) versus the much simpler, always-correct alternative: write
exactly what a real pull/push would need, and let a wrong password
surface as a real, clear failure on the very next real registry
operation — no different from a user hand-editing the file incorrectly
today. `Command::Login`'s own doc comment states this explicitly.

## Implementation

`set`/`unset` operate on a generic `serde_json::Value` (not the typed
`AuthFile`/`AuthEntry` structs `Credentials::load` uses for reading)
specifically so any *other* top-level field a real file already has
(`credsStore`/`credHelpers`, common in a real `~/.docker/config.json`)
survives untouched — only `auths.<host>` is ever read or written.
Written atomically (temp file + rename) with real `0o600` permissions,
matching real podman's own `ioutils.AtomicWriteFile(path, data, 0o600)`
exactly, and the same atomic-write pattern this project's own
`oci_bls::grubenv::write` already established for its own equally
sensitive file.

`base64_encode` (the one piece of new logic needed to actually compute
an `auth` value, since `Credentials` never needed to *produce* one
before) is hand-rolled rather than adding a `base64` crate dependency —
matching this project's own established "minimal dependencies"
practice (e.g. `HEALTHCHECK`'s own hand-rolled duration parser, 0116) —
verified directly against a known-good fixture already present
elsewhere in this same file (`base64("user:pass") ==
"dXNlcjpwYXNz"`, the exact value every pre-existing `credentials.rs`
test already used) plus every real RFC 4648 padding case (0, 1, 2
trailing bytes).

`tempfile` moved from a dev-only dependency to a real one for `oci-
registry` (already a real dependency of several other crates in this
workspace, e.g. `ociman`'s own build scratch directories, 0121) — no
new dependency added to the workspace as a whole.

## Real, automated tests

Ten new unit tests in `oci-registry` (base64 correctness including
every padding case; `set` creating a file from scratch with real
`0o600` permissions; preserving unrelated entries/fields; overwriting
an existing host's own credentials; `unset` removing only the named
host; both `unset` no-op cases — never logged in, file doesn't exist
at all; and a full `set`-then-`Credentials::load` round trip proving
what gets written is exactly what the read side already knows how to
consume). Five new `ociman_login`/`ociman_logout` CLI integration
tests (real credentials written and readable back; JSON output shape;
two registries coexisting; logout removing only the named one; logout
of a never-logged-in registry being a real no-op). All pre-existing
`oci-registry`/`ociman` tests still pass unmodified. Full `cargo build
--workspace --locked`/`cargo test --workspace --locked` (2 clean
runs)/`cargo fmt --all --check`/`cargo clippy --workspace --all-targets
--locked -- -D warnings`/`python3 ci/guards.py`/`cargo deny check` all
clean.

## What this doesn't do yet

* No real registry verification before saving (see above) — a real,
  explicit scope narrowing, not an oversight.
* No `--password-stdin`, no interactive password prompt, no
  `--authfile`/`--compat-auth-file` override flags real `podman
  login` also supports — `--username`/`--password` only, matching
  this project's own established "narrow first increment" pattern.
  `$REGISTRY_AUTH_FILE` (already respected by both the read and write
  paths) covers the same real need `--authfile` would.
