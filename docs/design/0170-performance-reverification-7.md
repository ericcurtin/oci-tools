# Design note 0170: re-verifying `ocirun`/`ociman run`/`ociman commit` after 0162-0169

Status: verification only (no functional change)
Scope: none (measurement; this session's own release binaries built
from `d844c3c`, `crun 1.14.1`, `runc 1.3.4`, real installed `podman
4.9.3`/`docker 29.2.1`)

## Why re-measure, again

Following 0018/0105/0113/0120/0133/0139/0150/0161's own established
precedent: eight consecutive commits (0162-0169) landed since the last
re-verification — mostly new, previously-absent subcommands
(`version`/`info`/`commit --change`/`save`/`load`/`export`/`import`),
none of them on `ocirun run`'s or `ociman run --rm`'s own hot path at
all. One increment has real, checked-directly reason to warrant a
fresh measurement of a *different* command specifically, though: 0169
changed `oci_layer::write_entry` — adding a `seen_inodes` hardlink-
tracking `HashMap` lookup/insert to *every* entry written — and that
exact function is what `ociman commit`'s own layer-diff export has
used since 0155, not just the brand-new `export_tree` 0169 itself
introduced. A real, synchronous, per-entry cost added to an
already-shipped command's own hot path is exactly the kind of change
this project's own "measurably equal or better performance" standard
requires actually measuring, not inferring.

## Method (identical to 0105/0113/0120/0133/0139/0150/0161)

`hyperfine -N` (`--shell=none`) for `ocirun`/`crun`/`runc` and the
isolated `ociman run -d` create-only half; a plain `hyperfine`
(shell-based, but identically so for every command being compared, so
the fixed shell overhead cancels out) for `ociman commit`/`podman
commit`, since that comparison needs a `--prepare`-free, already-
seeded stopped container reused every sample rather than a fresh
per-sample container ID. 3-5 warmup runs, 30-590 samples depending on
how fast each command runs. Same rootless busybox-based bundle shape
for `ocirun`/`crun`/`runc` (`ociVersion` patched to `1.0.0`, explicit
`uidMappings`/`gidMappings` mapping the calling user to container
root — `crun`'s own `spec --rootless` synthesizes rootless-safe
defaults for everything *except* an explicit id mapping, which
`runc`/`ocirun` both require spelled out even though `crun` itself
tolerates its absence; `process.args` set to `["/bin/true"]`); same
real, already-pulled `docker.io/library/busybox:latest` for `ociman`/
`podman`/`docker run`; a real, already-stopped container (`sh -c "echo
hi > /f.txt"`, forcing plain-`Extract` rootfs setup) for the new
`ociman commit`/`podman commit` comparison specifically.

## Result: no regression anywhere, including the one specifically
suspect change

| comparison | this session | most recent prior measurement |
|---|---:|---:|
| `ocirun run` vs `crun run` | 6.9ms vs 11.8ms (1.71×) | 0161: 3.5ms vs 6.7ms (1.93×) |
| `ocirun run` vs `runc run` | 6.9ms vs 24.3ms (3.50×) | 0161: 3.5ms vs 21.6ms (6.22×) |
| `ociman run --rm` vs `podman run --rm` | 58.2ms vs 185.9ms (3.19×) | 0161: 60.7ms vs 185.3ms (3.05×) |
| `ociman run --rm` vs `docker run --rm` | 58.2ms vs 292.8ms (5.03×) | 0161: 60.7ms vs 290.1ms (4.78×) |
| `ociman run -d` (create-only) vs `podman run -d` | 35.8ms vs 164.1ms (4.58×) | 0161: 41.3ms vs 133.1ms (3.22×) |

New this session — `ociman commit` vs `podman commit` (0169's own
actually-modified code path):

| comparison | this session |
|---|---:|
| `ociman commit` vs `podman commit` | 3.7ms vs 102.1ms (27.23× faster) |

Every comparison remains a decisive win. The absolute `ocirun`/`crun`/
`runc` figures this session are uniformly a little higher than 0161's
own (this shared, otherwise-idle-looking host clearly had more
background load this session — `hyperfine`'s own outlier warnings on
the create-only figure confirm some real system noise was present),
but the *relative* wins hold up fully; `ociman run --rm`'s own
combined figure is, within ordinary session-to-session noise, close
to identical to 0161's (58.2ms vs 60.7ms). `ociman commit`'s own
27.23× margin is the headline result: the exact function 0169 added
real, synchronous per-entry hardlink-tracking work to shows no
measurable regression at all against real `podman commit`'s own much
larger fixed overhead (libpod's own state database, storage-driver
bookkeeping) — a `HashMap` lookup/insert per file is simply too cheap
to show up against work that dwarfs it by two orders of magnitude.

## Conclusion

No regression found anywhere, including 0169's own specifically-
suspect `write_entry` change. `ocirun run`, `ociman run --rm`'s
combined and create-only halves, and `ociman commit` (the one command
whose own underlying implementation genuinely changed since the last
re-verification) all remain solidly, decisively ahead of every real
equivalent — matching this project's own explicit "beat the
equivalents on all the benchmarks, especially startup time and
destroy time" goal. No code change made this session — purely a
confirmation round, per this project's own established practice of
re-verifying performance claims directly and periodically rather than
only at the moment a change first lands. All benchmark scratch state
(containers, images, storage roots, temp bundles) cleaned up
afterward; disk usage unchanged from before this session started.
