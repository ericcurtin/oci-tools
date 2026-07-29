# Design note 0298: `ociman run`/`create --dns`/`--dns-search`/`--dns-option`, and a real `/etc/resolv.conf` during `ociman build` `RUN` steps

Status: implemented
Scope: `bin/ociman/src/main.rs`, `bin/ociman/src/build.rs`,
`tests/tests/ociman_run.rs`, `tests/tests/ociman_build.rs`.

## Closing `0297`'s own "still ahead"

`0297` gave `ocicri` a real `/etc/resolv.conf` and named a
`ociman run --dns`/`--dns-search`/`--dns-option` flag family as a
real, separately-scoped candidate. Real `podman run --dns`/
`--dns-search`/`--dns-option` (checked directly, `podman run --help`)
were the one remaining place a real, meaningful DNS gap existed:
`ociman run`/`create` never wrote `/etc/resolv.conf` at all.

## Confirming the shared primitive needs no changes at all

`0297`'s own `oci_runtime_core::resolv_conf::write_resolv_conf`
already implements exactly the right semantics for `ociman run` too,
confirmed by reading real podman's own richer
`~/git/container-libs/common/libnetwork/resolvconf/resolv.go`
directly: its `getDefaultResolvConf`'s own `hostNS` branch — taken
whenever the runtime spec has **no** `network` namespace at all (this
project's own `Spec::into_rootless` always strips it) — returns the
real host's own `/etc/resolv.conf` contents completely unfiltered,
with zero of the package's own loopback-nameserver-filtering/
namespace-aware-merge logic ever applying. In other words: for a
container sharing the host's real network namespace (this project's
own architecture, unconditionally), real podman's own much richer
algorithm degenerates to *exactly* the same simple "copy the host's
file verbatim, or use only the explicit values, never blended" rule
`0297` already ported from real cri-o's simpler `ParseDNSOptions`. No
new logic was needed — only new CLI surface and one more call site.

## Implementation

`RunArgs` (shared by `run`/`create`) gained `--dns`/`--dns-search`/
`--dns-option` (each repeatable), threaded into `prepare_container`'s
own `write_resolv_conf` call, added right after the existing
`write_etc_hosts` call at the exact same point in the setup sequence.

`ociman build`'s own `RUN` step setup also gained an **unconditional**
`write_resolv_conf(&rootfs_dir, &[], &[], &[])` call (always a
verbatim host copy, no `--dns`-equivalent build flag yet) — a real
`RUN apt-get update`/`RUN pip install ...`-style step genuinely needs
working DNS resolution to reach a real package registry, the same
real functional need `ociman run`'s own new default serves, and
before this change no `RUN` step had *any* `/etc/resolv.conf` at all
either.

## Verified

Manual, end-to-end: `ociman run --rm busybox cat /etc/resolv.conf`
with no `--dns` flags produces byte-for-byte the same content as this
host's own real `/etc/resolv.conf`; with
`--dns 1.1.1.1 --dns 8.8.8.8 --dns-search example.com --dns-option
ndots:5`, produces exactly `search example.com\nnameserver
1.1.1.1\nnameserver 8.8.8.8\noptions ndots:5\n`.

Integration (`tests/tests/ociman_run.rs`, two new tests;
`tests/tests/ociman_build.rs`, one new test): the same two end-to-end
cases for `run`, plus a real `RUN` step that captures its own
`/etc/resolv.conf` into the built image, verified against this host's
real file once the built image is actually run.

Regression: all 65 pre-existing `ociman_run.rs` tests and all 116
pre-existing `ociman_build.rs` tests pass unmodified.

Performance (this touches `ociman run`'s own hot path — every
container now does one extra file copy at startup): re-ran
`ci/bench.sh`'s `ociman run --rm` section after this change (34.7ms vs.
`0288`'s 30.7ms baseline, within ordinary session-to-session host-load
noise given the observed range of 17.6-51.5ms across repeated
samples) — still a decisive 5.44×/8.60× win over real `podman`/
`docker run --rm` respectively, the relative gap this project's own
goal actually cares about never at risk.

Full workspace: `cargo build`/`test --workspace` (111 test binaries),
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets --
-D warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh`.

## Still ahead

`ociman build --dns`/`--dns-search`/`--dns-option` (real podman/
buildah support these too; this slice only added the unconditional
host-copy default) remains a real, separately-scoped candidate.
