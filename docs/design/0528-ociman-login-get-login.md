# Design note 0528: `ociman login --get-login`

Status: implemented
Scope: `crates/oci-registry/src/credentials.rs`,
`bin/ociman/src/main.rs`, `tests/tests/ociman_login.rs`.

## What this closes

Real `podman login`/`docker login` both support `--get-login`, which
prints the username already logged in to a registry instead of
logging in. `ociman login` had no equivalent at all — a real CLI flag
it would reject as unrecognized — and, more fundamentally, no way to
even ask "what's the current login?" at all, since `username` was
unconditionally mandatory on every invocation.

## Real, checked-directly confirmation

- `~/git/container-libs/common/pkg/auth/cli.go:56`: `fs.BoolVar
  (&flags.GetLoginSet, "get-login", false, "Return the current login
  user for the registry")` — an ordinary, independent boolean flag,
  no `RequiredIfPresent`/`conflicts_with`-style declaration anywhere
  alongside it.
- `~/git/container-libs/common/pkg/auth/cli.go:54`: `fs.StringVarP
  (&flags.Username, "username", "u", "", ...)` — real `--username`
  has **no** default-empty-is-invalid check at the flag level at all;
  it's just an ordinary, optional string defaulting to `""`.
- `~/git/container-libs/common/pkg/auth/auth.go:161-167`
  (`auth.Login`):
  ```go
  if opts.GetLoginSet {
      if authConfig.Username == "" {
          return fmt.Errorf("not logged into %s", key)
      }
      fmt.Fprintf(opts.Stdout, "%s\n", authConfig.Username)
      return nil
  }
  ```
  This is the *first* thing `Login` does after loading credentials
  for the resolved registry key — strictly before the `IdentityToken`
  check, the `--password-stdin` branch, and `getUserAndPass`'s own
  interactive-prompt fallback. So real `--get-login` combined with
  `--username`/`--password`/`--password-stdin` never errors on the
  combination and never touches any of them either — they're simply
  never reached. No mutual-exclusivity check exists anywhere in real
  podman's own source for this; verified there is none by reading the
  whole of both `cli.go` and every branch in `Login` above line 161.
- `~/git/container-libs/image/pkg/docker/config/config.go:877-902`
  (`decodeDockerAuth`): `authConfig.Username` above comes from
  base64-decoding the stored `auth` field and cutting on the first
  `:` (`base64.StdEncoding.DecodeString` + `strings.Cut(decoded,
  ":")`). An invalid split or a decode failure both yield an empty
  `Username`, which `auth.Login`'s own check above then reports as
  `"not logged into %s"` — the exact same "not logged in" outcome as
  a genuinely missing entry, not a separate error path.
- `~/git/container-libs/common/pkg/auth/auth.go:287-312`
  (`getUserAndPass`): confirms real podman's own only fallback for a
  missing `--username` outside `--get-login` is an *interactive*
  terminal prompt (`cannot prompt for username without stdin` if
  `opts.Stdin == nil`, which in practice never happens since
  `loginOptions.Stdin = os.Stdin` is always set) — this project has
  no interactive-prompt architecture anywhere at all (already true
  before this change, e.g. `--password`/`--password-stdin` are both
  already hard-required with no prompt fallback), so a missing
  `--username` outside `--get-login` is simply a hard, immediate
  error here instead.

## Implementation

`crates/oci-registry/src/credentials.rs`:
- New private `Credentials::raw_auth_for` factoring out the one
  lookup (including the Docker-Hub-legacy-key fallback)
  `basic_auth_header` and the new `username_for` both now share.
- New `pub fn username_for(&self, registry_host: &str) -> Option
  <String>`: looks up the raw, still-base64 `auth` entry, decodes it,
  splits on the first `:`, and returns the username half — `None`
  for a missing entry, invalid base64, non-UTF-8 decoded bytes, no
  `:` in the decoded value, or an empty username (folding every one
  of real `decodeDockerAuth`'s own distinct failure modes into the
  single `None` its one real caller already treats identically, per
  the "not logged into" confirmation above).
- New private `base64_decode(&str) -> Option<Vec<u8>>`, the exact
  inverse of the module's own existing hand-rolled `base64_encode`
  (same `ALPHABET` constant, same reason to hand-roll rather than add
  a dependency — see this module's own doc comment): validates
  length is a multiple of 4, that `=` padding (0/1/2 characters) only
  ever appears trailing, and that every non-padding character is in
  the alphabet, before decoding each 4-character group into 1-3
  bytes.

`bin/ociman/src/main.rs`:
- `Command::Login::username` changes from `String` to `Option
  <String>` (still `#[arg(short, long)]`, so `-u`/`--username` are
  unchanged for every existing caller that still gives it
  explicitly).
- New `Command::Login::get_login: bool`, `#[arg(long = "get-login")]`.
- `cmd_login` gains a `get_login: bool` parameter and a `username:
  Option<&str>` (was `&str`). When `get_login` is set: loads
  `Credentials` via the existing shared `Credentials::load()` (the
  same multi-candidate-path read logic `ociman pull`/`build` already
  use — a deliberate, real difference from the single deterministic
  path `cmd_login`'s own *write* side uses, matching how a read
  should honor every location credentials could already live in,
  the same way real `config.GetCredentials` does), calls the new
  `username_for`, and either prints the username (or the `{registry,
  username}` pair under `--json`) or bails with the exact real
  `"not logged into {registry}"` wording — then returns immediately,
  before ever validating `username`/`password`/`password_stdin` at
  all (matching real `auth.Login`'s own early return exactly).
  Outside `get_login`, a `None` username is now a real,
  `anyhow::bail!`-based hard error (`"--username is required unless
  --get-login is given"`) rather than a value clap could no longer
  guarantee is present.

## Tests

Five new integration tests in `tests/tests/ociman_login.rs`:
- `login_get_login_prints_the_username_already_logged_in`
- `login_get_login_on_a_registry_never_logged_into_is_a_real_error`
- `login_get_login_ignores_username_and_password_when_given_alongside`
- `login_without_username_and_without_get_login_is_a_real_error`
- `login_get_login_json_reports_the_registry_and_username`

Plus six new unit tests in `crates/oci-registry/src/credentials.rs`:
`base64_decode_is_the_exact_inverse_of_base64_encode_for_every_padding_case`,
`base64_decode_rejects_input_base64_encode_could_never_have_produced`,
`username_for_decodes_the_username_half_of_a_real_stored_auth_entry`,
`username_for_is_none_for_a_registry_never_logged_into`,
`username_for_follows_the_same_docker_hub_legacy_key_fallback_as_basic_auth_header`,
`username_for_is_none_for_a_decoded_value_with_no_colon_at_all`.

Independently re-verified end to end (not just accepting the
implementation as delivered): re-checked every upstream citation
above directly against `~/git/container-libs`, confirmed `base64_
decode` is a genuine, correct inverse of `base64_encode` (unit tests
plus a manual cross-check against a real `base64` invocation for
several padding cases), and exercised the CLI by hand (`--get-login`
on an empty auth file, after a real login, combined with ignored
`--username`/`--password`, `--json` output, and the new hard error
for a missing `--username`). One real, if minor, side effect from
that manual exercise: an earlier turn's own manual `--tls-verify`
testing (`0527`) had left a real `quay.io` credential in this host's
own actual `$XDG_RUNTIME_DIR/containers/auth.json` (`Credentials::
load()`'s own multi-path merge means a `REGISTRY_AUTH_FILE` override
alone doesn't shadow it) -- cleaned up via a real `ociman logout
quay.io`, not a manual file edit.

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test -p oci-registry --lib
credentials` (22/22), `cargo test --workspace --locked` (123
test-result blocks, 0 failures -- the documented transient
`ocicri_container.rs` flakiness under this host's own persistent CPU
contention (plus a second, genuinely concurrent `opencode` session)
showed up once, on yet another different test in that same file
this time (`exec_sync_runs_commands_in_a_running_container`),
confirmed transient by rerunning in isolation; found and cleaned up
261 more leaked, orphaned systemd scopes accumulated during this
run's own execution, the same real host-hygiene issue `0527` first
diagnosed and fixed, before a fully clean full-suite rerun), `python3
ci/guards.py` (clean), `cargo deny check` (clean), `bash
ci/native-ci.sh` (clean on the first attempt with
`RUST_TEST_THREADS=2`), `bash ci/build-deb.sh` (clean on the first
attempt, real `dpkg -i`/`--version`/`dpkg -r` round trip). Pure
CLI-parsing-and-lookup addition -- no hot path touched, no
`ci/bench.sh` rerun needed.

## Deliberately still out of scope

`--secret` (`podman login`'s own `secret` flag, reading a password
from a `podman secret` — this project has no secrets subsystem of
any kind) and `--authfile`/`--compat-auth-file`/`--cert-dir` (this
project's own already-established single-env-var-driven auth file
resolution has no per-invocation path override at all, a pre-existing
gap unrelated to this increment) remain unimplemented, matching every
prior `ociman login`/`logout` design note's own identical scope.
