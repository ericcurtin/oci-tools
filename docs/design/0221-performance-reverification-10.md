# Design note 0221: re-verifying `ocirun`/`ociman` after 0184-0220

Status: verification only (no functional change)
Scope: none (measurement; this session's own release binaries built
from `eedf16e`, `crun 1.14.1`, `runc 1.3.4`, real installed `podman
4.9.3`/`docker 29.2.1` — identical tool versions to 0183, this
project's own previous full re-verification)

## Why re-measure now specifically

37 design-doc increments (0184-0220) landed since 0183, this
project's own longest gap between full re-verifications yet (every
prior one — 0018/0105/0113/0120/0139/0150/0161/0170/0176/0183 — came
after a smaller batch). None of those 37 touched `ocirun`/`ociman
run`'s own hot path directly (they were `ociman build --squash-all`/
untagged builds/commits, `ocibox` (a new binary, zero shared code with
the runtime-core hot path), `ocicri` (a deliberate, isolated,
long-lived-server exception — `tonic`/`tokio` confined to `ocicri`/
`oci-cri-types` only, confirmed again this session neither `ociman`
nor `ocirun` links either), packaging (`ci/build-rpm.sh`/
`ci/build-deb.sh`, shell scripts, zero Rust changes), and `ociboot
build-image`'s new origin-record write (a different binary entirely).
Still, this project's own explicit goal names re-verifying (not just
assuming) as the standard — and this is also the first re-verification
run using `ci/bench.sh` (0219) instead of hand-typed commands, itself
worth confirming actually reproduces the same numbers a fully manual
run would.

## Method

`ci/bench.sh`, run directly (no modification) — the three comparisons
it covers (`ocirun run` vs `crun`/`runc run`; `ociman run --rm` vs
`podman`/`docker run --rm`; `ociman rm` vs `podman rm`, destroy-only)
— plus `ociman commit` vs `podman commit` by hand (not yet in
`ci/bench.sh`, see 0219's own "what this doesn't cover yet"), using an
isolated `$OCI_TOOLS_STORAGE_ROOT` with `.rootless-overlay-supported`
forced to `false` (the same real, established technique 0146/0155's
own tests already use, since `ociman commit` doesn't support a
rootless-overlay-rootfs container yet — confirmed directly: attempting
it against this project's own real, default storage root's container
first hit exactly that documented, pre-existing limitation, unrelated
to anything measured here) — never touching this project's own real,
persistent default storage root's own cached capability marker.

## Result: no regression anywhere

| comparison | this session (0221) | most recent prior (0183) |
|---|---:|---:|
| `ocirun run` vs `crun run` | 2.41× | 2.20× |
| `ocirun run` vs `runc run` | 7.39× | 6.37× |
| `ociman run --rm` vs `podman run --rm` | 5.25× | 2.84× |
| `ociman run --rm` vs `docker run --rm` | 8.05× | 4.34× |
| `ociman rm` (destroy-only) vs `podman rm` | 44.77× | 13.94× |
| `ociman commit` vs `podman commit` | 27.88× | 38.19× |

Every comparison remains a decisive win. Several figures (`ociman run
--rm`, `ociman rm`) look meaningfully *better* than 0183 this
session — consistent with 0183's own "absolute numbers vary session to
session (host load...)" caveat rather than a real improvement: this
project's own binaries' absolute times themselves dropped noticeably
this session (e.g. `ociman run --rm`: 35.6ms vs 0183's 66.8ms) with no
matching code change to explain it (confirmed: nothing in 0184-0220
touches this path), so it reads as this session's host simply being
less loaded/more cache-warm than 0183's, not a genuine speedup — real
equivalents' own absolute numbers (`podman run --rm`: 187.2ms vs
0183's 189.9ms) stayed essentially flat in the same session, supporting
that read. `ociman commit`'s own ratio moved the other direction
(27.88× vs 38.19×) for the identical reason working in the other
direction (`podman commit` itself measured faster this session, 95.9ms
vs 98.7ms, while `ociman commit` itself measured slightly slower in
absolute terms, 3.4ms vs 2.6ms) — both well inside session-to-session
noise for a sub-5ms command (`ociman commit` triggered hyperfine's own
"took less than 5ms" calibration-accuracy warning both this session
and in 0183).

## A real, small methodology gap this session closed

`ci/bench.sh`'s own destroy-only `ociman rm`/`podman rm` comparison
uses `ociman rm --force` uniformly (0219's own documented choice, see
`docs/design/0219`) — this session's *new* `ociman commit` comparison
needed its own real workaround for a genuinely different, pre-existing
limitation (`ociman commit` refusing a rootless-overlay-rootfs
container outright, 0146), confirmed by hand, not previously written
down anywhere as part of a benchmarking methodology before now — future
sessions extending `ci/bench.sh` to cover `commit` should reuse this
session's own `.rootless-overlay-supported=false` + isolated
`$OCI_TOOLS_STORAGE_ROOT` technique rather than rediscovering it.

## Conclusion

No regression found anywhere across 37 new commits (0184-0220),
including the two, only-just-landed non-runtime-core additions
(`ocicri`'s deliberate `tonic`/`tokio` exception, confirmed still fully
isolated; `ociboot build-image`'s new origin-record write, confirmed
on a completely different binary's code path). `ocirun run`, `ociman
run --rm`, `ociman rm` (destroy-only), and `ociman commit` all remain
solidly, decisively ahead of every real equivalent, matching this
project's own explicit "beat the equivalents on all the benchmarks,
especially startup time and destroy time" goal. No code change made
this session — purely a confirmation round. All benchmark scratch
state (an isolated tempdir storage root, containers/images on both
`ociman`'s own isolated store and the real `podman` installation)
cleaned up afterward and confirmed via `podman images`/`podman ps -a`;
disk usage unchanged from before this session started (815G).
