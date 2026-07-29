# Design note 0311: `ociman rm --ignore`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_ps.rs`.

## Closing `0310`'s own "still ahead"

`0310` named `ociman rm --ignore` (tolerating an unresolvable id) as a
real, separately-scoped candidate. This note closes it.

## Real, checked-directly semantics — not assumed from `--help` text alone

Read `~/git/podman/cmd/podman/containers/rm.go` and `pkg/domain/infra/
abi/containers.go`'s own `ContainerRm`/`getContainers` directly, then
verified against the installed `podman 4.9.3` binary itself:

- Real podman's own `getContainers` call always passes `ignore: true`
  internally regardless of the user's own flag ("Force ignore as
  `podman rm` also handles external containers") — an unresolvable
  name never hard-errors at that first lookup stage at all in real
  podman's own implementation; it falls through to a second,
  storage-layer removal attempt (for a container c/storage itself
  knows about but libpod's own database doesn't — e.g. one made
  directly by buildah/skopeo). The user-facing `--ignore` flag only
  gates *that* second failure.
- Since this project has no such "external, non-libpod-tracked
  container" concept at all, the real, honest, one-step translation is
  exactly what `0310` anticipated: an id that doesn't resolve to any
  real container here is silently skipped under `--ignore`, a hard
  error otherwise.
- Verified live, both directions: `podman rm --ignore
  nonexistent-container-xyz` → exit `0`, silent. `podman rm
  nonexistent-container-xyz` (no flag) → exit `1`, hard error.
- Verified live that `--ignore` does **not** widen to any other
  failure class: a genuinely running container refused without
  `--force` produces the identical hard error (`cannot remove
  container ... container state improper`, exit `2`) whether or not
  `--ignore` is given. `--force` implies `--ignore`
  (`rmOptions.Force { rmOptions.Ignore = true }` in the Go source).

## Reused, not reinvented

`ociman rmi --ignore` already exists (`cmd_rmi`) with the identical
narrow scope, already documented with the identical real-podman
citation. `cmd_rm`'s own existing "resolve every id first, abort on
the first failure" loop is the exact same shape as `rmi`'s per-
reference resolve loop — this change mirrors it almost verbatim: the
eager `?`-propagating resolve loop becomes a `match` that drops an
unresolvable id (continuing to the next one) when `ignore` is set,
still aborting immediately otherwise. `force || ignore` reuses the
same convention `rmi --force` already established.

`resolve_container_id` itself doesn't distinguish "genuinely doesn't
exist" from a defensive-only "multiple containers share this name"
branch — but container-name uniqueness is already enforced at
creation time, making that second branch unreachable in practice. Any
resolve failure is therefore tolerated under `--ignore` here — a
narrower gate in theory than real podman's own two-specific-error-
class check, identical to it in every case actually reachable in this
project's own architecture (documented honestly in the code, not
silently assumed).

## Verified

Manual, end-to-end, cross-checked directly against the installed
`podman 4.9.3` before and after implementing: `--ignore` on a
nonexistent id succeeds silently; without the flag, the identical
input is a clear error; `--ignore` combined with one real, resolvable
container and one nonexistent one still removes the real one and
drops only the unresolvable one; `--force` alone (no explicit
`--ignore`) also tolerates a nonexistent id; a still-running container
refused without `--force` is *not* tolerated by `--ignore` alone,
matching real podman's own identical narrow behavior exactly (checked
side by side against the real binary).

Integration (`tests/tests/ociman_ps.rs`, 5 new tests, the established
home for `rm`'s own test suite): silently skips a nonexistent id;
still removes a real container alongside an unresolvable one; `--force`
alone implies `--ignore`; `--ignore` does not tolerate a non-stopped-
without-`--force` refusal (reusing the exact same bare "created"-
record seeding technique an existing test already established, rather
than launching a real long-lived process); `--ignore` never widens
`--cidfile`'s own separate "the file itself can't be read" case
(0310's own deliberately narrow scope, kept even now that `--ignore`
exists).

Regression: all 32 `ociman_ps.rs` tests pass (27 pre-existing + 5
new); full `cargo test --workspace --locked` (112 test result blocks,
0 failures).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).

Performance: `ociman rm` is a one-shot, offline command, not part of
any hot-path benchmark tracked in `docs/benchmarks.md` — the common,
no-`--ignore`-given case is unchanged (the resolve loop still aborts
immediately on the first failure, same as before this flag existed).
No re-benchmark needed.

## Still ahead

`ociman kill`/`ociman stop`/`ociman restart --all`/`--cidfile`/
`--ignore` remain real, separately-scoped candidates — all three are
still single-`<ID>` commands today (unlike `rm`, which already had the
`Vec<String>` multi-target shape this and `0310` both built on),
so extending any of them needs a genuine multi-target-command
refactor, not just one more flag.
