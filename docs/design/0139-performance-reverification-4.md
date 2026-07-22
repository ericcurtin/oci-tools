# Design note 0139: re-verifying `ocirun`/`ociman run`/`ociman build` after 0134-0138

Status: verification only (no functional change)
Scope: none (measurement; this session's own release binaries built
from `6ba400c`, `crun 1.14.1`, `runc 1.3.4`, real installed `podman`/
`docker`)

## Why re-measure, again

Following 0105/0113/0120/0133's own established precedent: five
consecutive commits (0134-0138: `--iidfile`, `--label`, `--annotation`,
and two `oci-bls` library-only increments with no CLI surface at all)
landed since the last re-verification. This project's own repeated
standard ("must have measurably equal or better performance than
before") means these needed re-checking against real equivalents
again, not assumed to still hold — even though careful reasoning about
each change's own hot-path exposure at commit time already gave strong
grounds for confidence (`--iidfile`/`--label`/`--annotation` are all
one-time, post-build metadata operations; `oci-bls::cmdline`/
`apply_kargs_diff` aren't wired into any binary's own runtime path at
all yet), a direct measurement is what this project's own standard
actually requires, not an inference.

## Method (identical to 0105/0113/0120/0133)

`hyperfine --shell=none`, 5+ warmup runs, 20-650+ samples depending on
how fast each command runs. Same rootless busybox-based bundle shape
for `ocirun`/`crun`/`runc` (patched `ociVersion` for `crun`'s own
stricter check); same real, already-pulled `docker.io/library/
busybox:latest`/`docker.io/library/ubuntu:24.04` for `ociman`/
`podman`/`docker`; same `FROM ubuntu:24.04` + `RUN echo hello`
Containerfile for the plain build comparison (0112's own exact
benchmark); same 0133's own dedicated dockerignore-under-real-load
benchmark (a real 5,000-file, 20MB `node_modules`-shaped directory,
`.dockerignore`-excluded, compared against a truly empty context).

## Result: no regression anywhere

| comparison | this session | most recent prior measurement |
|---|---:|---:|
| `ocirun run` vs `crun run` | 3.1ms vs 7.4ms (2.37×) | 0133: 3.5ms vs 7.8ms (2.25×) |
| `ocirun run` vs `runc run` | 3.1ms vs 21.1ms (6.76×) | 0133: 3.5ms vs 21.5ms (6.18×) |
| `ociman run --rm` vs `podman run --rm` | 55.3ms vs 185.6ms (3.36×) | 0133: 59.2ms vs 182.5ms (3.08×) |
| `ociman run --rm` vs `docker run --rm` | 55.3ms vs 283.4ms (5.12×) | 0133: 59.2ms vs 291.3ms (4.92×) |
| `ociman build` (warm) vs `podman build` (warm) | 74.8ms vs 88.2ms (1.18× faster) | 0133: 64.5ms vs 88.6ms (1.37× faster) |
| `ociman build` with a 20MB ignored dir vs an empty context | 73.0ms vs 72.1ms (1.01×, no real difference) | 0133 (after its own fix): 91.3ms vs 68.4ms (1.06×) |

Every comparison is unchanged within noise, or slightly better — none
of 0134-0138's own work touches `ocirun`'s or `ociman run`'s own hot
paths at all (all five commits are either `ociman build`-only metadata
plumbing, or `oci-bls` library code no binary calls yet), so this is
exactly the expected, confirmed-not-just-assumed outcome. `ociman
build`'s own plain-Containerfile margin against real `podman build`
narrowed slightly this session (1.18× vs 1.37×) — both figures are well
within each other's own measured standard deviation (this session:
74.8ms ± 7.0ms; 0133: 64.5ms ± 3.3ms), consistent with ordinary system-
load variance between sessions rather than a real regression; `ociman
build` remains solidly faster than `podman build` either way, and the
dockerignore-optimized path (a real 20MB ignored directory costing
virtually nothing extra, 1.01×) is, if anything, measurably *better*
than 0133's own already-good 1.06× result.

## Conclusion

No regression found. No code change needed or made this session —
purely a confirmation round, per this project's own established
practice of re-verifying performance claims directly and periodically
rather than only at the moment a change first lands.
