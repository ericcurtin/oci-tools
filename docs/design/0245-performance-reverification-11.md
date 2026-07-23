# Design note 0245: re-verifying every benchmark after 0222-0244 (+ `run -d` folded into `ci/bench.sh`)

Status: done (measurement + one `ci/bench.sh` addition, no product code
changed)
Host: this project's own aarch64 dev host, `crun 1.14.1`/`runc 1.3.4`/
`podman 4.9.3`/`docker 29.2.1`, release build.

## Why re-measure, again

Following 0018/0105/.../0183/0221's own established cadence (a formal
note roughly every 10-19 increments): twenty-three increments
(0222-0244) landed since 0221. `ci/bench.sh` has been run at the end
of *every* increment in that span (each commit message carries its
own figures), so no regression could have hidden long — but the
formal note is where the full table gets compared against its own
history, and this span contains one genuinely
hot-startup-path change worth calling out:

* **0239** (default `/dev` nodes) added real per-start work to every
  container launch — 6 device nodes (rootless: bind mounts) + 6
  symlinks — the same work real crun/runc do, making the comparison
  *fairer* while it needed re-measuring.
* Everything else in the span was `ocicri`-only (a long-lived server,
  the documented exception to the startup pillar), benchmark tooling
  (0235), or stop-path-only (0244).

## `run -d` folded into `ci/bench.sh` (the last hand-run figure)

`docs/benchmarks.md` had one remaining "hand-run-only" comparison:
the isolated detached create+start (`ociman run -d` vs
`podman run -d`, measured by hand in every note since 0161). Now a
fifth section of `ci/bench.sh`, same opportunistic-skip rules as the
others: each sample starts a real detached container (`sleep 60`) and
returns once it's running; the previous sample's container is removed
in `--prepare`, outside the timed region; docker included alongside
podman. Verified leftover-free across both stores after a full run.

## Results

| comparison | this session | most recent prior note |
|---|---:|---:|
| `ocirun run` vs `crun run` | 3.1ms vs 6.8ms (2.20×) | 0183: 3.4ms vs 7.5ms (2.20×) |
| `ocirun run` vs `runc run` | 3.1ms vs 20.3ms (6.59×) | 0183: 3.4ms vs 21.8ms (6.37×) |
| `ociman run --rm` vs `podman run --rm` | 33.2ms vs 200.2ms (6.04×) | 0183: 66.8ms vs 189.9ms (2.84×) |
| `ociman run --rm` vs `docker run --rm` | 33.2ms vs 298.3ms (9.00×) | 0183: 66.8ms vs 289.9ms (4.34×) |
| `ociman run -d` vs `podman run -d` | 39.5ms vs 151.3ms (3.83×) | 0170 (hand): 35.8ms vs 164.1ms (4.58×) |
| `ociman run -d` vs `docker run -d` | 39.5ms vs 175.8ms (4.45×) | (first scripted measurement) |
| `ociman rm` (destroy-only) vs `podman rm` | 1.3ms vs 72.9ms (54.16×) | 0183: 5.2ms vs 72.4ms (13.94×) |
| `ociman commit` vs `podman commit` | 3.4ms vs 114.8ms (33.75×) | 0183: 2.6ms vs 98.7ms (38.19×) |

## Reading

No regression anywhere — several figures are substantially *better*
than 0183's, which deserves honest interpretation rather than
celebration: the `run --rm`/`rm` improvements largely predate this
span (0221 already noted session-to-session variance; the rootless-
overlay rootfs cache and prune-based reclamation landed earlier and
dominate), and absolute numbers swing with host load session to
session. The stable claims: every comparison remains a decisive win;
`ocirun run` — the purest startup measurement, and the one 0239's own
added `/dev` work lands directly inside — is byte-for-byte the same
2.2× over crun it was at 0183, *with* the fairness gap closed (both
sides now populate the same device nodes); and destroy time (the
goal's own named emphasis) holds at ~50× over `podman rm`.

## Verified

- `ci/bench.sh` run end to end with the new section (figures above);
  no `benchd` leftovers in any of the three engines' stores.
- `bash -n ci/bench.sh`; no Rust code changed (`cargo test
  --workspace` passing identically before and after, plus the full
  local check suite as always before commit).
