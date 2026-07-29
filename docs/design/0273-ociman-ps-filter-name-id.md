# Design note 0273: `ociman ps --filter name=`/`id=`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_ps.rs`.

## Closing two more real gaps from `0272`

`0272` implemented `ociman ps --filter status=` and deliberately
deferred `name=`/`id=` pending a decision on whether to add a `regex`
crate dependency (real docker/podman both use actual regex matching
for these). This note closes that gap with a dependency-free design
that's behaviorally identical to both real tools for the overwhelming
majority of real-world usage.

## The key research finding: plain-text regex search *is* substring search

Both real docker's (`daemon/internal/filters/parse.go`'s `Args.Match`)
and real podman's (`vendor/.../filters.go`'s `filterReferences`-
adjacent helpers) `name=` filters run `regexp.MatchString(pattern,
source)` — Go's regex search is **unanchored**: it looks for the
pattern *anywhere* in the string, not a full match. For an ordinary,
non-regex filter value (no `^`, `$`, `.`, `*`, character classes,
etc. — overwhelmingly the common real case, e.g. `--filter
name=myapp`), this is behaviorally identical to a plain substring
search. Verified directly: `podman ps --filter name=contain` matched
a container actually named `mycontainer123` (substring, not full-name,
match).

This project implements `name=<substring>` as exactly that — a plain
`str::contains` check — rather than adding a new `regex` crate
dependency this project has nowhere else. This is an honest,
documented simplification: an actual regex pattern with metacharacters
would behave differently here than in real docker/podman, but that's
a rare real-world case for this specific flag, and the common case
matches exactly.

## `id=` is different: prefix match, not substring

Real podman's own `FilterID` (`vendor/.../filters.go`) is a special
case, not the generic `Match`: for a value that looks like plain hex,
it does `strings.HasPrefix`, never a substring-anywhere search (real
docker's own generic `Match` would technically allow substring-
anywhere for `id=`, but podman's own dedicated prefix rule is the
semantically correct one for how real users actually give IDs — a
truncated *prefix* of a longer one, e.g. `--filter id=abc123` to match
a container whose ID *starts with* `abc123`). Verified directly
against a real installed podman: a 6-hex-char prefix of a real
container's own ID matched via `--filter id=<prefix>`.

`ociman ps --filter id=<prefix>` matches this exactly: a case-
insensitive prefix check against the container's own already-short
ID (this project's container IDs are already the short, 12-hex-char
form throughout, unlike real podman's own full 64-hex-char one).

## Cross-key vs. same-key combination, checked directly

- Multiple values for the *same* key (`--filter name=a --filter
  name=b`) are OR'd together, same convention `status=` (`0272`)
  already established.
- Different *keys* (`--filter status=running --filter name=foo`) are
  ANDed together — verified directly: `podman ps --filter
  status=running --filter name=<name-of-a-stopped-container>` found
  nothing, even though each condition alone matches a different real
  container.
- Unlike `status=`, giving `id=`/`name=` does **not** override the
  default running-only/`--all` visibility rule on its own — verified
  directly: `podman ps --filter name=<name-of-a-stopped-container>`
  (no `-a`) still hides it, unlike `--filter status=created` (`0272`),
  which does override the default.

## Verified

Integration (`tests/tests/ociman_ps.rs`, three new tests):

- `--filter name=<substring>` matches a container by a substring of
  its own name, finds nothing for a non-matching substring, and (no
  `-a`) still respects the default running-only visibility rule.
- `--filter id=<prefix>` matches by a real prefix of a container's own
  short ID, finds nothing for a non-matching one.
- Different filter keys are ANDed together: `status=running` combined
  with a stopped container's own `name=` finds nothing, while the same
  `name=` combined with the *matching* `status=created` does find it
  (confirming the AND is real, not accidentally hiding everything).

Regression: all 13 pre-existing `ociman_ps.rs` tests still pass (one,
`ps_filter_with_an_unrecognized_key_or_value_is_a_clear_error`,
updated to use a still-genuinely-unsupported key, `label=`, since its
previous example, `name=`, is now supported).

Full workspace: `cargo build`/`test --workspace`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`python3 ci/guards.py`, `cargo deny check`, `ci/native-ci.sh`,
`ci/build-deb.sh`.

## Still ahead

`ociman ps --filter label=`/`label!=` remains deferred pending more
direct verification of its own still-not-fully-understood real
multi-value semantics (`0272`'s own research note). Real podman's own
larger `ps --filter` family (`ancestor`, `before`/`since`, `pod`,
`network`, `restart-policy`, `command`, ...) remains further,
separately-scoped candidates.
</content>
</invoke>
