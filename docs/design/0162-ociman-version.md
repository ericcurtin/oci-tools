# Design note 0162: `ociman version`

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Version` CLI variant and
dispatch; new `cmd_version`; new private `VersionReport`);
`tests/tests/ociman_version.rs` (2 new integration tests).

## What this does

`ociman version`: display detailed version information, matching real
`docker version`/`podman version` exactly for the "no remote server"
case — checked directly against a real, installed, rootless `podman
version` (no `--remote`), which itself shows only a `Client:` section,
no `Server:` one at all. This project has no daemon, so that's the
*only* case that applies here; there is no separate "server" half to
ever show.

Distinct from the pre-existing `ociman --version` (clap's own built-in
flag, unchanged): that one is a single-line, `<pkg_version> (git
<hash>)` string for quick reference; this is the fuller, structured
report real `docker`/`podman` both also expose as their own separate
`version` *subcommand* alongside their own identical `--version` flag.

## Deliberately narrower than real podman's own field set — and why

Real `podman version --format json`'s own `Client` object has
`Version`, `APIVersion`, `GoVersion`, `GitCommit`, `BuiltTime`/`Built`,
and `OsArch`/`Os`. This project only reports the three it has an
honest value for:

* `version` — this crate's own `CARGO_PKG_VERSION`.
* `git_commit` — the same `oci_cli_common::version::GIT_HASH` the
  existing `--version` flag already embeds.
* `os_arch` — `linux/<GOARCH-style-arch>`, reusing `oci_spec_types::
  image::Platform::host()` (already used for `FROM scratch`/image-
  platform resolution) rather than Rust's own `std::env::consts::ARCH`
  specifically so this matches real podman's own naming convention
  exactly (`arm64`, not `aarch64` — checked directly: a real `podman
  version` on this same host reports `linux/arm64`).

`GoVersion` has no honest equivalent at all (this is a Rust binary, not
Go) and `BuiltTime`/`Built` isn't currently recorded anywhere in this
project's own build process — both omitted entirely rather than filled
in with a fake or misleading placeholder value, matching this
project's own established convention (e.g. `ociman commit --message`'s
own doc comment on why it maps to a *different* real field instead of
inventing one) of never fabricating data just to match an upstream
tool's own field count.

## Real, automated tests

Two new integration tests in `tests/tests/ociman_version.rs`: plain-
text output has the exact `Client:`/`Version:`/`Git Commit:`/`OS/Arch:`
labels and never a `Server:` section; `--json` output has the same
three real fields with sane values (the workspace's own package
version, a non-empty git commit string, `linux/`-prefixed arch).

Full `cargo build --workspace --locked`/`cargo test --workspace
--locked` (2 clean runs)/`cargo fmt --all --check`/`cargo clippy
--workspace --all-targets --locked -- -D warnings`/`python3
ci/guards.py`/`cargo deny check`/`bash ci/native-ci.sh` all clean.

## What this doesn't do yet

* **`--format <go-template>`** — real podman's own arbitrary Go-
  template output format for `version` (beyond plain/`--json`); not
  implemented, matching this project's own established precedent of
  not implementing Go-template formatting anywhere else in its CLI
  surface either.
