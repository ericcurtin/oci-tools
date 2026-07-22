# Design note 0163: `ociman info`

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Info` CLI variant and
dispatch; new `cmd_info`; new private `HostInfo`/`StoreInfo`/
`InfoReport`; `cmd_version` refactored into a new shared `version_
report()` helper so `cmd_info` can embed the identical real values
without duplicating how they're computed); `tests/tests/
ociman_info.rs` (3 new integration tests).

## What this does

`ociman info`: display system information, matching real `docker
info`/`podman info`'s own general `host`/`store`/(`version`) section
shape — but a deliberately much narrower first slice of real `podman
info`'s own huge report (checked directly against a real installed
`podman info`: host CPU utilization percentages, `buildah`/`conmon`/
`netavark`/`aardvark-dns`/`pasta`/`slirp4netns` package versions and
paths, storage-driver internals like `graphStatus`/`graphRootAllocated`,
registry/plugin lists, rootless UID/GID mapping tables, ...), since this
project has no daemon, no separate network stack (`--network=none`
only, see `docs/design/0147`), no pluggable storage-driver backend
(erofs/ext4/xfs per the README's own design pillar, not overlay2/
btrfs/zfs/vfs), and no `conmon`-equivalent supervisor process to report
on at all.

## Every field has an honest, directly-checkable real value — nothing fabricated

* `host.hostname`/`host.kernel` — a real `uname(2)` (`rustix::system::
  uname`), not a hardcoded/assumed string.
* `host.mem_total`/`host.mem_free` — a real `sysinfo(2)` (`rustix::
  system::sysinfo`), the exact same real source `oci_runtime_core::
  cgroups::memory_limit_bytes_clamped_to_physical_ram` already uses
  for physical RAM elsewhere (0145) — including that same call site's
  own already-checked-directly finding that `totalram`/`freeram` need
  no `mem_unit` scaling on any mainstream 64-bit Linux target.
* `host.cpus` — `std::thread::available_parallelism()`, a real,
  already-in-the-standard-library CPU count, not a hardcoded `nproc`
  shell-out.
* `host.cgroup_version` — always `"v2"`, since this project has no
  cgroup v1 support at all to ever report anything else (unlike real
  podman, which reports whichever the host's own cgroup hierarchy
  actually is).
* `host.rootless` — the real, current invocation's own effective UID
  (`oci_cli_common::identity::effective_uid_gid`), not an assumed
  constant — this project's own storage-root resolution already
  branches on the exact same real check (`/var/lib/oci-tools/storage`
  for root, `$XDG_DATA_HOME`-based otherwise), so this reports which
  one is actually in effect right now.
* `store.graph_root` — the real, resolved storage root (`oci_cli_
  common::storage::default_root()`), the exact same path `ociman
  pull`/`run`/`images` etc. already use.
* `store.containers`/`store.images` — real, live counts (`StateStore::
  list().len()`/`Store::list_images().len()`), not cached or
  estimated.
* `version` — the identical `VersionReport` `ociman version` (0162)
  itself reports, via the newly-shared `version_report()` helper.

## A real, deliberate structural simplification: one `graph_root`, not two

Real podman's own `store` section has separate `graphRoot`/`runRoot`
paths, since its own pluggable graph-driver storage backend genuinely
is a different subsystem from its own container runtime state. This
project has no such split — images and containers already share the
exact same single storage root (`containers` is just a subdirectory of
it, per `open_container_store`'s own existing doc comment) — so
`ociman info` reports the one, honestly-named `graph_root` rather than
two paths that would happen to always be identical anyway.

## Real, automated tests

Three new integration tests in `tests/tests/ociman_info.rs`: plain-text
output has the real expected section headers and a real, non-fabricated
storage root path; `--json` output has sane, real host values (a
non-empty hostname/kernel, a `linux/`-prefixed arch, at least one CPU, a
real positive memory total with free ≤ total, `cgroup_version: "v2"`);
and — the most meaningful check — `store.containers`/`store.images`
genuinely reflect real, *current* local storage state (0 before
anything exists, 1/1 after seeding one image and running one container
against it), not just a fixed, always-zero shape.

Full `cargo build --workspace --locked`/`cargo test --workspace
--locked` (2 clean runs)/`cargo fmt --all --check`/`cargo clippy
--workspace --all-targets --locked -- -D warnings`/`python3
ci/guards.py`/`cargo deny check`/`bash ci/native-ci.sh` all clean.

## What this doesn't do yet

* **`--format <go-template>`** — same already-established precedent as
  `ociman version` (0162): not implemented anywhere in this project's
  CLI surface.
* Everything real `podman info` reports that this project has no real
  analogue for at all (see above) — not a gap to "catch up" on later so
  much as a genuine reflection of this project's own much simpler,
  daemonless, single-storage-driver architecture.
