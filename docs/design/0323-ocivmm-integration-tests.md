# Design note 0323: `ocivmm` integration test coverage

Status: implemented
Scope: `tests/tests/ocivmm_create_list_rm.rs` (new).

## Closing a real, total test-coverage gap

A broader survey of the codebase found `ocivmm` to be the most
neglected binary in terms of dedicated integration-test coverage:
every sibling binary (`ociman`, `ocirun`, `ocicri`, `ocibox`) has one
or more `tests/tests/<binary>_*.rs` files exercising the actual built
binary end to end; `ocivmm` had **zero** — its only coverage was the
small, inline `#[cfg(test)] mod tests` block at the bottom of
`bin/ocivmm/src/main.rs` (name validation, env merging, exit-status
parsing, systemd unit rendering) plus real end-to-end exercise via
`ci/vm-test`'s own x86_64-only dogfooding job. This note closes that
gap for exactly the parts of `ocivmm` that are both real and fully
offline-testable on this project's own aarch64 Linux dev/CI host.

## Why not `run`/`create`'s own full success path

`ocivmm run`'s actual VM boot needs a real KVM host (x86_64/Linux
only, `docs/design/0248`) or a real macOS/Apple Silicon HVF host
(`docs/design/0249`, itself still incomplete) — neither exists on
this host, so that half stays untested here, matching the exact same
reasoning `ci/vm-test`'s own dogfooding job already establishes (it
runs on a real x86_64 KVM-capable runner instead).

`create`'s own *success* path is genuinely heavier than any other
binary's own offline-seeded-image fixture: `provision_vm` requires the
source image to have a real distro package manager (`dnf`/`apt-get`)
and actually runs it inside a container to install a real kernel +
systemd — needing real network access to fetch real distro packages,
not just this project's own local mock registry. Attempting to
exercise that success path in an ordinary test run would mean a real,
possibly large network download plus a real package-manager run on
every test invocation — a poor fit for this project's own established
"fully offline, deterministic" test convention every other binary's
own fixture already follows.

## What this note actually covers

Everything else in `create`/`list`/`rm`/`cp` needs neither a real
hypervisor nor real network access:

- `create`'s own real, checked-directly upfront rejection
  (`provision_vm`'s `has_pkg_manager` gate): a plain busybox-based
  image (this project's own already-established offline test fixture)
  genuinely has neither `dnf` nor `apt-get`, so `create` fails clearly
  and — confirmed directly — leaves no half-created VM directory
  behind, the same real promise `ocibox create` already makes.
- `create` refusing an already-used name.
- `list`/`list --json`/`rm`/`rm --all` only ever read/write a
  directory tree and a small `vm.json` record — a directly-seeded
  record (a real, valid `VmRecord` shape a real `create` would have
  written, without needing a real, successfully-provisioned VM to get
  there) exercises the identical code path.
- `rm`'s own `--all`/explicit-name mutual exclusivity, unknown-name
  error, and path-traversal-name rejection (the same real security
  concern `ocibox rm`'s own identical charset validation guards
  against, `0206`).
- `cp`'s own "exactly one side must be `VMNAME:PATH`" validation and
  unknown-VM error — neither needs an actual loop-mount to exercise,
  since both are checked before any mounting happens at all.

## Verified

All 13 new tests pass. No product code changed — this note is purely
new test coverage, so "verified" here means the tests themselves
correctly exercise real, already-shipped `ocivmm` behavior (confirmed
by reading `bin/ocivmm/src/main.rs`'s own `create_vm`/`provision_vm`/
`cmd_list`/`cmd_rm`/`cmd_cp`/`parse_vm_path` directly before writing
each test, not guessed).

Regression: full `cargo test --workspace --locked`: 113 test result
blocks (up from 112 — one new test binary), 0 failures.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).

Performance: no product code changed; nothing to re-benchmark.

## Still ahead

`ocivmm run`'s own real VM-boot success path remains untested outside
`ci/vm-test`'s own real x86_64 KVM dogfooding job — a real, separately-
scoped candidate would be a lighter-weight, offline-testable success
path for `create` itself (e.g. a synthetic, already-kernel-and-
systemd-equipped test fixture image that could skip `provision_vm`'s
own real package-manager run entirely), which doesn't exist yet and
would need real design work of its own to build safely. The HVF/macOS
backend's own phase-4 `-EBUSY` blocker (`0249`) remains open,
unrelated to this note.
