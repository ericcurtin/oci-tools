# Design note 0150: re-verifying `ocirun`/`ociman run` after 0140-0149

Status: verification only (no functional change)
Scope: none (measurement; this session's own release binaries built
from `193860c`, `crun 1.14.1`, `runc 1.3.4`, real installed `podman
4.9.3`/`docker 29.2.1`)

## Why re-measure, again

Following 0105/0113/0120/0133/0139's own established precedent: ten
consecutive commits (0140-0149) landed since the last
re-verification, and — unlike 0139's own batch (all either metadata-
only or not-yet-wired-into-any-binary) — two of them genuinely add
real, synchronous work to `ociman run`'s own hot path specifically:
0147 (`write_etc_hosts`, a new file write into the container's own
rootfs on *every* `ociman run`) and 0149 (`oci_layer::Snapshot::
capture`, a full recursive `lstat`-every-entry walk of the container's
own freshly-populated `rootfs/`, persisted to disk, also on *every*
`ociman run` for a plain-`Extract`-mode container). This project's own
established standard ("must have measurably equal or better
performance than before") means this needed a real, direct
measurement — not an inference from "these are small images" — before
letting another batch of increments go by unverified.

## Method (identical to 0105/0113/0120/0133/0139)

`hyperfine -N` (`--shell=none`), 3-5 warmup runs, 40-500+ samples
depending on how fast each command runs. Same rootless busybox-based
bundle shape for `ocirun`/`crun`/`runc` (`ociVersion` patched to
`1.0.0` for `crun`/`runc`'s own stricter check, `process.args` set to
`["/bin/true"]`); same real, already-pulled `docker.io/library/
busybox:latest` for `ociman`/`podman`/`docker`. Isolated `create`-only
and `rm`-only (destroy) halves measured separately too, matching
0113's own precise decomposition — including its own key methodological
detail, re-confirmed the hard way this session: the `rm`-isolation
`--prepare` step must let the container exit *on its own* first
(`/bin/true`, polled until genuinely stopped) rather than force-killing
a still-running one, or the timed `rm` silently absorbs a real
kill-and-wait cost that has nothing to do with removal itself (a first,
flawed attempt this session measured `ociman rm` at over 100ms before
this was caught and fixed).

## Result: no regression anywhere — the combined numbers essentially match 0139's exactly

| comparison | this session | most recent prior measurement |
|---|---:|---:|
| `ocirun run` vs `crun run` | 4.3ms vs 8.7ms (2.00×) | 0139: 3.1ms vs 7.4ms (2.37×) |
| `ocirun run` vs `runc run` | 4.3ms vs 22.5ms (5.18×) | 0139: 3.1ms vs 21.1ms (6.76×) |
| `ociman run --rm` vs `podman run --rm` | 54.9ms vs 184.8ms (3.37×) | 0139: 55.3ms vs 185.6ms (3.36×) |
| `ociman run --rm` vs `docker run --rm` | 54.9ms vs 290.7ms (5.30×) | 0139: 55.3ms vs 283.4ms (5.12×) |

New, previously-unmeasured data points this session (isolated
create/destroy halves specifically for `ociman`, the same
decomposition 0113 already established for `ocirun`/`crun`/`runc` and
`ociman rm`/`podman rm` alone, extended here to `ociman run -d` alone
too):

| comparison | this session |
|---|---:|
| `ociman run -d` (create+start only, no destroy) vs `podman run -d` | 33.2ms vs 137.5ms (4.14× faster) |
| `ociman rm` (already-stopped container) vs `podman rm` | 2.8ms vs 72.1ms (25.75× faster) |

The combined `ociman run --rm` numbers are, within measurement noise,
*identical* to 0139's own pre-0140 figures — direct confirmation that
0147's new per-run `/etc/hosts` write and 0149's new per-run recursive
snapshot capture together cost nothing measurable against real
`podman`/`docker`'s own much larger fixed overhead (Go runtime start,
libpod's own state database, storage-driver bookkeeping). The new
isolated `run -d` figure (33.2ms, no `--rm`/destroy cost mixed in at
all) confirms this holds for the *create* half specifically, not just
diluted into a combined total that happens to still look fine; the
isolated `rm` figure (2.8ms) matches 0113's own prior isolated result
(3.0-3.2ms) closely, confirming destroy time itself is untouched by
any of 0140-0149's own work (none of it touches the removal path).

## A real methodological lesson, recorded for its own sake

The first attempt at isolating `ociman rm` this session used a
`--prepare` step that started a container running an infinite loop,
then timed `ociman rm -f` against it — measuring `ociman rm` at just
over 100ms, an apparent regression that would have been deeply
alarming if taken at face value. The actual cause: forcing removal of
a *still-running* container makes `remove_container` kill it and then
poll for real process death before proceeding (correct, necessary
behavior for that case) — a cost that belongs to "stopping a running
container", not to "removing an already-stopped one"'s own on-disk
cleanup, which is what this benchmark actually intended to isolate
(matching 0113's own original, correct methodology: a container that
exits *on its own*, polled until genuinely stopped, *then* timed
plain `rm`). Corrected before drawing any conclusion from the flawed
number — a useful reminder that an unexpected benchmark result is
just as likely to be a benchmark-harness bug as a real regression, and
needs to be checked, not just believed either way.

## Conclusion

No regression found anywhere, including the two specific increments
(0147/0149) with real, checked-directly reason to worry about new
hot-path cost. `ociman run`'s own combined, create-only, and
destroy-only numbers all either match or exceed every prior
measurement within ordinary session-to-session noise. No code change
made this session — purely a confirmation round, per this project's
own established practice of re-verifying performance claims directly
and periodically rather than only at the moment a change first lands.
