//! Resolving a user- or caller-supplied string to a stored image: as an
//! ordinary tag/digest reference, or as a real or short image ID (a hex
//! prefix of a manifest digest) — plus the "untagged image" sentinel
//! reference convention every one of those helpers, and several other
//! `Store` callers, need to agree on.
//!
//! Moved here from `ociman`-private code (`resolve_image_by_reference_or_id`/
//! `resolve_image_by_id_only`, 0122/0179) once `ocicri`'s own `ImageService`
//! needed the identical logic too — CRI's `ImageSpec.image` field is
//! routinely a bare digest/ID (`PullImageResponse.image_ref`/`Image.id`),
//! not just a tag — a real, verified-zero-behavior-change extraction (every
//! one of `ociman`'s own existing tests exercising this logic continues to
//! pass completely unmodified against the shared version).

use crate::{ImageRecord, Store, StoreError};

/// A stored image has no separate "this image has no tag" field of its
/// own — a bare digest string (e.g. `sha256:<hex>`) is used as
/// [`ImageRecord::reference`] instead for an untagged image, safe
/// because it can never collide with a real one: every real,
/// [`oci_spec_types::Reference::parse`]-derived reference's own
/// `Display` always writes `<registry>/<repository>...`, so it always
/// contains at least one `/`, which a bare digest string never does.
pub fn untagged_reference(digest: &oci_spec_types::Digest) -> String {
    digest.to_string()
}

/// Whether `reference` (an [`ImageRecord`]'s own field) is
/// [`untagged_reference`]'s own sentinel rather than a real tag — see
/// its own doc comment for why a bare digest string (no `/` at all)
/// can never be a real one.
pub fn is_untagged_reference(reference: &str) -> bool {
    !reference.contains('/')
}

/// Which of the two ways [`resolve_by_reference_or_id`] matched `spec`
/// — callers that need to know (like `ociman rmi`'s own "removing *by
/// ID* with more than one tag needs `--force`" policy, matching real
/// `podman rmi`'s own identical rule) inspect this; ones that don't
/// (like `ociman inspect`, which only ever reads) can just call
/// [`ResolvedImage::record`] and ignore which arm it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedImage {
    /// `spec` was itself an existing tag reference.
    Tag(ImageRecord),
    /// `spec` didn't match any tag; resolved via a real or short image
    /// ID fallback instead.
    Id(ImageRecord),
}

impl ResolvedImage {
    /// The resolved record, regardless of which way it matched.
    pub fn record(&self) -> &ImageRecord {
        match self {
            ResolvedImage::Tag(record) | ResolvedImage::Id(record) => record,
        }
    }
}

/// Resolve `spec` to a stored image record: first as an ordinary tag
/// reference (the overwhelmingly common case), then, if that fails, as
/// a real or short image ID — a hex prefix of its own manifest digest,
/// no `sha256:` prefix required — matching real `docker inspect
/// a1b2c3d4`/`podman inspect a1b2c3d4`'s own convention exactly.
/// Deduplicated by the *real* underlying digest, not by tag count: two
/// tags pointing at the exact same image never make an ID prefix
/// ambiguous — only two genuinely *different* images that happen to
/// share a digest prefix do (see [`resolve_by_id_only`]'s own doc
/// comment).
pub fn resolve_by_reference_or_id(
    store: &Store,
    spec: &str,
) -> Result<Option<ResolvedImage>, StoreError> {
    if let Ok(reference) = oci_spec_types::Reference::parse(spec)
        && let Some(record) = store.resolve_image(&reference.to_string())?
    {
        return Ok(Some(ResolvedImage::Tag(record)));
    }
    Ok(resolve_by_id_only(store, spec)?.map(ResolvedImage::Id))
}

/// The real-or-short-image-ID half of [`resolve_by_reference_or_id`],
/// split out so a caller that needs the *opposite* ordering (ID first,
/// tag/pull-policy second — `ociman`'s own `prepare_container`, where
/// trying a tag first would mean a real, wasted network round-trip for
/// the common "run/create by ID" case: an ID almost always also parses
/// as *some* syntactically valid but nonsense tag reference) can call
/// this directly, with no tag lookup of its own at all. Real tag
/// references essentially never accidentally match the hex-only filter
/// below (matches real docker/podman's own established "ID resolution
/// basically never collides with a real name" precedent).
pub fn resolve_by_id_only(store: &Store, spec: &str) -> Result<Option<ImageRecord>, StoreError> {
    let candidate = spec
        .strip_prefix("sha256:")
        .unwrap_or(spec)
        .to_ascii_lowercase();
    if candidate.is_empty()
        || candidate.len() > 64
        || !candidate.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Ok(None);
    }

    let mut by_digest: std::collections::HashMap<String, ImageRecord> =
        std::collections::HashMap::new();
    for record in store.list_images()? {
        if record.manifest_digest.hex().starts_with(&candidate) {
            // When the exact same image has more than one record (real
            // tags, or the untagged sentinel), a real tag always wins
            // over the sentinel here, deterministically -- a caller
            // like `ociman push`'s own "no real reference to push"
            // refusal reads `.reference` off whichever record this
            // returns, so an image that's *also* been given a real tag
            // (`ociman tag <id> ...`) alongside its own original
            // untagged record must never have that guard trip just
            // because `list_images`'s own iteration order happened to
            // visit the sentinel first.
            let hex = record.manifest_digest.hex().to_string();
            match by_digest.entry(hex) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(record);
                }
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    if is_untagged_reference(&entry.get().reference)
                        && !is_untagged_reference(&record.reference)
                    {
                        entry.insert(record);
                    }
                }
            }
        }
    }
    match by_digest.len() {
        0 => Ok(None),
        1 => Ok(by_digest.into_values().next()),
        count => Err(StoreError::AmbiguousId {
            spec: spec.to_string(),
            count,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oci_spec_types::digest::sha256;

    fn temp_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        (dir, store)
    }

    #[test]
    fn untagged_reference_is_recognized_by_is_untagged_reference() {
        let digest = sha256(b"some manifest bytes");
        let sentinel = untagged_reference(&digest);
        assert!(is_untagged_reference(&sentinel));
    }

    #[test]
    fn is_untagged_reference_rejects_a_real_reference() {
        let reference = oci_spec_types::Reference::parse("docker.io/library/busybox:latest")
            .unwrap()
            .to_string();
        assert!(!is_untagged_reference(&reference));
    }

    #[test]
    fn resolve_by_reference_or_id_finds_a_real_tag_first() {
        let (_dir, store) = temp_store();
        let digest = sha256(b"manifest-a");
        store
            .put_image(&ImageRecord {
                reference: "docker.io/library/foo:latest".to_string(),
                manifest_digest: digest.clone(),
            })
            .unwrap();

        let resolved = resolve_by_reference_or_id(&store, "docker.io/library/foo:latest")
            .unwrap()
            .unwrap();
        assert!(matches!(resolved, ResolvedImage::Tag(_)));
        assert_eq!(resolved.record().manifest_digest, digest);
    }

    #[test]
    fn resolve_by_reference_or_id_falls_back_to_a_short_id() {
        let (_dir, store) = temp_store();
        let digest = sha256(b"manifest-b");
        store
            .put_image(&ImageRecord {
                reference: untagged_reference(&digest),
                manifest_digest: digest.clone(),
            })
            .unwrap();

        let short_id = &digest.hex()[..12];
        let resolved = resolve_by_reference_or_id(&store, short_id)
            .unwrap()
            .unwrap();
        assert!(matches!(resolved, ResolvedImage::Id(_)));
        assert_eq!(resolved.record().manifest_digest, digest);
    }

    #[test]
    fn resolve_by_id_only_prefers_a_real_tag_over_the_untagged_sentinel() {
        let (_dir, store) = temp_store();
        let digest = sha256(b"manifest-c");
        store
            .put_image(&ImageRecord {
                reference: untagged_reference(&digest),
                manifest_digest: digest.clone(),
            })
            .unwrap();
        store
            .put_image(&ImageRecord {
                reference: "docker.io/library/tagged:latest".to_string(),
                manifest_digest: digest.clone(),
            })
            .unwrap();

        let record = resolve_by_id_only(&store, &digest.hex()[..12])
            .unwrap()
            .unwrap();
        assert_eq!(record.reference, "docker.io/library/tagged:latest");
    }

    #[test]
    fn resolve_by_id_only_is_ambiguous_across_two_different_images() {
        let (_dir, store) = temp_store();
        let digest_a = sha256(b"seed-a");
        // A single hex character (the lookup `candidate` below) always
        // matches *some* other digest within a handful of tries --
        // deterministic, no flake risk: this loop always terminates
        // (16 possible leading hex digits, each roughly equally
        // likely).
        let candidate = digest_a.hex()[..1].to_string();
        let mut digest_b = sha256(b"seed-b");
        let mut attempt = 0u32;
        while !digest_b.hex().starts_with(&candidate) {
            attempt += 1;
            digest_b = sha256(format!("seed-b-retry-{attempt}").as_bytes());
        }

        store
            .put_image(&ImageRecord {
                reference: untagged_reference(&digest_a),
                manifest_digest: digest_a,
            })
            .unwrap();
        store
            .put_image(&ImageRecord {
                reference: untagged_reference(&digest_b),
                manifest_digest: digest_b,
            })
            .unwrap();

        let err = resolve_by_id_only(&store, &candidate).unwrap_err();
        assert!(
            matches!(err, StoreError::AmbiguousId { count: 2, .. }),
            "{err:?}"
        );
    }

    #[test]
    fn resolve_by_id_only_returns_none_for_a_non_hex_looking_spec() {
        let (_dir, store) = temp_store();
        assert!(
            resolve_by_id_only(&store, "not-hex-at-all!!")
                .unwrap()
                .is_none()
        );
    }
}
