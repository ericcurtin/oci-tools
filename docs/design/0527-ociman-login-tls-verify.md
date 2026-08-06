# Design note 0527: `ociman login --tls-verify`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_login.rs`.

## What this closes

Real `podman login --tls-verify` had no `ociman login` equivalent at
all -- a real CLI flag `ociman` would reject as unrecognized, unlike
`Command::Pull`/`Command::Search`/`Command::Run`, which already have
the identical flag.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/login.go:56`: `flags.BoolVar(&loginOptions.
  tlsVerify, "tls-verify", false, ...)`.
- `~/git/podman/cmd/podman/login.go:67-70`: `if cmd.Flags().Changed
  ("tls-verify") { skipTLS = types.NewOptionalBool(!loginOptions.
  tlsVerify) }`.
- `~/git/podman/cmd/podman/login.go:102-104`: `skipTLS` feeds
  `SystemContext.DockerInsecureSkipTLSVerify`, passed to `auth.Login`
  -- real `--tls-verify`'s only real effect is controlling whether
  the *real registry connection* `auth.Login` makes to verify the
  given credentials skips TLS certificate verification.

This project's own `cmd_login` never makes any such connection at
all -- `Command::Login`'s own doc comment already states it
"deliberately does **not** verify the credentials against the real
registry first." A genuine, faithful no-op, the same "nothing to
skip" reasoning class the `--force` sweep (`0521`) already
established, applied here to a flag whose real target is a network
call this project deliberately skips rather than a confirmation
prompt.

## Implementation

`tls_verify: bool` added to `Command::Login`, copying the exact same
`#[arg(long, default_value_t = true, num_args = 0..=1,
default_missing_value = "true", action = clap::ArgAction::Set)]`
pattern `Command::Pull`/`Command::Search`/`Command::Run` already use
verbatim, accepted and immediately discarded (`tls_verify: _`) at the
one dispatch site. `cmd_login`'s own function signature is untouched.

## Tests

One new integration test in `tests/tests/ociman_login.rs`:
`login_tls_verify_flag_is_accepted_and_behaves_identically` -- proven
both ways (`--tls-verify=true`/`--tls-verify=false`) writing the
identical real credentials to the auth file.

## A real, significant environmental finding along the way (not a code issue)

While verifying this change, two of `ociman_build.rs`'s own systemd-
scope-property-readback tests
(`build_cpu_period_quota_and_shares_set_the_real_systemd_scopes_own_
properties`, `build_cpuset_flags_set_the_real_systemd_scopes_own_
allowed_cpus_property`) failed *consistently*, even in isolation,
across many retries. Independently confirmed via an A/B test against
a clean `origin/main` checkout (`git stash`/`git stash pop`) that
this is **not** a regression from this change or any prior one --
the identical failure reproduced on unmodified `main`.

Root cause, traced directly: this host's own real `systemd --user`
session had accumulated **1148 leaked, orphaned scope units**
(`ocicri-*`/`ociman-*`/`oci-runtime-core-test-*.scope`, all
confirmed via `systemctl --user show --property=MainPID` to have no
live process at all) from many prior test runs across this project's
own long development history on this shared host, compounded by a
second, genuinely concurrent `opencode` session actively running its
own test suite at the same time (confirmed via `ps aux`; the scope
count was still climbing during observation). This volume of dead
units degraded real `systemd`/D-Bus property-query responsiveness
enough to make the two timing-sensitive read-back tests fail
reliably rather than only occasionally. Verified every scope had an
empty `MainPID` (no live process for any of the 1148) before cleaning
them up with `systemctl --user stop`; both tests then passed cleanly,
and the full suite (123 test-result blocks) ran clean afterward
(with the usual, previously-documented transient `ocicri_
container.rs` flakiness clearing on a single isolated rerun, same as
every other turn this session). No code change of any kind was
needed or made for this -- purely host-state hygiene, orthogonal to
`ociman login --tls-verify` itself.

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (123
test-result blocks, 0 failures -- clean after the scope cleanup
above), `python3 ci/guards.py` (clean), `cargo deny check` (clean),
`bash ci/native-ci.sh` (clean on the first attempt with
`RUST_TEST_THREADS=2`), `bash ci/build-deb.sh` (clean on the first
attempt, real `dpkg -i`/`--version`/`dpkg -r` round trip). Pure
CLI-parsing addition -- no hot path touched, no `ci/bench.sh` rerun
needed.
</content>
