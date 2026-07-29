# Design note 0299: `ociman build --dns`/`--dns-search`/`--dns-option`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `bin/ociman/src/build.rs`,
`tests/tests/ociman_build.rs`.

## Closing `0298`'s own "still ahead"

`0298` gave `ociman build`'s own `RUN` steps an unconditional, real
`/etc/resolv.conf` (always a verbatim host copy) and named
`ociman build --dns`/`--dns-search`/`--dns-option` as the one
remaining real gap: real `podman build`/`buildah bud` support the
identical three flags `ociman run`/`create` already got in `0298`,
but `ociman build` had no way to override the default at all.

## The real `--dns none` special case

Checked directly against real buildah source
(`~/git/podman/vendor/go.podman.io/buildah/run_linux.go`):

```go
if len(b.CommonBuildOpts.DNSServers) != 1 ||
    strings.ToLower(b.CommonBuildOpts.DNSServers[0]) != "none"
```

i.e. `/etc/resolv.conf` creation for a `RUN` step is skipped **only**
when exactly one `--dns` value was given and it is `none`
(case-insensitive); any other combination — zero values, one or more
non-`"none"` values, or `"none"` mixed with anything else — still
creates the file normally. Ported verbatim as:

```rust
let dns_disabled = dns.len() == 1 && dns[0].eq_ignore_ascii_case("none");
```

## No changes needed to the shared primitive

`oci_runtime_core::resolv_conf::write_resolv_conf` (from `0297`,
already reused unchanged by `0298`) is reused here for a third time
with zero changes: `ociman build`'s own `RUN`-step setup now calls it
conditionally, passing the new `dns`/`dns_search`/`dns_option` slices
straight through instead of always passing empty slices.

## Implementation

Three repeatable flags added to `Command::Build` (`--dns`,
`--dns-search`, `--dns-option`), threaded through `cmd_build`'s and
its internal `build_stage` helper's already-long parameter lists
(both already carry `#[allow(clippy::too_many_arguments)]`; adding 3
more follows the same established convention rather than introducing
an options struct, out of scope for this slice). The one `RUN`-step
call site (right after the existing `write_etc_hosts` call) now reads:

```rust
let dns_disabled = dns.len() == 1 && dns[0].eq_ignore_ascii_case("none");
if !dns_disabled {
    oci_runtime_core::resolv_conf::write_resolv_conf(&rootfs_dir, dns, dns_search, dns_option)
        .context("writing /etc/resolv.conf for the build container")?;
}
```

## A real, pre-existing build-cache limitation, confirmed not new

While testing manually, builds with different `--dns` flags but an
otherwise-identical `Containerfile` hit the *same* cached layer (the
`RUN` step's own cache key is the instruction text plus the base
image state, not any of the ambient per-build flags such as
`--dns`/`--add-host`). This project's own build cache has never
accounted for `--add-host` either (checked: no existing code or test
references this interaction), so this is a real, faithful,
pre-existing limitation shared with `--add-host`, not something newly
introduced here, and out of scope to fix in this slice. `--no-cache`
(already implemented) always bypasses it, confirmed by manual test.

## Verified

Manual, end-to-end (with `--no-cache` to avoid the cache limitation
above): a `RUN cat /etc/resolv.conf > /captured.txt` step with
`--dns 1.1.1.1 --dns-search example.com --dns-option ndots:5`
produces exactly `search example.com\nnameserver 1.1.1.1\noptions
ndots:5\n`; with `--dns none`, a `RUN test -e /etc/resolv.conf`-style
step confirms the file is genuinely absent.

Integration (`tests/tests/ociman_build.rs`, two new tests, each using
its own fresh storage directory so no cross-test cache interaction
applies): `build_dns_flags_synthesize_a_real_resolv_conf_for_run_steps`
asserts the exact synthesized content for
`--dns 1.1.1.1 --dns 8.8.8.8 --dns-search example.com --dns-option
ndots:5`; `build_dns_none_skips_writing_resolv_conf_for_run_steps_entirely`
asserts `--dns none` leaves `/etc/resolv.conf` absent during the
`RUN` step (confirmed via the `RUN` step's own live stdout, which
`ociman build` streams through exactly like real `podman build`).

Regression: all 117 pre-existing `ociman_build.rs` tests pass
unmodified; full `cargo test --workspace --locked` (0 failures across
every test binary).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).

Performance: `ociman build` is not part of the hot-path
(container-launch) benchmarks tracked in `docs/benchmarks.md`; this
change adds no work to any already-benchmarked path (`ociman run`'s
own `--dns` handling from `0298` is unchanged), so no re-benchmark was
needed.

## Still ahead

No further DNS-related gap is known between `ociman run`/`create`/
`build` and real `podman run`/`create`/`build`. `ocicri` (`0297`) does
not take per-sandbox `--dns`-equivalent CLI flags at all — that
surface is Kubernetes' own `DNSConfig` on the pod spec, already
wired since `0297`.
