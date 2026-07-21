# Design note 0107: a real `ubuntu:24.04` compatibility bug, and what it uncovered about scale

Status: implemented (the compatibility fix) + verification only (the
scale re-measurement, no functional change)
Scope: `crates/oci-spec-types/src/image.rs` (`null_as_default`,
applied to `ContainerConfig::exposed_ports`/`volumes`/`labels`),
`crates/oci-spec-types/tests/fixtures/ubuntu-24.04-image-config.json`

## Why this, now

0106 flagged "a real multi-thousand-file image... wasn't measured
directly this session" as an open gap in validating its own syscall-
reduction fix at real scale. Pulling one to close that gap
(`docker.io/library/ubuntu:24.04`, ~100 MB, real registry pull) hit a
real, blocking bug before any benchmark could even run.

## The bug: `"Volumes": null`

`ociman inspect`/`run`/anything else touching this image's config
failed outright:

```
error: reading config for docker.io/library/ubuntu:24.04
  caused by: blob sha256:ea17ec... does not look like a valid
  manifest: invalid type: null, expected a map at line 1 column 404
```

The real config blob's own `config.Volumes` field is a literal JSON
`null`. `ContainerConfig::volumes` (`BTreeMap<String,
serde_json::Value>`) only had `#[serde(default)]`, which — a genuine,
easy-to-miss serde subtlety — only ever covers a field **missing
entirely** from the JSON; a field that's **present but `null`** is a
different case serde's derive doesn't treat the same way, and fails
with exactly the observed type-mismatch error. `entrypoint`/`cmd`
(already `Option<Vec<String>>`) never had this problem — serde's own
built-in `Option<T>` impl already treats `null` as `None` for free —
which is exactly why this went unnoticed until a field that *isn't*
`Option`-typed hit it for real.

This isn't a hypothetical or a malformed image: Go's own
`encoding/json` (what every Docker-ecosystem tool that ever wrote this
config used) marshals a `nil` map as `null`, not `{}` — the standard,
expected shape for "this field was never set" from any Go-built image
config, and `docker.io/library/ubuntu` is about as mainstream a base
image as exists. This is precisely the kind of "drop-in replacement"
gap the project's own top-level goal cares most about: a real,
extremely common image that a real `podman`/`docker` handle without
comment, that `ociman` couldn't even `inspect`, let alone `run`.

## The fix

`null_as_default`: a small generic `deserialize_with` helper that
deserializes `Option<T>` first, then falls back to `T::default()` for
`None` — covering both "missing" and "present but null" uniformly.
Applied to `exposed_ports`/`volumes`/`labels` (`ContainerConfig`'s
three map fields, the ones sharing this exact real-world-null-prone
shape). Verified with a new test using a real, captured fixture
(`ubuntu-24.04-image-config.json`, the actual blob this session
pulled) — matching this crate's own established pattern
(`parses_real_busybox_image_config_including_pascal_case_container_
config` did the same for the earlier `PascalCase` bug) rather than a
hand-written minimal repro. Confirmed end-to-end, not just at the
unit-test level: `ociman inspect`/`ociman run --rm ubuntu:24.04 --
/bin/echo hello` both now work against the real pulled image.

## What this uncovered once unblocked: a real scale gap against podman

With `ubuntu:24.04` finally usable, 0106's own open gap could finally
be closed — `strace -f -c`, same real `ociman run --rm` cycle, this
session's own release binaries (0106's fix vs. immediately before it):

| | before 0106 | after 0106 |
|---|---:|---:|
| total syscalls | 43492 | 34279 (**-21%**) |
| `statx` | 6266 | 45 (**-99%**) |
| `mkdirat` | 3461 | 680 (**-80%**) |
| syscall errors | 6487 | 63 (**-99%**) |

`hyperfine` (`--shell=none`, 40 samples): **312.2 ms → 304.8 ms**
mean, `System` time 126.3 ms → 118.3 ms — confirms 0106's own
prediction ("the eliminated calls scale with file count, so a real
multi-thousand-file image should see proportionally more") directly:
a clearer, more statistically confident win here (tighter relative to
this run's own noise band) than the busybox-scale measurement 0106
itself reported.

But measured directly against real `podman run --rm` for the exact
same image and cycle: **`podman` (181.9 ms) is 1.71× *faster* than
`ociman` (310.8 ms)** at this real scale — the opposite of every
smaller-image comparison this project has ever published (0034,
0105). This is an honest, important finding, not one to bury: real
`podman`'s own `overlay2` graph driver mounts a copy-on-write overlay
in near-constant time regardless of image size, while `ociman`'s own
"no overlay/COW filesystem" design pillar means every `run` still
fully extracts every layer's own files from scratch — cheap enough to
stay ahead of podman at busybox's own ~370-file scale (0105: `ociman`
2.9-3.5× *faster*), but a real, current image with tens of thousands
of files flips that relationship entirely. 0106's own syscall-count
fix measurably narrows this gap (fewer wasted syscalls per file) but
cannot close it on its own: the fundamentally different big-O shape
(`ociman`'s own extraction cost scales with file count; `podman`'s
own overlay mount cost does not) is an architectural property, not a
constant-factor one.

## What this doesn't fix, and why not attempted here

Closing the scale gap for real needs an architectural change — most
plausibly a cached "already-extracted golden copy" per image digest,
cloned into each new container's own rootfs via a cheap
copy-on-write/hardlink mechanism instead of a full decompress-and-
write-every-file extraction each time — not a small, safe, reversible
change to make in the same session as an unrelated compatibility bug
fix, and a real design decision (which mechanism, and how it
interacts with this project's own explicit "no overlay2/no
COW-filesystem" design pillar) that deserves its own dedicated
increment rather than being rushed here. Flagged here, honestly and
with real numbers, as this project's own next real priority for the
"beat every real equivalent on all benchmarks" goal specifically for
larger, more realistic images — not deferred silently.
