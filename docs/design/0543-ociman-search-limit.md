# Design note 0543: `ociman search --list-tags --limit`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_search_limit.rs`.

## What this closes

This closes two things at once, both in `ociman search --list-tags`:

1. A missing flag: `--limit`.
2. A real, pre-existing bug independent of that flag: before this
   increment, `cmd_search` printed *every* tag `Client::list_tags`
   fully paginated through, with no cap at all. Real `podman search
   --list-tags` always caps its own result at 25 by default, even with
   no `--limit` given — for any repository with more than 25 tags
   (e.g. `docker.io/library/busybox`, which has hundreds), this
   project's previous output was already observably wrong versus real
   podman's, not merely missing an optional flag.

## Real, checked-directly confirmation

- Flag definition: `~/git/podman/cmd/podman/images/search.go:91-93` --
  `flags.IntVar(&searchOptions.Limit, "limit", 0, "Limit the number of
  results")`, `0` its own real "unset" sentinel.
- Real default cap and truncation logic:
  `~/git/container-libs/common/libimage/search.go` --
  `searchMaxQueries = 25` (line 23); `searchRepositoryTags` (lines
  257-283) fetches the entire, fully-paginated tag list first (via
  `registryTransport.GetRepositoryTags`, the same pagination this
  project's own `Client::list_tags` already ports, `docs/design/
  0371`), then: `limit := min(len(tags), searchMaxQueries); if
  options.Limit != 0 { limit = min(len(tags), options.Limit) }`
  (lines 271-274) — a real, executed truncation, not dead code.
- **Live-verified against a real installed `podman 4.9.3`, not just
  read from source:**
  - `podman search --list-tags docker.io/library/busybox` (no
    `--limit`) → exactly 26 lines (header + 25 tags).
  - `podman search --list-tags --limit 3 ...` → exactly 3 tag rows.
  - `podman search --list-tags --limit 0 ...` (explicit `0`) → same
    26 lines as the default — confirms `0` really is the "unset"
    sentinel, not "zero results."
  - `podman search --list-tags --limit -1 ...` → exit `0`, **completely
    empty stdout, not even the usual `NAME`/`TAG` header row**. Traced
    to `~/git/podman/cmd/podman/images/search.go:158-160`: `if
    len(searchReport) == 0 { return nil }` runs before any printing at
    all. The empty result itself comes from Go's own `for i := range
    limit` iterating zero times for a negative `limit` (real podman's
    own CLI layer never validates `--limit`'s sign either) combined
    with `options.Limit != 0` (a negative value is non-zero, so it
    still overrides the default 25 with itself).

## Implementation

`bin/ociman/src/main.rs`: `limit: i64` (`#[arg(long, default_value_t =
0, allow_negative_numbers = true)]`) added to `Command::Search`.
`allow_negative_numbers` is needed so `--limit -1` (space-separated)
parses as a value rather than clap treating `-1` as an unrecognized
flag — matching the same space-separated syntax real podman's own
pflag-based CLI already accepts without complaint.

`cmd_search` now truncates the already-fully-paginated `tags: Vec
<String>` via `tags.truncate(effective_limit)` (a real no-op when
`effective_limit` is `>=` the actual tag count, so no separate
`min(...)` is needed): `effective_limit` is `DEFAULT_SEARCH_LIMIT`
(`25`, a new const citing `searchMaxQueries`) when `limit == 0`,
otherwise `usize::try_from(limit).unwrap_or(0)` — which naturally
reproduces the real "negative limit yields zero results" quirk above
(a negative `i64` fails the `usize` conversion, falling back to `0`).

One deliberate, documented divergence for the zero-results case: the
plain-text path now matches real podman's exact "print nothing at all,
not even the header" behavior (a real, direct, simple port of the
`if len(searchReport) == 0 { return nil }` check) — but `--json`
still always emits a real, valid JSON value (`[]` for zero tags),
matching every other `ociman` command's own established `--json`
convention (e.g. `ociman images --json` on an empty store prints `[]`,
never nothing) rather than real podman's own quirk of printing
literally nothing even for `--format json` on zero results, which
would make its own stdout invalid JSON. This project's `--json` shape
was already a deliberate, documented narrowing of real podman's own
richer per-entry shape (a plain array of tag strings, not
`{"Name":...,"Tag":...}` objects) — staying internally consistent with
every other command's own "`--json` is always valid JSON" convention
matters more here than chasing a real quirk that only exists because
of real podman's own richer, differently-shaped output path.

## Tests

`tests/tests/ociman_search_limit.rs`, six new integration tests
against a real, local, anonymous plain-HTTP mock registry (the same
minimal `MockRegistry` pattern `ociman_tls_verify.rs` already
established, reused here to serve a real `GET /v2/testrepo/tags/list`
response):

- `search_without_limit_caps_at_the_real_default_of_25` (100 real
  tags on the mock, no `--limit` → header + 25 rows)
- `search_limit_overrides_the_real_default` (`--limit 3` → 3 rows)
- `search_limit_larger_than_available_tags_returns_them_all` (`--limit
  1000` against only 5 real tags → all 5, no padding)
- `search_explicit_limit_zero_behaves_like_the_default`
- `search_negative_limit_yields_zero_results_no_header_in_plain_mode_
  but_valid_json_array`
- `search_json_reports_the_truncated_tag_array`

Manually exercised beyond the automated tests: `ociman search
--list-tags docker.io/library/busybox` (default, `--limit 3`,
`--limit 0`, `--limit -1`, `--limit -1 --json`) against the real,
live Docker Hub registry, each compared directly against the
equivalent real `podman search` invocation on this same host.

## Verification

`cargo build --workspace --locked` (clean), `cargo fmt --all` (clean,
no changes needed for the new test file), `cargo clippy --workspace
--all-targets --locked -- -D warnings` (clean), targeted
`ociman_search.rs`/`ociman_search_limit.rs` runs (2/2, 6/6), a full
`cargo test --workspace --locked` run (clean), `python3 ci/guards.py`
(clean), `cargo deny check` (clean), `bash ci/native-ci.sh` (clean),
`bash ci/build-deb.sh` (clean, real `dpkg -i`/`--version`/`dpkg -r`
round trip). No new registry round trips added (reuses the exact same
already-paginating `Client::list_tags` call, just truncating its
already-fetched result afterward) — no hot path touched, no
`ci/bench.sh` rerun needed.
