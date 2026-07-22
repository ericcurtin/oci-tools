# Design note 0147: `ociman run --add-host` and a real, synthesized `/etc/hosts`

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Run`'s new `add_host`
field, `cmd_run`'s new call site, `parse_extra_host`, `write_etc_hosts`);
`bin/ociman/src/rootfs_setup.rs` (new `upper_dir` helper, shared with
`main.rs`'s pre-existing `resolve_container_root` — no behavior
change there); `tests/tests/ociman_run.rs` (4 new tests).

## A real, previously-missing default

Before this increment, `ociman run` never created a `/etc/hosts` file
at all — a base image without one baked in left a container with none,
diverging from every real container engine, all of which always
provide at least a minimal one (`127.0.0.1 localhost` at an absolute
minimum). Picked up as its own increment: not just `--add-host` (real
docker/podman's own flag for extra entries), but the always-present
default entries underneath it too.

## Checked directly against real podman's own `etchosts` package

The exact entry set and precedence rules are a direct port of real
podman's own `go.podman.io/common/libnetwork/etchosts` package
(`~/git/container-libs/common/libnetwork/etchosts/hosts.go`):
`parseExtraHosts` (`name[;name2...]:IP`, semicolon-separated names) →
`parse_extra_host`; `newHost`'s own three-tier entry order (user
entries first, then a base file — this project has no base-hosts-file
concept of its own, so this tier is simply empty — then the built-in
`containerIPs` entries) → `write_etc_hosts`.

One real subtlety, verified by direct experiment before writing any
test: `addEntriesIfNotExists` checks **every** built-in entry against
the *same*, user-entries-only name set, never updated as earlier
built-in entries get written. Concretely: a user `--add-host
localhost:9.9.9.9` suppresses **both** the `127.0.0.1 localhost` *and*
`::1 localhost` built-in lines entirely (not just the first one) —
confirmed with a throwaway manual test before committing to this
behavior in the real implementation, then locked in with its own
dedicated unit and integration test.

## No container networking of its own means every container is "`--network=none`"

This project sets up no container networking at all yet (no bridge,
no pasta, no CNI). Real podman's own `getHostsEntries` has a
dedicated branch for exactly this situation (`hasNetNone()`): the
container's own hostname/name map to `127.0.0.1`, the same address a
real `--network=none` podman container's own loopback-only view would
show. Every `ociman run` container's own synthesized `/etc/hosts`
always matches that specific real case, since it's the only one that
actually applies here.

## Writing before the container's own mount namespace exists — the same real discovery 0146 already made

`0146`'s own real, checked-directly discovery (a rootless-overlay-
rootfs container's own writes land in a private `upper/` directory,
genuinely distinct from the `rootfs/` directory a fresh container
starts with) applies here too, and is handled the same way: `cmd_run`
now computes a `write_root` — `rootfs_dir` for a plain-`Extract`
container, or `rootfs_setup::upper_dir(bundle_dir)` (new, `pub(crate)`,
also now used by `main.rs`'s own pre-existing `cmd_cp`-side
`resolve_container_root`, replacing its own independent
`.join("upper")` call — one shared source of truth for that path) for
an `Overlay` one — and writes `/etc/hosts` there directly, *before*
the container's own process (and its own private mount namespace)
ever exists. `write_etc_hosts` also creates `root/etc` first if
missing — a real, common case for a minimal base image (even a bare
`busybox` rootfs may ship no `/etc` directory at all, confirmed
directly while testing this same feature).

## Real, automated tests

Twelve new unit tests (`parse_extra_host`'s own parsing/validation
rules; `write_etc_hosts`'s own entry ordering, deduplication, the
`etc/`-directory-creation fallback, and the "user overriding
`localhost` suppresses both built-in localhost lines" subtlety found
above) plus four new real, end-to-end integration tests in
`tests/tests/ociman_run.rs`: a default `/etc/hosts` with no
`--add-host` at all; `--add-host` adding a real extra entry (semicolon-
separated names); the `localhost`-override suppression, verified via a
real running container's own `cat /etc/hosts` output rather than just
the unit-level string-building logic; and `host-gateway` being a
clear, real error. All pre-existing `ociman_run.rs` tests (44 total
now, including these 4) still pass unmodified — in particular,
`run_read_only_sets_root_readonly_in_the_real_spec` confirms
`--read-only` and the new host-side (pre-launch) `/etc/hosts` write
don't interact at all, since the write happens entirely before the
container's own process (and its own `root.readonly` spec setting)
exist.

Full `cargo build --workspace --locked`/`cargo test --workspace
--locked` (2 clean runs)/`cargo fmt --all --check`/`cargo clippy
--workspace --all-targets --locked -- -D warnings`/`python3
ci/guards.py`/`cargo deny check`/`bash ci/native-ci.sh` all clean.

## What this doesn't do yet

* **The `host-gateway` IP keyword** (real podman resolves it to a real
  host-reachable gateway address) — a clear, real error instead: this
  project has no container networking of its own at all yet, so there
  is no real address to resolve it to.
* **`ociman build --add-host`** — real `podman build` also accepts
  this flag (applied to every `RUN` step during the build); not
  implemented yet, `ociman run`-only for this first increment.
* **A base `/etc/hosts` file already baked into the image** — real
  podman's own `BaseHostsFile` config option (parsing an existing
  hosts file and merging into it, rather than always starting from a
  blank slate) isn't implemented; every container's own `/etc/hosts`
  is entirely synthesized from scratch, overwriting anything the base
  image itself may have shipped there.
