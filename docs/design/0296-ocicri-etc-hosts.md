# Design note 0296: `ocicri` containers get a real `/etc/hosts`

Status: implemented
Scope: `crates/oci-runtime-core/src/etc_hosts.rs` (moved from
`bin/ociman/src/main.rs`), `crates/oci-runtime-core/src/lib.rs`,
`bin/ociman/src/main.rs`, `bin/ociman/src/build.rs`,
`bin/ocicri/src/bundle.rs`, `tests/tests/ocicri_container.rs`.

## Closing another real, explicitly-named gap

`bundle.rs`'s own module doc comment already named `/etc/hosts`
wiring directly as "deliberately out of scope for this slice" — the
same way it once named hostname wiring out of scope before `0292`
closed that one. Every CRI-managed container had no `/etc/hosts` at
all before this: `getent hosts $(hostname)`, `ping localhost`-by-name,
and any tool expecting a real `/etc/hosts` to exist would all fail
inside a `ocicri`-launched pod.

## Sharing `ociman run`'s own already-verified primitive

`ociman run`'s `write_etc_hosts`/`parse_extra_host` (`0147`) already
solve the *identical* problem for the *identical* reason: neither
project has any container-networking setup of its own at all (no
bridge/pasta/CNI), so a synthesized `/etc/hosts` mapping the
container's own identity name(s) to `127.0.0.1` — matching real
podman's own `--network=none` case exactly — is the correct, honest
answer for both. Rather than writing a second copy, both functions
moved out of `ociman`-private code into `oci_runtime_core::etc_hosts`
— the same "shared primitive moves to a shared crate the moment a
second, unrelated caller needs it" move `glob.rs` (`0295`),
`resolve_by_reference_or_id` (`0122`/`0213`), and `time`
(`oci-spec-types`) already went through.

Since `oci-runtime-core` has no `anyhow` dependency at all (every
other function in the crate returns plain `std::io::Result`, e.g.
`rootfs::populate_dev`), the moved functions were converted from
`anyhow::Result`/`anyhow::bail!`/`anyhow::ensure!` to
`std::io::Result`/`io::Error::other(...)` to match the crate's own
established convention exactly, rather than pulling in a new
dependency for one module. `ociman`'s own two call sites (`main.rs`'s
`run`, `build.rs`'s `RUN` steps) needed no other change: `anyhow::
Error` already implements `From<io::Error>`, so `?`/`.context(...)`
at each call site keep working unchanged.

## Wiring into `ocicri`

`prepare_in` (`bundle.rs`) now calls `write_etc_hosts` right after
every image layer is extracted into the container's own dedicated
`rootfs/`, using `cri.hostname` (the sandbox's own already-resolved
real hostname, `0292`) as the one `own_names` entry — no `--add-host`
equivalent yet, since real Kubernetes' own `PodSpec.HostAliases` (the
real source for extra host entries in a real cluster) is a genuinely
separate field this project's own `PodSandboxConfig` parsing doesn't
read yet, honestly left for its own later increment rather than
guessed at.

## Verified

Unit (`crates/oci-runtime-core/src/etc_hosts.rs`, 12 tests moved
verbatim from `ociman`): every pre-existing case — default entries,
no-identity build-container shape, `--add-host` precedence, the
`localhost`-override-suppresses-both-builtins rule, missing-`/etc`
creation, malformed-entry errors — passes byte-for-byte identically in
its new home.

Integration (`tests/tests/ocicri_container.rs`, one new test): a real
`CreateContainer` call's own extracted `rootfs/etc/hosts` contains
both the standard `127.0.0.1 localhost` line and the sandbox's own
real hostname mapped to `127.0.0.1`.

Regression: `ociman`'s own full `ociman_run.rs` (65 tests) and
`ociman_build.rs` (116 tests) suites, both of which exercise
`/etc/hosts` writing extensively, pass completely unmodified; all 28
pre-existing `ocicri` unit tests and 18 (17 pre-existing + 1 new)
`ocicri_container.rs` integration tests pass.

Full workspace: `cargo build`/`test --workspace` (111 test binaries),
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets --
-D warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh`.

## Still ahead

`resolv.conf` wiring and real Kubernetes `PodSpec.HostAliases`
(`ocicri`'s own `--add-host` equivalent) both remain, along with
joining the sandbox's own shared namespaces (`0233`) so every
container in one pod genuinely shares the identical `/etc/hosts`
rather than each independently synthesizing the same content.
