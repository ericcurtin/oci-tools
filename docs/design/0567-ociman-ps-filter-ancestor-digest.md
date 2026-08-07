# Design note 0567: `ociman ps --filter ancestor=<full-manifest-digest>`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_ps.rs`.

## What this closes

`docs/design/0281` (which implemented `--filter ancestor=`'s own
name/tag substring matching) explicitly deferred an exact, full
manifest-digest match under "Still ahead." No later note (checked
through `0566`) ever revisits or closes it.

## Real, checked-directly confirmation — re-verified live against a
real installed binary, not just trusting the old note

`0281`'s own original research already found a real installed-vs-
cloned-source version mismatch: the cloned `~/git/podman` source
(`v5.4.0-rc1`) suggests `ancestor=` should substring-match a short
image-ID prefix too, but the actually-*installed* `podman` (4.9.3, on
this same host) does not. Re-verified today, live, unchanged:

```
$ podman create --name x docker.io/library/busybox:latest true
$ podman inspect docker.io/library/busybox:latest --format '{{.Id}}'
e0e8b3cbfed68a90084781e2962f9c0deead51c5a3f11a488eef0283a4284bc2
$ podman ps -a --filter ancestor=e0e8b3cbfed68a90084781e2962f9c0deead51c5a3f11a488eef0283a4284bc2 --format '{{.Names}}'
x
$ podman ps -a --filter ancestor=e0e8b3cbfed6 --format '{{.Names}}'
(no output — a 12-char short prefix does NOT match)
$ podman ps -a --filter ancestor=sha256:e0e8b3cbfed6...4284bc2 --format '{{.Names}}'
(no output — a sha256:-prefixed full ID does NOT match either)
```

A real, precisely-bounded rule: only a full, bare, un-prefixed
64-hex-char ID matches — no short prefix, no `sha256:` scheme prefix.

## Real functional gap, not a faithful no-op

Before this change, `ociman ps --filter ancestor=<any-real-image-ID>`
could **never** match anything at all — the stored container
annotation is always a name:tag reference string, never a bare hex
ID, so a plain substring check is always false for a hex-shaped
value. This is an observable capability real `podman ps --filter
ancestor=<id>` users (a documented, commonly-used form) would trip
over silently (empty results, no error) switching to `ociman`.

This project's own "image ID" convention project-wide is the
**manifest digest** (not real docker/podman's own separate config-
digest-based `IMAGE ID`, which is why the numeric ID values in the
example above differ in spirit, not just value, from what this
project's own `ociman images` shows) — the same already-established
divergence every other image-ID-resolving command here (`rmi`/
`inspect`/`images`) already has, not a new one introduced here.

## Why this is narrow and safe

Pure CLI-filtering logic plus one additional, on-demand lazy image-
store lookup (the same shape `--size`'s own existing `open_store()`
already establishes) — no cgroup/namespace/capability/systemd/mount
interaction of any kind. `cmd_run`/`cmd_create`/`cmd_stop`/`cmd_kill`/
etc. are completely untouched.

## Implementation

- `matches_ancestor_filter` gains a third parameter,
  `manifest_digest: Option<&str>` — checked last, after the existing
  name/tag substring rules, via exact case-insensitive string
  equality (never a `.contains()`/prefix check, matching the real,
  checked-directly-narrow rule above).
- `cmd_ps`: a new `ancestor_wants_digest_lookup` bool (true only when
  at least one `--filter ancestor=` value is exactly 64 hex
  characters) decides whether the image store needs opening at all —
  combined into the existing `store` lazy-open alongside `--size`
  (`(size || ancestor_wants_digest_lookup).then(open_store)`). When
  needed, a `HashMap<&str, String>` mapping each *distinct* container
  image reference to its real, resolved manifest-digest hex is built
  once, up front (the same "resolve once outside the per-container
  filter closure, which must stay infallible" reasoning
  `before_threshold`/`since_threshold` already establish) — a
  container whose own recorded image can no longer be resolved (e.g.
  `rmi`'d since) simply has no entry, so its own `ancestor=` digest
  match honestly fails rather than erroring.
- A real bug caught and fixed while implementing this: the existing
  size-computation `.map()` closure gated only on `store.is_some()`,
  not on the `size` flag itself — meaning `--filter ancestor=<digest>`
  (which now also opens the store) would have accidentally turned on
  the `SIZE` column too, even without `--size`. Fixed by gating
  explicitly on `size && store.is_some()`.

## Tests

One new integration test in `tests/tests/ociman_ps.rs`:
`ps_filter_ancestor_matches_a_real_full_manifest_digest_but_not_a_
prefix_or_scheme` — a real image built via `seed_image`, its own
real manifest digest resolved via `Store::resolve_image`, proving the
full digest matches, a `sha256:`-prefixed form doesn't, a short
prefix doesn't, and (the real bug above) the `SIZE` column never
appears without `--size` even when the digest lookup opens the store.

Manually verified end to end beyond the automated test: a real image
built via `ociman build`, a real container created from it,
`--filter ancestor=<full hex>` (match), `ancestor=sha256:<full hex>`
(no match), `ancestor=<12-char prefix>` (no match), a case-insensitive
uppercase full hex (match), and the pre-existing name/tag substring
matching all confirmed unaffected.

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (129
test-result blocks, all passing — no new test file added, so the
block count is unchanged from `0566`; `RUST_TEST_THREADS=2` given
this host's own heavy, persistent concurrent-session CPU contention
this same day; one isolated `ocicri_container.rs` flake under the
same contention on the first attempt, confirmed transient by an
immediate isolated rerun, then a fully clean run on the second
attempt), `python3 ci/guards.py` (clean), `cargo deny check` (clean),
`bash ci/native-ci.sh` (clean on the first attempt), `bash ci/
build-deb.sh` (clean on the first attempt, real `dpkg -i`/
`--version`/`dpkg -r` round trip). `ociman ps` is not exercised by
`ci/bench.sh` at all, and the common case (no `ancestor=` filter, or
a non-digest-shaped one) has zero added overhead — the new digest map
is only ever computed when a filter value is exactly 64 hex
characters — no benchmark rerun needed.

## Deliberately still out of scope

Real docker/podman's own broader "or a descendant" image-lineage
semantics for `ancestor=` remains a real, deliberately deferred
candidate — this project's own content-addressed store has no direct
"parent image" graph at all to trace it through.
