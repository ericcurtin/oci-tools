# Design note 0272: `ociman ps --filter status=<value>`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_ps.rs`.

## Closing a real gap: `ociman ps` had no `--filter` at all

Real `podman ps --filter`/`docker ps --filter` (checked directly
against a real installed `podman ps --help`) is a large, commonly
used family: `id`, `label`, `label!`, `annotation`, `annotation!`,
`name`, `exited`, `status`, `ancestor`, `before`, `since`, `volume`,
`health`, `until`, `pod`, `network`, `restart-policy`, `command`. This
note closes the single cleanest, best-verified, most commonly used
one — `status=` — deliberately leaving the rest for later, separately
researched increments (see "Still ahead").

## Research findings that shaped this narrower scope

Two things were checked directly against a real installed `podman`
before committing to an implementation, and both turned out more
subtle than a first read of podman's own vendored source suggested:

- **`label=`/`label!=` for `ps`** looked, from
  `vendor/go.podman.io/common/pkg/filters/filters.go`'s own
  `MatchLabelFilters`, like it should AND multiple values together
  (every `--filter label=` value must match). Direct testing mostly
  confirmed that (`label=env=prod --filter label=team=wrong` found
  nothing on a container matching neither), but `label=env=prod
  --filter label=env=staging` (two values for the *same* label key,
  each individually satisfiable by a *different* container) actually
  matched *both* containers — behavior the source's `MatchLabelFilters`
  read alone doesn't obviously explain. Rather than guess further at
  a still-not-fully-understood real edge case, this is deliberately
  left for a dedicated future increment with more direct testing.
- **`name=`/`id=`** use real regex matching (`regexp.MatchString`) in
  real podman, not a plain substring match — a new `regex` crate
  dependency this project doesn't currently have anywhere (checked
  directly: `grep -rn regex Cargo.lock` found nothing). Adding a full
  regex engine purely for this one flag is a real trade-off against
  this project's own "every binary stays lean, fast startup" pillar
  that deserves its own deliberate decision (substring-only vs. a new
  dependency), not folded silently into this smaller increment.

`status=`, by contrast, had a clean, fully verified real semantics
with no such open questions (see below), making it a safe, complete
increment on its own.

## Semantics, checked directly against a real installed `podman`

- Multiple `status=` values are OR'd together (checked directly:
  `--filter status=running --filter status=created` showed both a
  running and a merely-created container in the same call).
- Giving `--filter status=` **at all** overrides the default
  running-only filter entirely, `--all`/`-a` or not — checked
  directly: `podman ps --filter status=created` (no `-a` at all)
  still showed a `created` container a plain, filterless `podman ps`
  would otherwise hide. `ociman ps --filter status=...` matches this
  exactly.
- Real podman's own displayed/filterable vocabulary is finer-grained
  than this project's own five real states (`configured`/`created`
  split, a separate `exited` display string that a `stopped`
  filter value aliases to). This project has no such split — its own
  `ociman ps`'s `STATUS` column already only ever shows one of
  `creating`, `created`, `running`, `stopped`, `paused` (`Status::
  as_str`), so `--filter status=` accepts exactly those five values
  verbatim, an honest match to what this project's own vocabulary
  actually is rather than a literal, inapplicable copy of podman's
  finer one.
- An unrecognized `--filter` key, or an unrecognized `status=` value,
  is a clear, immediate error rather than a silently-ignored no-op,
  matching this project's own established `prune`/`images --filter`
  convention.

## Verified

Integration (`tests/tests/ociman_ps.rs`, three new tests):

- `--filter status=created` (no `-a`) shows a never-started container
  a plain `ps` hides; `--filter status=running` against the same
  container finds nothing (it never started).
- `--filter status=created --filter status=stopped` (OR'd) shows both
  a created-only and a stopped container in one call.
- An unrecognized filter key (`name=`) and an unrecognized `status=`
  value are both clear, immediate errors.

Full workspace: `cargo build`/`test --workspace`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`python3 ci/guards.py`, `cargo deny check`, `ci/native-ci.sh`,
`ci/build-deb.sh`.

## Still ahead

`ociman ps --filter name=`/`id=` is implemented in `0273` (resolved
the substring-vs-regex-dependency question without a new dependency:
real docker/podman's own regex search is behaviorally a substring
search for any ordinary, non-regex filter value). `label=`/`label!=`'s
own "still-not-fully-understood real multi-value semantics" turned out
to be test contamination, not a real anomaly — redone from a clean
slate, it cleanly AND's (`0274`'s own research note) — but implementing
it honestly first needed container-level labels to exist at all
(`ociman run`/`create --label`, closed in `0274`), so the filter itself
is left for a follow-up increment now that there's something real to
filter on. The rest of real podman's own larger `ps --filter` family
(`ancestor`, `before`/`since`, `pod`, `network`, ...) remain real,
separately-scoped next candidates.
</content>
</invoke>
