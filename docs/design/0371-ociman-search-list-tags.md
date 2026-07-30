# Design note 0371: `ociman search --list-tags`

Status: implemented
Scope: `crates/oci-registry/src/client.rs`, `crates/oci-spec-types/src/reference.rs`,
`bin/ociman/src/main.rs`, `tests/tests/ociman_search.rs`.

## What this closes

`oci-registry` had no tag-listing primitive at all: `pull_manifest`/
`pull_blob`/`upload_blob`/`push_manifest`/`blob_exists` cover the
manifest/blob paths, but nothing for `GET /v2/<name>/tags/list` — the
real distribution-spec v2 endpoint real `podman search --list-tags`
(and `skopeo list-tags`) actually use.

## Real, checked-directly semantics

Read `~/git/container-libs/image/docker/docker_image.go`'s own
`GetRepositoryTags` directly: a plain, already-authenticated (same
bearer-token flow every other request here already uses) `GET`, a
`{"tags": [...]}` JSON body, and — a real behavior found by reading
rather than assumed — genuine `Link`-header pagination (RFC 5988-
adjacent): the client keeps following whatever URL the header names
until the registry stops sending one, so a large repository's own tag
list is never silently truncated to just the first page. Two real,
checked-directly registry quirks that same real client specifically
tolerates are ported too: a JSON `null` tag entry (cited: some
Sonatype Nexus versions) and a bare digest string standing in for a
tag (cited: some Artifactory versions) are both silently skipped
rather than surfaced as parse errors.

Also read `~/git/podman/cmd/podman/images/search.go` directly for the
CLI-level semantics: `TERM`'s own tag/digest, if any, is ignored
entirely (only the repository matters); `podman search TERM` *without*
`--list-tags` is a fundamentally different, free-text search against
every configured registry, using Docker Hub's own separate, largely-
deprecated v1 search API (`GET https://index.docker.io/v1/search?q=`)
— a genuinely different protocol from the v2 distribution spec this
crate already speaks, plus a "configured search registries"
(`registries.conf`-style) concept this project has no equivalent of at
all. Confirmed empirically against a real installed `podman 4.9.3`:
plain `podman search busybox` (no explicit registry) returns nothing
at all on this host (no default search registries configured), while
`podman search --list-tags docker.io/library/busybox` works and
prints `NAME\tTAG` rows with the bare repository name (no implied
`:latest`) repeated per tag.

## Implementation

New `Client::list_tags(reference: &Reference) -> Result<Vec<String>,
RegistryError>` in `oci-registry`, following the exact pagination
loop and the two quirk-tolerances above; a new small module-level
`next_tags_page_path` helper parses a `Link` header down to just the
next page's own path+query (deliberately as simple as the real
reference client's own parsing — no `rel="next"` check at all, just
whatever URL the first `<...>` segment names, always resolved against
the *same* host the first request used, matching that real client's
own identical choice).

New `Reference::familiar_repository()` (refactored out of
`familiar()`, which already computed it internally): the shortened,
user-familiar repository name alone, with no `:tag`/`@digest` suffix
at all — needed so `ociman search --list-tags`'s own per-tag rows
show the bare repository name, matching real podman's own checked-
directly output exactly, rather than a plain `familiar()` call's own
misleading implied `:latest`.

New `Command::Search { term, list_tags, tls_verify }`; `cmd_search`
rejects a bare (non-`--list-tags`) search immediately with a clear,
honest "not supported yet" error — before ever attempting
`Reference::parse` or any network I/O at all, so an unresolvable
`TERM` never even risks a slow DNS timeout for a mode this project
doesn't implement. `--list-tags` reuses `oci_registry::client_for`
(the exact same `--tls-verify`-aware client constructor `ociman
pull`/`push` already use) and prints `NAME\tTAG` (or `--json` for a
plain array of tag strings — a real, deliberate narrowing of real
podman's own richer per-entry JSON shape, `{"Name": ..., "Tag": ...}`
repeated per entry with `Name` never actually varying for this
project's own single-repository case, not worth reproducing).

## Verified

New unit tests in `crates/oci-registry/src/client.rs`:
`next_tags_page_path_reduces_an_absolute_url_to_just_path_and_query`;
`next_tags_page_path_keeps_a_bare_path_as_is`;
`next_tags_page_path_is_none_for_a_malformed_header`;
`list_tags_follows_link_header_pagination_and_filters_bad_entries` (a
real two-page mock, page one carrying a `Link` header plus a JSON
`null`/empty-string/bare-digest entry all needing to be filtered,
page two having none — confirming pagination genuinely stops there).
New unit test in `crates/oci-spec-types/src/reference.rs`:
`familiar_repository_drops_any_tag_or_digest_suffix`. New tests in
`tests/tests/ociman_search.rs` (CLI-surface, no-network-needed
coverage, matching `ociman push`'s own `0127` precedent for where the
real network round trip is verified instead):
`search_without_list_tags_is_a_clear_error_before_any_network_attempt`;
`search_list_tags_of_an_empty_term_is_a_clear_error`. A real,
manually-verified end-to-end round trip against a real, live
`docker.io/library/busybox` repository (hundreds of tags, spanning
multiple real pagination pages) confirmed both the plain-text and
`--json` output shapes during this feature's own development.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures, full clean
run, no flakes), `python3 ci/guards.py`, `cargo deny check`, `bash
ci/native-ci.sh`, `bash ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).
