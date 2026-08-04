# Design note 0415: `ociman login --password-stdin`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_login.rs`,
`README.md`.

## What this closes

`ociman login` previously only ever accepted a password via the
plain, process-list-visible `--password`/`-p` flag. Real `podman
login`/`docker login` both also support `--password-stdin`, letting a
caller (a CI pipeline, a shell script piping a secret from a password
manager, ...) avoid ever putting the password on the command line at
all — a real, common security practice this project had no
equivalent of.

## Real, checked-directly confirmation

`~/git/podman/vendor/go.podman.io/common/pkg/auth/auth.go`'s own
`opts.StdinPassword` branch:

```go
if opts.StdinPassword {
    var stdinPasswordStrBuilder strings.Builder
    if opts.Password != "" {
        return errors.New("can't specify both --password-stdin and --password")
    }
    if opts.Username == "" {
        return errors.New("must provide --username with --password-stdin")
    }
    scanner := bufio.NewScanner(opts.Stdin)
    for scanner.Scan() {
        fmt.Fprint(&stdinPasswordStrBuilder, scanner.Text())
    }
    ...
    password = stdinPasswordStrBuilder.String()
}
```

Two things reproduced verbatim: the exact `"can't specify both
--password-stdin and --password"` error string, and a real, easily
missed quirk — `bufio.Scanner.Text()` strips each line's own trailing
newline and the loop never re-inserts a separator, so multiple stdin
lines are concatenated into one password with **no** separator at
all (verified with a dedicated test below, not assumed).

The `"must provide --username with --password-stdin"` check has no
real target in this project: `username` is already unconditionally
mandatory in `Command::Login`'s own arg struct (it always has been,
even before this change), so that branch could never trigger here —
called out explicitly as a deliberate divergence in the field's own
doc comment, not a silent omission.

## Implementation

- `Command::Login`: `password: String` becomes `password:
  Option<String>`; new `#[arg(long = "password-stdin")]
  password_stdin: bool`.
- `cmd_login` gains a `password_stdin: bool` parameter and, in order:
  rejects `password_stdin && password.is_some()` with the exact real
  error string above; when `password_stdin` is set, reads every line
  of stdin via `BufRead::lines()` (the same real precedent already
  established by `cmd_load`'s own `-` stdin path) and joins them with
  no separator (the quirk above); otherwise falls through to the
  existing `--password` value — and, since neither flag being given
  is a real, honest gap this project has no interactive-terminal-
  prompt fallback for (real podman prompts interactively; this
  project's `username`/password were already both mandatory before
  this change, matching its own established "no interactive prompt"
  convention), that case is now a new, clear
  `"either --password or --password-stdin is required"` error rather
  than a `clap`-level "missing required argument" one, since the flag
  itself is no longer marked required at the `clap` level.

## Tests

Four new tests in `tests/tests/ociman_login.rs` (via a new
`ociman_with_stdin` helper mirroring `ociman_import.rs`'s own
established stdin-piping pattern):
`login_password_stdin_writes_the_same_credentials_as_password`
(end-to-end, asserting the exact same `base64("myuser:mypass")` the
plain `--password` test already uses),
`login_password_stdin_concatenates_multiple_lines_with_no_separator`
(two stdin lines `"pass"`/`"word"` become one `base64("user:
password")` entry — proves the no-separator quirk is real, not
guessed), `login_rejects_both_password_and_password_stdin_together`,
`login_with_neither_password_nor_password_stdin_is_a_real_error`
(both asserting the exact real error strings above). All prior
`ociman_login.rs` tests continue to pass unmodified (12/12 total).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean, no diff), `cargo clippy --workspace --all-targets --locked --
-D warnings`, `cargo test --workspace --locked` (119 test-result
blocks, 0 failures on the run used for the commit decision),
`python3 ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh`
(one earlier attempt hit the known, pre-existing
`ocicri_container.rs` `ExecSync`/launcher-timing host-contention
flake — confirmed environmental: passed in isolation immediately
after, and a retry of the full script passed cleanly end to end),
`bash ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg -r` round
trip). This touches only `ociman login`'s own credential-file path,
not any hot path at all — no benchmark re-run needed.

## Deliberately still out of scope

Real podman's own "no username/password given, try existing stored
credentials, verify them against the real registry" fallback
(`docker.CheckAuth`, a real HTTP round trip) is not reproduced —
consistent with `oci_registry::credentials::set`'s own long-standing,
already-documented choice not to verify credentials against the real
registry at login time at all.
