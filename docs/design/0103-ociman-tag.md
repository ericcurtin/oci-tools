# Design note 0103: `ociman tag` (milestone 2/3)

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Tag`, `cmd_tag`, `TagResult`),
`tests/tests/ociman_tag.rs`.

Following 0102's `ociman rmi`, `tag` was the other conspicuously
missing basic image-management command: every real `docker`/`podman`
workflow that builds or pulls an image under one name and re-tags it
for a registry push (`docker tag myimage myregistry/myimage:v1`) had
no equivalent here at all. Picked as this session's own next
increment for the same reason 0102 was: genuinely useful, narrowly
scoped, and needed zero new store-layer plumbing — `oci_store::
Store::put_image` already does exactly the one write this needs.

## No new machinery at all — same insight as 0074's own external-image `COPY --from=`

`ociman build`'s own final step (`cmd_build`) already tags its result
by calling `store.put_image(&ImageRecord { reference: tag, manifest_
digest })` directly against an already-known digest. `cmd_tag` is
*exactly* that same call, just resolving the manifest digest from an
existing `resolve_image` lookup (`source`) instead of a build's own
freshly-ingested manifest — no blob is read, copied, or even opened:
this project's own store is content-addressed, so two references
pointing at the same manifest digest is the entire meaning of "the
same image, two names" (`docker tag`'s own documented behavior:
"creates a new tag for the same underlying image, no new blobs").

`target` silently overwrites whatever `target` used to point at, same
as both real tools (`store.put_image` was already a create-or-overwrite
upsert, so this needed no special-casing either) — verified by
`tag_overwrites_an_existing_target_reference`.

## Real, automated tests

`tests/tests/ociman_tag.rs`: tagging a real seeded image under a
second reference and confirming, on the real on-disk store (not just
the CLI's own exit code), that the new reference resolves to the exact
same manifest digest, the original reference is untouched, and the new
tag is independently usable by `ociman run`; a clear error for an
unknown source; overwriting an existing target reference (pointing it
at a different image, then confirming the retag actually moved it);
and the `--json` output's own canonical `source`/`target` fields.

## Performance

No hot path touched — tagging is an infrequent, offline metadata
operation (one JSON pointer file written), not part of any
startup/destroy-time benchmark this project's own README goal cares
about.
