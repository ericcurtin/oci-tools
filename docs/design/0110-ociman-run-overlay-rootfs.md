# Design note 0110: wiring the real overlay-based rootfs into `ociman run`

Status: implemented
Scope: new `bin/ociman/src/rootfs_setup.rs`; `bin/ociman/src/main.rs`
(`cmd_run`'s own rootfs setup, `cmd_exec`'s own `--user` resolution);
`crates/oci-runtime-core/src/state.rs` (`StateStore::remove`);
`crates/oci-runtime-core/src/overlay.rs`
(`reset_permissions_for_removal` made `pub`).

## Why this, now

0107 measured and documented a real regression against real `podman`
at scale (real `podman run` 1.71× *faster* than `ociman run` for a
real `ubuntu:24.04`, since `ociman` fully re-extracts every layer's
own files from scratch on every single invocation). 0108 and 0109
each built one of the two prerequisite pieces this fix needs — a real,
tested rootless-overlay feasibility probe, and a real, tested per-
manifest-digest rootfs-extraction cache — deliberately unwired from
any live container path, landed as their own safe, standalone
increments. With both pieces real and tested, this increment does the
actual wiring: `ociman run` now uses a real overlay mount (the cache
as `lowerdir`, a fresh private `upperdir`/`workdir` per container)
whenever the environment supports it, falling all the way back to the
exact previous per-container extraction otherwise.

## The shape: one ordinary overlay entry in the bundle's own spec

Exactly the design 0108's own "what this doesn't do yet" section
already sketched, and it held up unchanged: no new code in
`oci_runtime_core` at all was needed. Its own mount-application code
(`rootfs::plan_rootfs_setup`/`launch::execute_rootfs_action`) already
applies an arbitrary `spec.mounts` entry generically. `rootfs_setup::
decide` (new) either returns `RootfsSetup::Extract` (today's own
unchanged code path) or `RootfsSetup::Overlay { mount, user_resolve_
root }` — one real `spec.mounts` entry (`destination: "/"`,
`type: "overlay"`, `lowerdir=`/`upperdir=`/`workdir=` options),
prepended (not appended) to the bundle's own mount list, ahead of the
already-present `/proc`/`/dev`/`/sys`/... entries, which are all
subdirectories of the root this one provides.

`decide` never fails outright: any real problem past the initial
support check (building/reusing the cache, creating the container's
own private `upper`/`work` directories) is logged and degrades to
`RootfsSetup::Extract` instead — a real, transient problem on any
single invocation (disk full building the cache, ...) shouldn't fail
a container a moment before this feature existed would have started
successfully.

## Probing once, not once per container

0108's own probe forks and does a real `unshare(CLONE_NEWUSER|
CLONE_NEWNS)` + mount cycle — real, measurable cost that would be a
pure, ongoing regression if paid on every single `ociman run`,
especially in an environment that turns out not to support it at all
(every container there would pay the probe's own cost *and* still
fall back to the unchanged extraction path). `rootless_overlay_
supported_cached` persists the answer in a small marker file under
the storage root (`.rootless-overlay-supported`) the first time it's
needed, so every later invocation just reads one small file instead —
the same "detect once, remember the answer" trade real container
engines already make for analogous capability checks. The cacheable
logic itself (`read_or_compute_cached_bool`) has five direct unit
tests of its own, entirely independent of the real (unsafe to call
from a plain `#[test]`, see 0108's own established reasoning) probe.

## Two more real bugs, both caught by the existing test suite actually running against it

Landing this exposed the *existing*, already-extensive `ociman_run.rs`/
`ociman_exec.rs`/`ociman_name.rs`/... test suite to the real overlay
path for the first time (this session's own dev host supports it, so
these tests now organically exercise it, not just a synthetic
probe) — and it found two more real, previously-invisible bugs, the
same way actually running code (not just reading it) already caught
two in 0108 itself:

1. **`ociman exec --user <name>` read the wrong `/etc/passwd`.**
   `cmd_exec`'s own `--user` resolution read `bundle.rootfs_path()` —
   a plain host-side directory path — directly. For a container using
   the overlay path, that directory is (and stays) empty from the
   *host's* own point of view: the overlay mount that actually
   populates it exists only *inside* the container's own private mount
   namespace. Fixed by reading through `/proc/<pid>/root` instead — the
   kernel's own live view of exactly what that already-running
   container process's own root filesystem contains right now, correct
   regardless of how the rootfs was constructed (and not just for this
   one case: correct for *any* mount the container's own init might set
   up in the future too, not an assumption specific to this project's
   own rootfs-construction method). Caught by
   `exec_user_flag_resolves_a_named_user_via_the_containers_own_etc_
   passwd` failing the moment this increment first ran the existing
   suite against the real overlay path, not anticipated in advance.
2. **Removing a container that used the overlay path failed outright
   with a real `EACCES`/`EPERM`.** The exact same real kernel-level
   finding 0108 already made and worked around for its own probe's
   scratch directories (a real overlay mount's own kernel bookkeeping
   locks its `workdir/work` subdirectory to mode `0000`) recurs for a
   real container's own bundle directory once it exits — `StateStore::
   remove`'s own plain `fs::remove_dir_all` can't traverse that locked
   subdirectory. Fixed by promoting 0108's own cleanup helper
   (`overlay::reset_permissions_for_removal`) to `pub` and using it as
   a **retry-only** fallback: `remove` still tries a plain
   `remove_dir_all` first (free for the overwhelming majority of
   containers, which never hit this), and only pays the recursive
   permission-reset walk's own real cost if that first attempt actually
   fails — never a cost paid by a container that didn't need it. Caught
   by `run_name_shows_up_in_ps_and_rm_accepts_the_name` — bisected down
   from a confusing "fails in this test file but an apparently-
   identical standalone reproduction succeeds" symptom to the one real
   difference (this test's own `rm` step, missing from the first,
   passing reproduction attempt) before finding the real cause, not
   guessed at from the error message alone.

## Measured impact: the regression not just closed, but reversed

Same real methodology 0105/0107 already established (`hyperfine
--shell=none`, 5+ warmup, 60 samples), this session's own release
binaries, real `docker.io/library/ubuntu:24.04`:

| | mean (0107, pre-fix) | mean (now) | relative (now) |
|---|---:|---:|---:|
| `ociman run --rm` | ~310.8 ms | **~48.1 ms** | 1.00× |
| `podman run --rm` | ~181.9 ms | ~187.2 ms | 3.89× slower |
| `docker run --rm` | ~282-284 ms | ~286.8 ms | 5.97× slower |

`ociman`'s own `System` time collapsed from ~123.5 ms to ~6.3 ms — the
direct, measured effect of replacing a full per-file re-extraction
with one `mount(2)` call. 0107's own regression (`podman` 1.71×
*faster* than `ociman` at this exact scale) isn't just closed, it's
reversed: `ociman run` is now **3.89× faster than podman and 5.97×
faster than docker** for this same real image and cycle.

The smaller-image case (`docker.io/library/busybox:latest`, 0105/0106's
own benchmark) shows no regression either: ~50-53 ms now, matching
(if not slightly improving on) the ~55-62 ms these same increments
measured immediately before this one, and still a solid **3.4× faster
than podman** — the overlay path's own small fixed overhead (cache
lookup, one mount syscall) roughly breaks even against the old
extraction path's own already-small cost for a ~370-file image, exactly
as expected, while paying off decisively as file count grows.

## Real, automated tests

No new integration tests were written specifically for the overlay
path — deliberately: this session's own real verification was the
*existing*, extensive `ociman_run.rs` (40 tests: seccomp, capabilities,
resource limits, read-only, volumes, user resolution, ...),
`ociman_exec.rs`, `ociman_name.rs`, `ociman_build.rs` (unaffected —
scoped to `ociman run` only, see below), `ociman_rmi.rs`,
`ociman_detach.rs`, and more, all passing unmodified against the real
overlay path this session's own dev host takes by default — a much
stronger, more real signal than a handful of new, necessarily-narrower
tests could give on their own, and it directly caught both real bugs
above. `rootfs_setup`'s own new, pure caching-logic tests (five, see
above) cover the one piece of this increment that's safely testable
in isolation at all, matching 0108's own established precedent for
why the real fork+`unshare`-based logic itself isn't unit-tested
directly.

## What this doesn't do yet

* **`ociman build`'s own analogous base-layer extraction
  (`build_stage`) is untouched** — its own rootfs gets further
  modified (`RUN`/`COPY`/`ADD`) and re-diffed to produce new layers, a
  materially different, more invasive change to wire up (the diff
  computation would need to understand an overlay's own `upperdir` as
  the diff directly) than `ociman run`'s own simpler "extract once,
  run one command, tear down" shape — unchanged from 0109's own
  identical note.
* An environment where 0108's own probe reports no support (or where
  building the cache/preparing per-container directories fails for
  any other reason) still takes the exact original, unchanged
  extraction path — not measured freshly this session (this dev host
  supports the overlay path), but unchanged code, still covered by
  every test that ran before this increment existed.
* No cleanup of an old/unreferenced rootfs-cache entry — matches
  0109's own identical, already-documented gap.
