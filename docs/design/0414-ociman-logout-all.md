# Design note 0414: `ociman logout --all`

Status: implemented
Scope: `crates/oci-registry/src/credentials.rs`, `bin/ociman/src/
main.rs`, `tests/tests/ociman_login.rs`, `README.md`.

## What this closes

`ociman logout` previously only ever accepted exactly one, mandatory
`registry` argument. Real `podman logout`/`docker logout` both also
support `--all`, clearing every stored registry's own credentials in
one call — a real, common operator action (e.g. wiping a CI runner's
own credential state between jobs) this project had no equivalent of
at all.

## Real, checked-directly confirmation

`~/git/podman/vendor/go.podman.io/common/pkg/auth/auth.go`'s own
`Logout`:

```go
if opts.All {
    if len(args) != 0 {
        return errors.New("--all takes no arguments")
    }
    if err := config.RemoveAllAuthentication(systemContext); err != nil {
        return err
    }
    fmt.Fprintln(opts.Stdout, "Removed login credentials for all registries")
    return nil
}
...
case 0:
    if !opts.AcceptUnspecifiedRegistry {
        return errors.New("please provide a registry to log out from")
    }
```

Both exact error/success message strings above are reproduced
verbatim. `AcceptUnspecifiedRegistry` is real podman's own opt-in to a
`registries.conf`-style "default registry" fallback when no argument
is given at all — a concept this project has no equivalent of (same
scope narrowing already noted for `ociman search`'s own free-text
mode), so the "no registry and no `--all`" case here is always a
clear, immediate error, never a silent default-registry guess.

## Implementation

- `crates/oci-registry/src/credentials.rs`: new `unset_all(path)`,
  mirroring `unset`'s own existing shape exactly (same `read_or_
  default`/`write_atomic` reuse, same "missing file is a real no-op"
  tolerance) but clearing every key of `auths` instead of one named
  key, and — unlike `unset`, which never writes on a genuine miss —
  skipping the write entirely when `auths` was *already* empty, so a
  repeated `--all` logout on an already-clean file is a real no-op,
  not a gratuitous rewrite.
- `bin/ociman/src/main.rs`: `Command::Logout`'s own `registry` field
  becomes `Option<String>`; new `#[arg(long)] all: bool`. `cmd_logout`
  now takes `(registry: Option<&str>, all: bool, json: bool)` and
  reproduces the exact real validation order above: `all` together
  with a `registry` is the first, immediate error; no `registry` and
  no `all` is the second; otherwise dispatches to either `unset_all`
  or the existing `unset` path unchanged. `LogoutResult`'s own
  `registry` field becomes `Option<String>` (`None` under `--all`) to
  keep `--json` output honest about which case ran.

## Tests

Four new tests in `tests/tests/ociman_login.rs`:
`logout_all_removes_every_registry_at_once` (two real logins, then
`--all`, asserting the exact success message and an emptied `auths`
object on disk), `logout_all_together_with_a_registry_is_a_real_
error`, `logout_with_neither_a_registry_nor_all_is_a_real_error` (both
asserting the exact real error strings above), plus three new unit
tests in `credentials.rs` itself (`unset_all_clears_every_entry_but_
preserves_other_top_level_fields`, `unset_all_of_an_already_empty_
auth_file_is_a_real_no_op` — asserting the file's own mtime is
untouched, proving the no-op is real and not just a same-content
rewrite, `unset_all_of_a_missing_file_is_a_real_no_op_not_an_error`).
All prior `ociman_login.rs`/`credentials.rs` tests continue to pass
unmodified.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean, no diff), `cargo clippy --workspace --all-targets --locked --
-D warnings`, `cargo test --workspace --locked` (119 test-result
blocks, 0 failures on a clean run — one earlier run hit the known,
pre-existing `ocicri_container.rs` `ExecSync` host-contention flake,
confirmed environmental: passed in isolation immediately after, and a
concurrent `opencode` session plus a long-running CPU-spinning process
were both independently confirmed running on this same host at the
time), `python3 ci/guards.py`, `cargo deny check`, `bash ci/
native-ci.sh`, `bash ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip). This touches only `ociman login`/`logout`'s own
credential-file path, not any hot path at all — no benchmark re-run
needed.

## Deliberately still out of scope

`ociman login --password-stdin` remains unimplemented — a real,
separate, similarly-small gap (`~/git/podman/vendor/go.podman.io/
common/pkg/auth/auth.go`'s own `opts.StdinPassword` branch) identified
alongside this one but left for a future increment to keep this one
focused on a single behavior.
