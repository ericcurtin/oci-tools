//! `ociman build`'s own local build cache — the piece every design
//! note from 0050 onward has deferred ("the build cache — still
//! nothing actually caches a previous build's own result yet").
//!
//! # Design, adapted from real buildah rather than real BuildKit
//!
//! Real BuildKit (`~/git/moby`'s vendored copy) caches against a
//! general content-addressed DAG of build ops (`solver/cachekey.go`) —
//! a poor fit here, since `ociman build` has no such graph at all,
//! just a straight-line sequential executor over a real, live rootfs
//! (`bin/ociman/src/build.rs`'s own `build_stage`). Real buildah's own
//! builder (`~/git/podman`'s vendored `go.podman.io/buildah/
//! imagebuildah/stage_executor.go`) is the much closer architectural
//! match — also a plain sequential per-instruction executor over a
//! real container filesystem — so this module ports its model
//! directly onto this project's own already-existing
//! `ImageConfig.history`/`rootfs.diff_ids` shape (`crates/
//! oci-dockerfile/src/commit.rs`'s `record_layer`/
//! `record_empty_history`), rather than inventing a new metadata
//! format:
//!
//! * **Candidates are every image already in local storage**
//!   ([`load_candidates`]), read back once per `ociman build`
//!   invocation (`store.list_images()` plus one `image_manifest`/
//!   `image_config` read each) — matching real buildah's own
//!   `intermediateImageExists`, which likewise scans every image in
//!   local storage rather than maintaining a separate cache index.
//! * **A candidate matches at a given point in the current build** if
//!   its own `history` list has a strictly longer prefix than what
//!   the current build has produced so far, that prefix is entry-for-
//!   entry equal (`created_by` and `empty_layer` only — see
//!   [`history_prefix_matches`]'s own doc comment for why `created`/
//!   `author`/`comment` are deliberately not compared), *and* its
//!   next entry's own `created_by` string-equals what the current
//!   instruction would record if it actually ran — this is real
//!   buildah's own `historyAndDiffIDsMatch`, minus the extra `Created`
//!   timestamp/`Comment`/`Author` comparison it also does (redundant
//!   here in practice: those only ever differ for entries this
//!   project's own history-prefix check already requires to be
//!   character-for-character `created_by`-identical, at which point a
//!   *rebuild* of the identical instruction producing a real,
//!   differently-timestamped result the first prefix mismatch already
//!   handles is not a distinction worth chasing).
//! * **A hit reuses the candidate's own already-stored layer**
//!   (`descriptor`/`diff_id`, at the same position [`find_cached_layer`]
//!   matched) instead of re-running the instruction at all — real
//!   startup/teardown of a namespace is real, measured cost this
//!   project's own benchmarks care about (this crate's own top-level
//!   README goal: beat every real equivalent on startup/destroy time
//!   *especially*), so skipping it outright, not just skipping the
//!   file I/O, is the entire point.
//!
//! # `RUN`'s own cache key is its recorded `created_by` text, with any currently-declared `ARG` values folded in
//!
//! No separate signature is computed for `RUN`: its `created_by` is
//! the resolved shell/exec command text (`build.rs`'s own
//! `run_instruction`) — `oci_dockerfile::expand_stage` deliberately
//! never touches `RUN`'s own command-line text at build time (real
//! Docker doesn't either; see its own module doc comment for why), so
//! a literal `RUN echo $VERSION` stays exactly that in `created_by`,
//! never becoming `RUN echo 1.0`. Since 0119, `ARG` values a `RUN`
//! step can actually see (via its own injected process environment,
//! not a text substitution — see `run_step_spec`'s own doc comment)
//! are folded into `created_by` as a real prefix instead (matching
//! real Docker's own `prependEnvOnCmd`, visible in a real `docker
//! history` as `RUN |1 VERSION=1.0 /bin/sh -c ...`): without this, a
//! `--build-arg` override that changes what the exact same `RUN` text
//! would actually produce could otherwise still hash-match an earlier
//! build's own differently-parameterized cache entry and incorrectly
//! reuse its stale layer — matching real Docker's own classic
//! builder, which likewise busts a `RUN` layer's cache on
//! command-text-plus-build-args-plus-parent-chain, with no filesystem
//! content digest of its own to compute (there's no source content to
//! hash for a `RUN` in the first place).
//!
//! # `COPY`/`ADD` fold a real content digest into `created_by`
//!
//! Unlike `RUN`, `COPY`/`ADD`'s own `created_by` text alone (source/
//! dest names, `--from`) says nothing about whether the *bytes* being
//! copied changed since the cached build — real Docker's own classic
//! builder faces the same gap and closes it the same way this module
//! does: folding a real content digest of the copied source tree
//! directly into the recorded `created_by` string itself (real
//! Docker's own convention is visible in a real `docker history`:
//! `COPY dir:1414d0f7... in /app`), rather than inventing a separate,
//! unpersisted side-channel a later build has no way to recompute
//! against ([`content_digest`]). This is computed *before* deciding
//! whether the step is a cache hit (the same real, unavoidable
//! ordering real buildah's own `ContentDigester` has: you cannot know
//! whether copied content changed without reading it), but a hit
//! still skips the (often far more expensive, for a large source
//! tree) actual copy into the rootfs, replaced by extracting the one
//! already-compressed cached layer instead.
//!
//! # What this module deliberately does not do
//!
//! * No `--cache-from`/`--cache-to` remote cache import/export (real
//!   buildah's own `generateCacheKey` for exactly that) — everything
//!   here is local-storage-only, matching this project's own
//!   established "narrow first increment" pattern.
//! * No cache invalidation beyond prefix/text matching — an image
//!   removed from local storage (`ociman rmi`, 0102) simply stops
//!   being a candidate the next time [`load_candidates`] runs; there
//!   is no separate cache-specific pruning of its own.

use std::path::Path;

use oci_spec_types::Digest;
use oci_spec_types::digest::Sha256Writer;
use oci_spec_types::image::{Descriptor, ImageConfig, ImageManifest};
use oci_store::Store;

/// One already-built local image, preloaded once per `ociman build`
/// invocation by [`load_candidates`] and consulted before every
/// `RUN`/`COPY`/`ADD` instruction via [`find_cached_layer`].
pub struct CacheCandidate {
    config: ImageConfig,
    manifest: ImageManifest,
}

/// Every image currently in local storage, as a cache candidate —
/// read once up front (not re-read per instruction) since neither the
/// store nor any of its images change during a single `ociman build`
/// invocation until the very end (`cmd_build`'s own final
/// `store.put_image`). An image whose manifest/config can't be read
/// back (a corrupt or foreign entry) is silently skipped rather than
/// failing the whole build — a build cache is a pure optimization, so
/// its own failure to load one candidate is never a reason to error
/// out of an otherwise-successful build.
pub fn load_candidates(store: &Store) -> Vec<CacheCandidate> {
    store
        .list_images()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|record| {
            let manifest = store.image_manifest(&record).ok()?;
            let config = store.image_config(&record).ok()?;
            Some(CacheCandidate { config, manifest })
        })
        .collect()
}

/// Whether `candidate`'s own history has a prefix that exactly
/// matches `current` (the current build's own `config.history` so
/// far, at whatever length it happens to be at this point). Compares
/// only `created_by`/`empty_layer` — not `created`/`author`/`comment`
/// — see this module's own top-level doc comment for why that's
/// sufficient here.
fn history_prefix_matches(
    candidate: &ImageConfig,
    current: &[oci_spec_types::image::HistoryEntry],
) -> bool {
    if candidate.history.len() <= current.len() {
        return false;
    }
    candidate.history[..current.len()]
        .iter()
        .zip(current)
        .all(|(a, b)| a.created_by == b.created_by && a.empty_layer == b.empty_layer)
}

/// A cache hit: everything [`reuse_cached_layer`][crate::build::
/// reuse_cached_layer] (`build.rs`) needs to splice an already-stored
/// layer into the build in progress, unmodified.
pub struct CachedLayer {
    /// Append this to the manifest's own `layers` list, exactly as
    /// [`oci_dockerfile::record_layer`] would for a freshly-committed
    /// layer.
    pub descriptor: Descriptor,
    /// Append this to `rootfs.diff_ids`, same as `descriptor` above.
    pub diff_id: Digest,
    /// The candidate's own already-recorded history entry for this
    /// step, reused verbatim (including its own original `created`
    /// timestamp) rather than fabricating a new one timestamped now —
    /// nothing new was actually created at this build's own wall-clock
    /// time, so there is no truer timestamp to record than the one
    /// from whenever this layer really was built.
    pub history_entry: oci_spec_types::image::HistoryEntry,
}

/// Look for a candidate whose own history matches the current build's
/// progress so far (`current_history`, `current_layer_count`
/// non-empty layers deep) and whose next history entry is exactly
/// `created_by` — the one already computed (with a content digest
/// folded in for `COPY`/`ADD`, see [`content_digest`]) for the
/// instruction about to run. Returns the already-stored layer to
/// reuse verbatim, skipping the instruction entirely, on a hit.
///
/// Candidates are tried in [`load_candidates`]'s own order (the
/// store's own `list_images` order); the *first* match wins, matching
/// real buildah's "most recently created" tie-break only loosely (this
/// project's own `oci_store::images::list` has no creation-time
/// ordering of its own yet to pick from) — any match is equally valid
/// since a cache hit's whole point is "this would produce
/// byte-identical output", not "this is the newest such output".
pub fn find_cached_layer(
    candidates: &[CacheCandidate],
    current_history: &[oci_spec_types::image::HistoryEntry],
    current_layer_count: usize,
    created_by: &str,
) -> Option<CachedLayer> {
    candidates.iter().find_map(|candidate| {
        if !history_prefix_matches(&candidate.config, current_history) {
            return None;
        }
        let next = &candidate.config.history[current_history.len()];
        if next.empty_layer || next.created_by.as_deref() != Some(created_by) {
            return None;
        }
        let descriptor = candidate.manifest.layers.get(current_layer_count)?.clone();
        let diff_id = candidate
            .config
            .rootfs
            .diff_ids
            .get(current_layer_count)?
            .clone();
        Some(CachedLayer {
            descriptor,
            diff_id,
            history_entry: next.clone(),
        })
    })
}

/// A real content digest of every one of `sources` (each already
/// resolved to a real path under `source_root`, in the given order —
/// deterministic across repeated builds provided the source tree
/// itself is unchanged, since glob expansion is already sorted by the
/// caller), folded into `COPY`/`ADD`'s own recorded `created_by` text
/// so a cache lookup can tell whether the actual bytes being copied
/// changed since a candidate was built — matching real buildah's own
/// `generatePathChecksum`/`ContentDigester` in spirit (a directory's
/// own full recursive content, not just its top-level listing), if
/// not its exact on-wire byte format (that format is this module's
/// own implementation detail, never compared against any other real
/// tool's own digest).
///
/// Regular file content, a symlink's own target, and each entry's own
/// path relative to `source_root` (so renaming or moving a source
/// invalidates the cache exactly like real docker's own equivalent
/// digest does) all feed the hash; permission bits deliberately don't
/// (they never affect a plain `COPY`/`ADD`'s own copied-file mode
/// unless `--chmod` overrides it, which is folded into `created_by`
/// separately, see `build.rs`'s own `copy_instruction`/
/// `add_instruction`).
pub fn content_digest(source_root: &Path, sources: &[String]) -> anyhow::Result<Digest> {
    let mut hasher = Sha256Writer::new();
    for source in sources {
        let path = crate::build::safe_join(source_root, source.trim_start_matches('/'))?;
        hash_path(&path, source, &mut hasher)?;
    }
    Ok(hasher.finish_digest())
}

/// Recursive helper for [`content_digest`]: writes a stable,
/// self-delimiting byte stream describing `path` (labeled `label`,
/// its own path relative to `source_root`) into `hasher`. Directory
/// entries are visited in sorted order (matching `build.rs`'s own
/// `expand_wildcard_source`'s established "lexical order" convention
/// elsewhere in this same build executor) so the digest never depends
/// on a directory's own arbitrary on-disk readdir order.
fn hash_path(path: &Path, label: &str, hasher: &mut Sha256Writer) -> anyhow::Result<()> {
    use std::io::Write as _;

    let metadata = std::fs::symlink_metadata(path)
        .with_context(path, "reading metadata for content digest")?;
    if metadata.file_type().is_symlink() {
        let target = std::fs::read_link(path).with_context(path, "reading symlink target")?;
        writeln!(hasher, "L {label} -> {}", target.display())?;
    } else if metadata.is_dir() {
        writeln!(hasher, "D {label}")?;
        let mut entries: Vec<_> = std::fs::read_dir(path)
            .with_context(path, "reading directory")?
            .collect::<std::io::Result<_>>()
            .with_context(path, "reading directory entry")?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let child_label = format!("{label}/{}", entry.file_name().to_string_lossy());
            hash_path(&entry.path(), &child_label, hasher)?;
        }
    } else {
        let bytes = std::fs::read(path).with_context(path, "reading file content")?;
        writeln!(hasher, "F {label} {}", bytes.len())?;
        hasher.write_all(&bytes)?;
    }
    Ok(())
}

/// Tiny local `anyhow::Context`-equivalent for a `std::io::Result`,
/// spelled out longhand rather than pulling in `anyhow::Context` here
/// too: [`hash_path`] is the only caller, and needs the path in every
/// message anyway.
trait WithPathContext<T> {
    fn with_context(self, path: &Path, what: &str) -> anyhow::Result<T>;
}

impl<T> WithPathContext<T> for std::io::Result<T> {
    fn with_context(self, path: &Path, what: &str) -> anyhow::Result<T> {
        self.map_err(|e| anyhow::anyhow!("{what} for {}: {e}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oci_spec_types::image::{HistoryEntry, RootFs};

    fn history_entry(created_by: &str, empty_layer: bool) -> HistoryEntry {
        HistoryEntry {
            created: Some("2026-01-01T00:00:00Z".to_string()),
            created_by: Some(created_by.to_string()),
            author: None,
            comment: None,
            empty_layer,
        }
    }

    fn layer_descriptor(seed: &str) -> Descriptor {
        Descriptor {
            media_type: oci_spec_types::image::MEDIA_TYPE_IMAGE_LAYER_GZIP.to_string(),
            digest: digest(seed),
            size: 1,
            urls: vec![],
            annotations: Default::default(),
            platform: None,
        }
    }

    fn digest(seed: &str) -> Digest {
        oci_spec_types::digest::sha256(seed.as_bytes())
    }

    /// A minimal but real [`CacheCandidate`]: `history` is the
    /// candidate's own full history (empty-layer entries included),
    /// `layer_seeds` one arbitrary distinguishing seed per *non*-
    /// empty-layer entry, in the same relative order (mirroring how
    /// `record_layer` only ever appends to `rootfs.diff_ids`/
    /// `manifest.layers` for a real layer-producing instruction).
    fn candidate(history: Vec<HistoryEntry>, layer_seeds: &[&str]) -> CacheCandidate {
        let diff_ids: Vec<Digest> = layer_seeds.iter().map(|s| digest(s)).collect();
        let layers: Vec<Descriptor> = layer_seeds.iter().map(|s| layer_descriptor(s)).collect();
        CacheCandidate {
            config: ImageConfig {
                rootfs: RootFs {
                    kind: "layers".to_string(),
                    diff_ids,
                },
                history,
                ..Default::default()
            },
            manifest: ImageManifest {
                schema_version: 2,
                media_type: None,
                config: layer_descriptor("config"),
                layers,
                annotations: Default::default(),
            },
        }
    }

    #[test]
    fn empty_candidate_history_never_matches() {
        assert!(!history_prefix_matches(
            &ImageConfig::default(),
            &[history_entry("RUN a", false)]
        ));
    }

    #[test]
    fn identical_prefix_matches() {
        let current = vec![history_entry("RUN a", false)];
        let candidate_config = ImageConfig {
            history: vec![history_entry("RUN a", false), history_entry("RUN b", false)],
            ..Default::default()
        };
        assert!(history_prefix_matches(&candidate_config, &current));
    }

    #[test]
    fn diverging_prefix_does_not_match() {
        let current = vec![history_entry("RUN a", false)];
        let candidate_config = ImageConfig {
            history: vec![
                history_entry("RUN a-different", false),
                history_entry("RUN b", false),
            ],
            ..Default::default()
        };
        assert!(!history_prefix_matches(&candidate_config, &current));
    }

    #[test]
    fn find_cached_layer_hits_the_first_missing_instruction() {
        let candidates = vec![candidate(
            vec![history_entry("RUN a", false), history_entry("RUN b", false)],
            &["layer-a", "layer-b"],
        )];
        let hit = find_cached_layer(&candidates, &[history_entry("RUN a", false)], 1, "RUN b")
            .expect("RUN b should hit the candidate's own second layer");
        assert_eq!(hit.descriptor.digest, digest("layer-b"));
        assert_eq!(hit.diff_id, digest("layer-b"));
        assert_eq!(hit.history_entry.created_by.as_deref(), Some("RUN b"));
    }

    #[test]
    fn find_cached_layer_misses_on_different_created_by() {
        let candidates = vec![candidate(vec![history_entry("RUN a", false)], &["layer-a"])];
        assert!(find_cached_layer(&candidates, &[], 0, "RUN a-different").is_none());
    }

    #[test]
    fn find_cached_layer_misses_on_shorter_candidate_history() {
        let candidates = vec![candidate(vec![history_entry("RUN a", false)], &["layer-a"])];
        assert!(
            find_cached_layer(
                &candidates,
                &[history_entry("RUN a", false)],
                1,
                "RUN b (not in any candidate)"
            )
            .is_none()
        );
    }

    #[test]
    fn find_cached_layer_never_matches_an_empty_layer_entry() {
        // A candidate whose *next* entry is an empty-layer one (e.g.
        // an `ENV` between two `RUN`s) must never be mistaken for a
        // real, layer-producing match, even if its own `created_by`
        // text happened to collide.
        let candidates = vec![candidate(
            vec![history_entry("RUN a", false), history_entry("RUN b", true)],
            &["layer-a"],
        )];
        assert!(
            find_cached_layer(&candidates, &[history_entry("RUN a", false)], 1, "RUN b").is_none()
        );
    }

    #[test]
    fn load_candidates_reads_back_real_stored_images() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let config = ImageConfig::default();
        let config_bytes = serde_json::to_vec(&config).unwrap();
        let config_ingested = store.ingest(&config_bytes[..]).unwrap();
        let manifest = ImageManifest {
            schema_version: 2,
            media_type: None,
            config: Descriptor {
                media_type: oci_spec_types::image::MEDIA_TYPE_IMAGE_CONFIG.to_string(),
                digest: config_ingested.digest,
                size: config_ingested.size,
                urls: vec![],
                annotations: Default::default(),
                platform: None,
            },
            layers: vec![],
            annotations: Default::default(),
        };
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        let manifest_ingested = store.ingest(&manifest_bytes[..]).unwrap();
        store
            .put_image(&oci_store::ImageRecord {
                reference: "docker.io/library/example:latest".to_string(),
                manifest_digest: manifest_ingested.digest,
            })
            .unwrap();

        let candidates = load_candidates(&store);
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].config.history.is_empty());
    }

    #[test]
    fn content_digest_changes_when_file_content_changes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"one").unwrap();
        let first = content_digest(dir.path(), &["a.txt".to_string()]).unwrap();

        std::fs::write(dir.path().join("a.txt"), b"two").unwrap();
        let second = content_digest(dir.path(), &["a.txt".to_string()]).unwrap();

        assert_ne!(first, second);
    }

    #[test]
    fn content_digest_is_stable_for_unchanged_content() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/a.txt"), b"content").unwrap();
        let first = content_digest(dir.path(), &["sub".to_string()]).unwrap();
        let second = content_digest(dir.path(), &["sub".to_string()]).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn content_digest_changes_when_a_file_is_renamed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"content").unwrap();
        let first = content_digest(dir.path(), &["a.txt".to_string()]).unwrap();

        std::fs::rename(dir.path().join("a.txt"), dir.path().join("b.txt")).unwrap();
        let second = content_digest(dir.path(), &["b.txt".to_string()]).unwrap();

        assert_ne!(first, second);
    }
}
