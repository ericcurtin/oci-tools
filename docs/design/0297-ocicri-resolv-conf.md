# Design note 0297: `ocicri` containers get a real `/etc/resolv.conf`

Status: implemented
Scope: `crates/oci-runtime-core/src/resolv_conf.rs` (new),
`crates/oci-runtime-core/src/lib.rs`, `bin/ocicri/src/bundle.rs`,
`bin/ocicri/src/runtime_service.rs`, `tests/tests/ocicri_container.rs`.

## Closing `0296`'s own "still ahead"

`0296` closed `/etc/hosts` wiring for `ocicri` and explicitly named
`resolv.conf` wiring as the one remaining item in `bundle.rs`'s own
module doc comment. Every CRI-managed container had no
`/etc/resolv.conf` at all before this — real DNS resolution from
inside a container would fail entirely regardless of what a pod's own
`dnsPolicy`/nameservers were configured to be.

## Why this is meaningful, not just cosmetic, for this project specifically

Every container this project launches shares the calling process's
own real network namespace unmodified — `Spec::into_rootless` strips
the `network` namespace entry from the spec outright, and neither
`ocicri` nor `ociman` sets up any bridge/pasta/CNI of its own. This
means a real host nameserver genuinely *is* reachable from inside the
container exactly as it is from the host itself — copying the host's
own `/etc/resolv.conf` into the container is a real, functional fix,
not a decorative one.

## A new primitive, deliberately not real podman's own richer one

Real cri-o's own `ParseDNSOptions`
(`~/git/cri-o/internal/lib/sandbox/infra.go`), checked directly, is
simple: with no explicit `servers`/`searches`/`options` at all, copy
the real host's own `/etc/resolv.conf` verbatim; otherwise synthesize
one from scratch in a fixed order — `search` line first (if any), then
one `nameserver` line per server, then `options` last (if any).

Real podman's own `libnetwork/resolvconf` package
(`~/git/container-libs/common/libnetwork/resolvconf/resolv.go`) is
considerably richer — filtering loopback nameservers
(`127.0.0.1`/`127.0.0.53`), namespace-aware `KeepHost*` merge modes —
but that complexity exists specifically for a container with its
*own* private network namespace, a case this project has no
equivalent of at all. Implemented cri-o's simpler rule instead
(`oci_runtime_core::resolv_conf`, a new module — genuinely distinct
concern from `etc_hosts`, not merged into it), matching this project's
own actual "shares the host's real netns" architecture precisely
rather than porting logic for a scenario that can't occur here.

## Wiring

`CriProcessConfig` gained three new fields, `dns_servers`/
`dns_searches`/`dns_options`, resolved once at the `CreateContainer`
call site from `sandbox_config.dns_config` — a `None` `dns_config`
(kubelet's own common case for a pod with a default DNS policy, and
`crictl`'s own bare default) becomes all-empty, matching real cri-o's
own identical `if b.config.GetDnsConfig() == nil { b.config.DnsConfig
= &types.DNSConfig{} }` default exactly. `prepare_in` calls
`write_resolv_conf` right after `write_etc_hosts`, into the same
already-extracted `rootfs/`.

## Verified

Unit (`crates/oci-runtime-core/src/resolv_conf.rs`, 5 new tests): the
real synthesis order (search, then nameservers, then options), each
field independently omittable, and the real host-file-copy fallback
verified against this actual host's own `/etc/resolv.conf` content.

Integration (`tests/tests/ocicri_container.rs`, two new tests): an
explicit `dns_config` produces the exact expected synthesized content;
no `dns_config` at all copies the real host's own `/etc/resolv.conf`
byte-for-byte into the container's extracted rootfs.

Regression: all 28 pre-existing `ocicri` unit tests (two updated to
add the three new required `CriProcessConfig` fields) and all 18
pre-existing `ocicri_container.rs` integration tests pass unmodified.

Full workspace: `cargo build`/`test --workspace` (111 test binaries),
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets --
-D warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh`.

## Still ahead

Real Kubernetes' own `PodSpec.HostAliases` (`ocicri`'s own
`--add-host` equivalent, a separate `PodSandboxConfig` field this
project's own parsing doesn't read yet), and joining the sandbox's own
shared namespaces (`0233`) so every container in one pod genuinely
shares the identical `/etc/hosts`/`/etc/resolv.conf` rather than each
independently synthesizing the same content, both remain. A
`ociman run --dns`/`--dns-search`/`--dns-option` CLI flag family
(real podman has one) is a real, separately-scoped candidate noted
here but not attempted this round.
