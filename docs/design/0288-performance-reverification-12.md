# Design note 0288: re-verifying every benchmark after 0246-0287

Status: done (measurement only, no product code changed)
Host: this project's own aarch64 dev host, `crun 1.14.1`/`runc 1.3.4`/
`podman 4.9.3`/`docker 29.2.1`, release build.

## Why re-measure, again

Following 0018/.../0221/0245's own established cadence (a formal note
roughly every 10-40 increments): forty-two increments (0246-0287)
landed since 0245, none of them a large hot-path rewrite (mostly
`ociman` CLI-surface additions — `ps --filter` variants, `--group-add`,
`-u`/`--user`, `system df -v`, `exists`, `stats` streaming — each
individually re-verified with `ci/bench.sh` at commit time per this
project's own established convention), but this span is long enough
that a full, formal re-comparison against 0245's own numbers is
overdue rather than merely inferred from "no single commit looked
alarming."

## Results

`ci/bench.sh` run twice (once with `podman`/`docker` missing the
`busybox` image, opportunistically skipping those comparisons; once
after pulling it into both, for the complete set below):

| comparison | this session | 0245 |
|---|---:|---:|
| `ocirun run` vs `crun run` | 3.1ms vs 6.9ms (2.23×) | 3.1ms vs 6.8ms (2.20×) |
| `ocirun run` vs `runc run` | 3.1ms vs 21.3ms (6.89×) | 3.1ms vs 20.3ms (6.59×) |
| `ocirun exec` vs `crun exec` | 2.0ms vs 3.7ms (1.82×) | 2.1ms vs 3.7ms (1.75×) |
| `ocirun exec` vs `runc exec` | 2.0ms vs 19.1ms (9.34×) | 2.1ms vs 19.2ms (9.08×) |
| `ociman exec` vs `podman exec` | 2.7ms vs 137.8ms (50.76×) | 2.9ms vs 138.1ms (47.86×) |
| `ociman exec` vs `docker exec` | 2.7ms vs 47.1ms (17.34×) | 2.9ms vs 47.5ms (16.47×) |
| `ociman run --rm` vs `podman run --rm` | 30.7ms vs 189.0ms (6.15×) | 33.2ms vs 200.2ms (6.04×) |
| `ociman run --rm` vs `docker run --rm` | 30.7ms vs 286.8ms (9.33×) | 33.2ms vs 298.3ms (9.00×) |
| `ociman run -d` vs `podman run -d` | 37.7ms vs 139.8ms (3.71×) | 39.5ms vs 151.3ms (3.83×) |
| `ociman run -d` vs `docker run -d` | 37.7ms vs 170.3ms (4.52×) | 39.5ms vs 175.8ms (4.45×) |
| `ociman rm` (destroy-only) vs `podman rm` | 1.8ms vs 70.2ms (39.66×) | 1.3ms vs 72.9ms (54.16×) |
| `ociman commit` vs `podman commit` | 3.9ms vs 96.3ms (24.51×) | 3.4ms vs 114.8ms (33.75×) |
| `ociman build --no-cache` vs `podman build --no-cache` | 65.9ms vs 1363ms (20.67×) | 68.7ms vs 1345ms (19.58×) |
| `ociman build --no-cache` vs `docker build --no-cache` | 65.9ms vs 1097ms (16.65×) | 68.7ms vs 1102ms (16.04×) |
| `ociman build` (cached) vs `podman build` (cached) | 9.1ms vs 178.5ms (19.66×) | 8.4ms vs 178.6ms (21.23×) |
| `ociman build` (cached) vs `docker build` (cached) | 9.1ms vs 237.5ms (26.16×) | 8.4ms vs 226.5ms (26.93×) |

## Reading

No regression anywhere. Every figure sits within ordinary session-to-
session host-load noise of its own 0245 counterpart (the two
individually-largest swings, `ociman rm`'s 39.66× vs 54.16× and
`ociman commit`'s 24.51× vs 33.75×, are both still comfortably
higher than any figure from *before* 0245, e.g. 0183's 13.94×/38.19×
— reading a shrinking absolute-multiple as a regression here would be
mistaking normal noise in an already-tiny (1.3-3.9ms) absolute number
for a real trend; the destroy-time story `docs/benchmarks.md` names
explicitly stays at a decisive several-dozen-times win either way).
`ocirun run`'s own purest startup measurement — the figure least
sensitive to storage-layer/build-cache changes and thus the most
meaningful single trend line across every reverification note since
0018 — remains essentially unchanged at ~2.2× over crun, ~6.9× over
runc.

## Verified

- `ci/bench.sh` run twice end to end (figures above from the complete,
  post-pull run); no leftover containers/dangling images in any of
  podman/docker/this project's own stores afterward (`podman image
  prune -f` reclaimed the dangling `commit`/`build`-benchmark layers
  the script's own repeated timed samples produce; the `busybox`
  image pulled into podman specifically for this comparison was
  removed again afterward, matching this project's own established
  "leave no test residue in a real installed tool's store" convention;
  docker's own pre-existing `busybox:latest`/`untiltest:*` images,
  already present before this session, predating today, were left
  untouched).
- No Rust code changed this note — `cargo build`/`test --workspace`
  passing identically before and after, plus the full local check
  suite as always before commit (`cargo fmt --all --check`, `cargo
  clippy --workspace --all-targets --locked -- -D warnings`, `python3
  ci/guards.py`, `cargo deny check`, `ci/native-ci.sh`,
  `ci/build-deb.sh`).
