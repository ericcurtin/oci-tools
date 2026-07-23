# Design note 0235: fold the `ociman commit` comparison into `ci/bench.sh`

Status: implemented
Scope: `ci/bench.sh`, `docs/benchmarks.md`.

## Closing a documented gap in the consolidated benchmark script

0219 consolidated this project's three headline hyperfine comparisons
(`ocirun run` vs crun/runc, `ociman run --rm` vs podman/docker,
`ociman rm` vs `podman rm`) into one reusable `ci/bench.sh`, and
`docs/benchmarks.md` has explicitly listed `ociman commit` — a
headline-table figure of its own (38× vs real `podman commit` in
0183) re-measured by hand in every performance-reverification note
since 0161 — as "real, still-ahead follow-up work to fold into the
script rather than leaving [it] hand-run-only" ever since. This
increment folds it in.

## Method — the same one every reverification note already used

Verbatim from 0176's own "Method" section (and 0170/0183's identical
one): one real, already-stopped container per tool (`sh -c "echo hi >
/f.txt"`, a real, nonempty diff layer), reused every sample, each
sample re-committing over the same tag
(`localhost/oci-tools-bench-commit:latest`) — a real, no-error
operation for both tools. Repeated re-commits don't meaningfully grow
either store: the committed *layer* is content-identical every sample
(same rootfs, same file mtimes), so it deduplicates to one blob in
both content-addressed stores; only each commit's own
`created`-timestamped image config differs (tiny).

## The one real step the hand-run notes never spelled out

Those notes' own phrase "forcing plain-`Extract` rootfs setup" hides a
real prerequisite, re-discovered the hard way while wiring this up
(the first version of this section failed outright on this project's
own dev host): on any host where the rootless-overlay rootfs
optimization (0108) is supported, a container created in the *default*
store gets an overlay rootfs — and `ociman commit` rejects exactly
that container with its own real, documented "not supported yet" error
(0146). The hand-run measurements worked because they forced the
plain-`Extract` path; the script now encodes that forcing explicitly
so it never needs re-discovering again:

- The ociman half runs against a scratch storage root under the
  script's own `$workdir`, with the `.rootless-overlay-supported`
  probe-cache marker (`bin/ociman/src/rootfs_setup.rs`) pre-seeded
  `false` — the same mechanism this project's own offline integration
  tests already use for the identical reason.
- The image gets into that scratch store *offline*, via
  `ociman save` (from the same already-pulled default-store image
  every other section requires) piped through a real archive file
  into `ociman load` — a real, digest-verified round trip, no network.
- Cleanup is just `rm -rf "$workdir"`: the default ociman store is
  never touched at all. The podman half cleans up its own bench
  container/tag; deliberately no `podman image prune` (it would sweep
  dangling images this script didn't create — podman's few tiny
  leftover dangling configs are documented in the script instead).

Fairness: podman commits against its own default store, ociman
against a scratch store — the same asymmetry every hand-run
measurement already had (both tools do the identical work either way:
read one container's rootfs, diff, write one layer + config; the
storage root's path doesn't change what's committed), and the
alternative (skipping the comparison entirely on every
overlay-capable host, i.e. every dev host) would leave the figure
unverifiable exactly where it's actually watched.

Skipping stays opportunistic like every other section: no podman, or
the image not already pulled, skips that side with a clear message
rather than failing the run.

## Verified

- `bash ci/bench.sh` end to end on this host, twice in a row: the new
  section reports a real comparison both times (3.4ms vs 94.7ms,
  27.9×, then 29.5× — consistent with 0170/0183's own 27-38×
  hand-measured range), the three pre-existing sections still report
  their usual decisive wins, and the second run is unaffected by the
  first's leftovers.
- No leftovers after either run: no bench tag in either store, no
  `benchcommit` container in podman, no stray temp directories.
- The scratch-store forcing verified step by step by hand first
  (marker written, save/load round trip, plain-Extract container
  created, two consecutive commits over the same tag both succeed).
- `bash -n ci/bench.sh`; this change touches no Rust code at all
  (`cargo test --workspace` passing identically before and after).
