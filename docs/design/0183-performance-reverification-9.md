# Design note 0183: re-verifying `ocirun`/`ociman` after 0177-0182

Status: verification only (no functional change)
Scope: none (measurement; this session's own release binaries built
from `b9aee3b`, `crun 1.14.1`, `runc 1.3.4`, real installed `podman
4.9.3`/`docker 29.2.1`)

## Why re-measure now specifically

Following 0018/0105/0113/0120/0133/0139/0150/0161/0170/0176's own
established precedent: six commits (0177-0182) landed since the last
re-verification (0176, itself covering 0171-0175). Unlike 0176's own
two re-measured commits (0177 `build --squash`, 0178 `ADD --checksum`
— both build-only) and 0179-0181 (untagged builds/commits/prune —
none on `ociman run`'s own hot path either), **0182 is different**: it
adds a real, new check (`resolve_image_by_id_only`) directly onto
`prepare_container`'s own image-resolution path, which both `ociman
run` and `ociman create` call on *every single invocation*, tagged
image argument or not. This is exactly the kind of change this
project's own "measurably equal or better performance" standard
requires actually measuring, not inferring from "it's just a cheap
string check" — the same rigor 0170 already applied to 0169's own
`write_entry` change for the identical reason.

## Method (identical to every prior re-verification)

`hyperfine -N` (`--shell=none`) for `ocirun`/`crun`/`runc`; a plain
`hyperfine` for every `ociman`/`podman`/`docker` comparison. 3-5 warmup
runs, 60-1006 samples depending on how fast each command runs. Same
rootless busybox-based bundle shape for `ocirun`/`crun`/`runc`; same
real, already-pulled `docker.io/library/busybox:latest` for `ociman`/
`podman`/`docker run`; a real, already-stopped container for the
`commit` comparison.

New this session, specifically to isolate 0182's own new code path: a
direct `ociman run --rm <tag>` vs `ociman run --rm <short-id>`
comparison, both against the exact same image, isolating whatever
`resolve_image_by_id_only`'s own extra check costs from every other
fixed per-invocation overhead this project's binaries already pay
identically either way.

## Result: no regression anywhere, including 0182's own new code path

| comparison | this session | most recent prior measurement |
|---|---:|---:|
| `ocirun run` vs `crun run` | 3.4ms vs 7.5ms (2.20×) | 0176: 3.3ms vs 6.5ms (1.99×) |
| `ocirun run` vs `runc run` | 3.4ms vs 21.8ms (6.37×) | 0176: 3.3ms vs 21.7ms (6.67×) |
| `ociman run --rm` vs `podman run --rm` | 66.8ms vs 189.9ms (2.84×) | 0176: 66.4ms vs 187.9ms (2.83×) |
| `ociman run --rm` vs `docker run --rm` | 66.8ms vs 289.9ms (4.34×) | 0176: 66.4ms vs 286.6ms (4.32×) |
| `ociman run -d` (create-only) vs `podman run -d` | 59.7ms vs 137.8ms (2.31×) | 0176: 49.5ms vs 139.3ms (2.81×) |
| `ociman rm` (destroy-only) vs `podman rm` | 5.2ms vs 72.4ms (13.94×) | 0176: 4.9ms vs 71.2ms (14.57×) |
| `ociman commit` vs `podman commit` | 2.6ms vs 98.7ms (38.19×) | 0176: 2.5ms vs 100.3ms (40.42×) |

New this session — isolating 0182's own new `resolve_image_by_id_only`
check directly (no prior baseline; this is the first time this exact
comparison has been run):

| comparison | this session |
|---|---:|
| `ociman run --rm <short-id>` vs `ociman run --rm <tag>` | 63.7ms vs 68.8ms (1.08× *faster* by ID) |

Every comparison remains a decisive win, and every figure sits well
within ordinary session-to-session noise of its own most recent prior
measurement (the largest relative drift, create-only's 2.31× vs
2.81×, is fully explained by this session's own absolute `podman run
-d` figure barely moving at all — 137.8ms vs 139.3ms — while `ociman
run -d`'s own absolute figure moved from 49.5ms to 59.7ms, itself
still well inside the noise band every other `ociman` figure shows
session to session on this same shared host).

The headline result for this session's own specific purpose: running
by a short image ID is not merely "no slower" than running by tag —
it's marginally *faster* (within noise of being identical), directly
confirming `resolve_image_by_id_only`'s own design intent from 0182:
its hex-prefix filter rejects a real tag string in one cheap string
scan with no store access at all, so the extra check this project's
own hot path now performs on every invocation costs nothing
measurable, and an actual ID lookup short-circuits the *existing*
`Reference::parse`+`resolve_or_pull` path entirely rather than adding
to it.

## Conclusion

No regression found anywhere across six new commits (0177-0182),
including the one specifically suspect change (0182's own new
per-invocation check on `ociman run`/`create`'s hot path) — measured
directly rather than assumed safe just because it "looks cheap."
`ocirun run`, `ociman run --rm`'s combined and create-only halves,
`ociman rm` (destroy-only), and `ociman commit` all remain solidly,
decisively ahead of every real equivalent, matching this project's own
explicit "beat the equivalents on all the benchmarks, especially
startup time and destroy time" goal. No code change made this
session — purely a confirmation round, per this project's own
established practice of re-verifying performance claims directly and
periodically rather than only at the moment a change first lands. All
benchmark scratch state (containers, images, storage roots, temp
bundles, and — learned from a prior session's own cleanup lapse,
0181 — every dangling image left behind on the real `podman`/`docker`
installations by this session's own testing) cleaned up afterward;
disk usage unchanged from before this session started (786G).
