//! The real `ImageService` gRPC implementation — every RPC is now
//! genuinely implemented: `ListImages`/`ImageStatus`/`PullImage`/
//! `RemoveImage`/`ImageFsInfo`, reusing this project's own
//! already-tested `oci_store`/`oci_registry` primitives directly (the
//! same ones `ociman images`/`ociman inspect`/`ociman pull` already
//! use) rather than anything new, plus `StreamImages` (the
//! `CRIListStreaming` streaming sibling of `ListImages` — 0213's own
//! guess that real `cri-o` "may not even implement it" was re-checked
//! directly and found outdated, see `docs/design/0234`: cri-o's own
//! `image_list.go` genuinely implements it, and so does this now,
//! sharing the exact same filtered-list computation).
//!
//! Behavior checked directly against real `cri-o`'s own
//! implementation (`~/git/cri-o/server/image_list.go`/`image_status.go`/
//! `image_pull.go`/`image_remove.go`): a filter naming one specific
//! image resolves just that one (0 or 1 results, never an error for
//! "not found"); `ImageStatus` of an unresolvable image returns an
//! empty response (`image: None`), not an error either — only a
//! request naming no image at all is a real error; `PullImage` is
//! always unconditional (no pull-policy concept at the CRI layer at
//! all, unlike `ociman run --pull`); `RemoveImage` of an already-
//! removed (or never-existing) image is a real, silent, idempotent
//! success, matching the real proto's own documented contract
//! ("must not return an error if the image has already been
//! removed"), and — a genuinely *different* rule than `ociman rmi`'s
//! own, checked directly against `removeImage`'s own real `UntagImage`
//! path and the proto's own doc comment on `RemoveImage`
//! ("removing the image by a single tag will remove all of its tags,
//! even across different repositories") — removing *any one* tag or
//! ID resolving to an image removes *every* tag/reference sharing
//! that same manifest digest, unconditionally, with no `--force`-
//! style ambiguity gate at all (this RPC has no interactive
//! confirmation to skip in the first place, the same "nothing to
//! skip" reasoning this project's own `ocibox rm`/`ephemeral` already
//! established for an identical reason).
//!
//! Real `cri-o`'s own `RemoveImage` additionally refuses to remove an
//! image any container still references (`volumeInUse`) — not ported
//! here: this project's own `ocicri` can't create any container via
//! CRI at all yet (every `RuntimeService` pod-sandbox/container RPC is
//! still a real, honest `Status::unimplemented`), so there is
//! currently no possible "in use by a real CRI container" case to
//! even check against.
//!
//! `PullImage`'s own real, unconditional pull runs on a
//! `tokio::task::spawn_blocking` thread, not directly in its own
//! `async fn` — this project's own registry client (`oci_registry`,
//! shared unchanged with every other binary) is a plain, synchronous,
//! blocking `ureq`-based client throughout; running it directly on a
//! tokio worker thread would block that whole worker for the entire
//! real network round trip, starving every other RPC this server is
//! also supposed to be answering concurrently in the meantime.

use std::collections::BTreeSet;

use tonic::{Request, Response, Status};

use crate::cri;

/// The real `ImageService` state — like [`crate::runtime_service::
/// RuntimeServiceImpl`], holds nothing of its own: [`oci_store::Store`]
/// is cheap to open fresh per request (a couple of `create_dir_all`
/// calls, no real state to cache yet) and every read here is already
/// safe for concurrent access (plain filesystem reads, the same
/// `Store` every other binary already shares).
#[derive(Debug, Default)]
pub struct ImageServiceImpl;

fn open_store() -> Result<oci_store::Store, Status> {
    oci_store::Store::open(oci_cli_common::storage::default_root())
        .map_err(|e| Status::internal(format!("opening image storage: {e}")))
}

fn store_error(context: &str, e: oci_store::StoreError) -> Status {
    Status::internal(format!("{context}: {e}"))
}

/// `(uid, username)` from an image's own declared `ContainerConfig`
/// user string — matching real `cri-o`'s own identical
/// `getUserFromImage` logic exactly (checked directly,
/// `server/image_status.go`): no user declared -> both empty; a
/// `user:group` form only ever looks at the user half; numeric ->
/// `uid`; otherwise -> `username`.
fn uid_and_username(user: Option<&str>) -> (Option<i64>, String) {
    let Some(user) = user.filter(|u| !u.is_empty()) else {
        return (None, String::new());
    };
    let user = user.split(':').next().unwrap_or(user);
    match user.parse::<i64>() {
        Ok(uid) => (Some(uid), String::new()),
        Err(_) => (None, user.to_string()),
    }
}

/// Builds a real CRI `Image` message from every stored pointer sharing
/// one real manifest digest (`group`'s own first record is used for
/// the manifest/config reads — every record in a group resolves to
/// the identical manifest by construction). `repo_tags`: every real
/// (non-sentinel, see `oci_store::is_untagged_reference`) reference in
/// the group. `repo_digests`: one `<registry>/<repository>@<digest>`
/// entry per distinct repository among those same tags — matching
/// real `cri-o`'s own fallback for a storage backend with no
/// separately-tracked digest references (`ConvertImage`'s own
/// `from.PreviousName + "@" + Digest"` case), the same shape this
/// project's own store actually has (a `RepoDigests` entry isn't
/// tracked as its own thing, only via a real, resolvable tag).
fn build_image(
    store: &oci_store::Store,
    group: &[oci_store::ImageRecord],
) -> Result<cri::Image, Status> {
    let record = &group[0];
    let summary = store
        .image_summary(record)
        .map_err(|e| store_error("reading image summary", e))?;
    let manifest = store
        .image_manifest(record)
        .map_err(|e| store_error("reading image manifest", e))?;
    let config = store
        .image_config(record)
        .map_err(|e| store_error("reading image config", e))?;
    let container_config = config.config.unwrap_or_default();

    let digest_string = summary.manifest_digest.to_string();
    let mut repo_tags = Vec::new();
    let mut repo_digests = BTreeSet::new();
    for r in group {
        if oci_store::is_untagged_reference(&r.reference) {
            continue;
        }
        repo_tags.push(r.reference.clone());
        if let Ok(reference) = oci_spec_types::Reference::parse(&r.reference) {
            repo_digests.insert(format!(
                "{}/{}@{digest_string}",
                reference.registry(),
                reference.repository()
            ));
        }
    }
    repo_tags.sort();

    let (uid, username) = uid_and_username(container_config.user.as_deref());

    Ok(cri::Image {
        id: digest_string.clone(),
        repo_tags,
        repo_digests: repo_digests.into_iter().collect(),
        size: summary.size,
        uid: uid.map(|value| cri::Int64Value { value }),
        username,
        spec: Some(cri::ImageSpec {
            image: digest_string.clone(),
            annotations: manifest
                .annotations
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            image_ref: digest_string,
            ..Default::default()
        }),
        pinned: false,
    })
}

/// Groups every stored [`oci_store::ImageRecord`] by its own real
/// manifest digest — several tags (or the untagged sentinel) can all
/// point at the same digest, and one real CRI `Image` message
/// represents one real image, not one pointer.
fn group_by_digest(
    records: Vec<oci_store::ImageRecord>,
) -> std::collections::BTreeMap<String, Vec<oci_store::ImageRecord>> {
    let mut grouped: std::collections::BTreeMap<String, Vec<oci_store::ImageRecord>> =
        std::collections::BTreeMap::new();
    for record in records {
        grouped
            .entry(record.manifest_digest.to_string())
            .or_default()
            .push(record);
    }
    grouped
}

/// The real, blocking half of [`ImageServiceImpl::pull_image`] — a
/// real, unconditional pull, matching CRI's own `PullImage` semantics
/// exactly (real `cri-o`'s own doc comment: no pull-policy concept at
/// this layer at all, unlike `ociman run --pull`) via the exact same
/// already-shared `oci_registry::pull_unconditionally` `ociman pull`/
/// `ocibox create` already use. Always `tls_verify: true` (secure by
/// default) — this first slice doesn't yet expose a way to name an
/// insecure registry the way `ociman pull --tls-verify=false` does
/// (the real CRI `PullImageRequest` has no equivalent field at all;
/// that decision belongs to a real, cluster-level registry
/// configuration this project doesn't read yet, see this module's own
/// doc comment). Real kubelet-supplied `PullImageRequest.auth`
/// (per-pull inline credentials, distinct from the on-disk auth file
/// `ociman login` populates) is not honored yet either — a pull always
/// falls back to whatever `oci_registry::Credentials::load` already
/// finds on disk, exactly like every other pull in this project.
fn pull_image_blocking(spec: &str) -> Result<String, Status> {
    let reference = oci_spec_types::Reference::parse(spec)
        .map_err(|e| Status::invalid_argument(format!("parsing image reference {spec:?}: {e}")))?;
    let store = open_store()?;
    let record = oci_registry::pull_unconditionally(&store, &reference, true)
        .map_err(|e| Status::unavailable(format!("pulling {reference}: {e}")))?;
    Ok(record.manifest_digest.to_string())
}

/// Real, on-disk filesystem usage for one real directory — checked
/// directly against real `cri-o`'s own `getStorageFsInfo`/`utils.
/// GetDiskUsageStats` (`server/image_fs_info.go`, `utils/
/// filesystem.go`): a real directory-tree walk summing each entry's
/// own byte length (never `statfs(2)`), a real, current wall-clock
/// timestamp (never derived from any file's own mtime).
///
/// Unlike real cri-o's own walk (which counts every entry once,
/// including directories, with no hardlink awareness at all), this
/// reuses `oci_store::dir_stats` — the exact same real, hardlink-
/// deduplicated walk `ociman prune` already depends on for a correct
/// reclaimed-bytes figure (a real bug class, 0106/0111, this RPC has
/// no reason to reintroduce just to match cri-o's own cruder
/// arithmetic).
///
/// A directory that simply doesn't exist yet (a real, ordinary state
/// for `oci_store::cache_root` on a store that's never `run`/
/// `build-image`d anything) is a real, honest zero, not an error —
/// unlike real cri-o's own test suite, which does expect an error for
/// a missing directory: that difference is deliberate, since a
/// missing `cache_root` here is routine, expected state, not
/// misconfiguration, and a caller (a real kubelet) shouldn't see this
/// RPC fail just because nothing has been extracted yet. Any other
/// I/O error (permission denied, ...) still propagates as a real,
/// honest failure.
fn filesystem_usage(dir: &std::path::Path, now_nanos: i64) -> Result<cri::FilesystemUsage, Status> {
    let (bytes, files) = match oci_store::dir_stats(dir) {
        Ok(stats) => stats,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (0, 0),
        Err(e) => {
            return Err(Status::internal(format!(
                "computing usage for {}: {e}",
                dir.display()
            )));
        }
    };
    Ok(cri::FilesystemUsage {
        timestamp: now_nanos,
        fs_id: Some(cri::FilesystemIdentifier {
            mountpoint: dir.display().to_string(),
        }),
        used_bytes: Some(cri::UInt64Value { value: bytes }),
        inodes_used: Some(cri::UInt64Value { value: files }),
    })
}

/// The one real filtered-list computation behind both `ListImages`
/// and its `CRIListStreaming` sibling `StreamImages` — factored out
/// (a pure, behavior-preserving move, `docs/design/0234`) exactly
/// like real cri-o's own shared `listImages` helper serving both of
/// its RPCs. A filter naming one specific image resolves just that
/// one real image (0 or 1 results) — matching real `cri-o`'s own
/// identical "historically interpreted as a single image lookup"
/// behavior (`image_list.go`), never an error for "doesn't exist".
fn image_list_items(filter: Option<cri::ImageFilter>) -> Result<Vec<cri::Image>, Status> {
    let store = open_store()?;
    let filter_spec = filter
        .and_then(|f| f.image)
        .map(|s| s.image)
        .filter(|s| !s.is_empty());

    if let Some(spec) = filter_spec {
        let images = match oci_store::resolve_by_reference_or_id(&store, &spec)
            .map_err(|e| store_error("resolving image filter", e))?
        {
            Some(resolved) => {
                let digest = resolved.record().manifest_digest.to_string();
                let group = store
                    .list_images()
                    .map_err(|e| store_error("listing images", e))?
                    .into_iter()
                    .filter(|r| r.manifest_digest.to_string() == digest)
                    .collect::<Vec<_>>();
                vec![build_image(&store, &group)?]
            }
            None => Vec::new(),
        };
        return Ok(images);
    }

    let records = store
        .list_images()
        .map_err(|e| store_error("listing images", e))?;
    let mut images = Vec::new();
    for group in group_by_digest(records).into_values() {
        images.push(build_image(&store, &group)?);
    }
    Ok(images)
}

#[tonic::async_trait]
impl cri::image_service_server::ImageService for ImageServiceImpl {
    async fn list_images(
        &self,
        request: Request<cri::ListImagesRequest>,
    ) -> Result<Response<cri::ListImagesResponse>, Status> {
        let images = image_list_items(request.into_inner().filter)?;
        Ok(Response::new(cri::ListImagesResponse { images }))
    }

    type StreamImagesStream = tonic::codegen::BoxStream<cri::StreamImagesResponse>;

    /// The `CRIListStreaming` variant of `list_images`: the exact same
    /// filtered-list computation, streamed in chunks of real cri-o's
    /// own `streamChunkSize` (see `docs/design/0234` and `stream.rs`'s
    /// own module doc comment — an empty result streams zero messages
    /// and closes immediately, matching real cri-o's own
    /// `StreamImages`, `image_list.go`, exactly).
    async fn stream_images(
        &self,
        request: Request<cri::StreamImagesRequest>,
    ) -> Result<Response<Self::StreamImagesStream>, Status> {
        let items = image_list_items(request.into_inner().filter)?;
        Ok(Response::new(crate::stream::chunked(items, |images| {
            cri::StreamImagesResponse { images }
        })))
    }

    async fn image_status(
        &self,
        request: Request<cri::ImageStatusRequest>,
    ) -> Result<Response<cri::ImageStatusResponse>, Status> {
        let request = request.into_inner();
        let verbose = request.verbose;
        let spec = request.image.map(|s| s.image).filter(|s| !s.is_empty());
        let Some(spec) = spec else {
            return Err(Status::invalid_argument("no image specified"));
        };

        let store = open_store()?;
        // An unresolvable image is a real, empty response (`image:
        // None`), never an error -- matching real `cri-o`'s own
        // identical behavior exactly (`ImageStatus`, `image_status.go`).
        let Some(resolved) = oci_store::resolve_by_reference_or_id(&store, &spec)
            .map_err(|e| store_error("resolving image", e))?
        else {
            return Ok(Response::new(cri::ImageStatusResponse::default()));
        };

        let digest = resolved.record().manifest_digest.to_string();
        let group = store
            .list_images()
            .map_err(|e| store_error("listing images", e))?
            .into_iter()
            .filter(|r| r.manifest_digest.to_string() == digest)
            .collect::<Vec<_>>();
        let image = build_image(&store, &group)?;

        // Verbose info: a single `"info"` key holding a JSON blob with
        // the image's own labels and raw OCI config -- matching real
        // `cri-o`'s own identical shape exactly
        // (`createImageInfo`/`image_status.go`). Only populated when
        // the request actually asked for it (`verbose`), matching the
        // real proto contract ("It should only be returned non-empty
        // when Verbose is true").
        let mut info = std::collections::HashMap::new();
        if verbose {
            let config = store
                .image_config(resolved.record())
                .map_err(|e| store_error("reading image config", e))?;
            let payload = serde_json::json!({
                "labels": config.config.as_ref().map(|c| c.labels.clone()).unwrap_or_default(),
                "imageSpec": config,
            });
            info.insert(
                "info".to_string(),
                serde_json::to_string(&payload).unwrap_or_default(),
            );
        }

        Ok(Response::new(cri::ImageStatusResponse {
            image: Some(image),
            info,
        }))
    }

    async fn pull_image(
        &self,
        request: Request<cri::PullImageRequest>,
    ) -> Result<Response<cri::PullImageResponse>, Status> {
        let spec = request
            .into_inner()
            .image
            .map(|s| s.image)
            .filter(|s| !s.is_empty());
        let Some(spec) = spec else {
            return Err(Status::invalid_argument("no image specified"));
        };

        // A real, unconditional pull is always a real network round
        // trip -- run on a blocking-pool thread so one slow/stuck pull
        // never starves this server's own tokio worker threads from
        // answering every other RPC in the meantime (this project's
        // own registry client, `oci_registry`/`ureq`, is a plain,
        // synchronous blocking client throughout, shared unchanged
        // with every other binary — see this module's own doc comment
        // for why introducing an async HTTP stack just for this one
        // RPC isn't worth it).
        tokio::task::spawn_blocking(move || pull_image_blocking(&spec))
            .await
            .map_err(|e| Status::internal(format!("pull task panicked: {e}")))?
            .map(|image_ref| Response::new(cri::PullImageResponse { image_ref }))
    }

    async fn remove_image(
        &self,
        request: Request<cri::RemoveImageRequest>,
    ) -> Result<Response<cri::RemoveImageResponse>, Status> {
        let spec = request
            .into_inner()
            .image
            .map(|s| s.image)
            .filter(|s| !s.is_empty());
        let Some(spec) = spec else {
            return Err(Status::invalid_argument("no image specified"));
        };

        let store = open_store()?;
        let resolved = match oci_store::resolve_by_reference_or_id(&store, &spec) {
            Ok(resolved) => resolved,
            // A short ID prefix genuinely matching more than one
            // *different* image is a real client-input problem (which
            // one did the caller actually mean?) -- distinct from
            // "nothing resolved at all", which is the real, silent,
            // idempotent-success case below.
            Err(oci_store::StoreError::AmbiguousId { spec, count }) => {
                return Err(Status::invalid_argument(format!(
                    "image ID {spec:?} is ambiguous: matches {count} different images"
                )));
            }
            Err(e) => return Err(store_error("resolving image", e)),
        };

        // Nothing resolved at all: a real, silent, idempotent success
        // -- matching the real proto's own documented contract
        // exactly ("must not return an error if the image has already
        // been removed").
        let Some(resolved) = resolved else {
            return Ok(Response::new(cri::RemoveImageResponse::default()));
        };

        // Every real reference sharing this same manifest digest is
        // removed, not just the one `spec` happened to name -- see
        // this module's own doc comment for the real proto citation
        // this matches (removing by any one tag/ID removes every tag,
        // a genuinely different rule than `ociman rmi`'s own).
        let digest = resolved.record().manifest_digest.to_string();
        for record in store
            .list_images()
            .map_err(|e| store_error("listing images", e))?
        {
            if record.manifest_digest.to_string() == digest {
                store
                    .remove_image(&record.reference)
                    .map_err(|e| store_error("removing image", e))?;
            }
        }

        Ok(Response::new(cri::RemoveImageResponse::default()))
    }

    async fn image_fs_info(
        &self,
        _request: Request<cri::ImageFsInfoRequest>,
    ) -> Result<Response<cri::ImageFsInfoResponse>, Status> {
        let store = open_store()?;
        let now_nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        // `image_filesystems`: this project's own real, content-
        // addressed blob store -- every image's actual on-disk bytes.
        // `container_filesystems`: this project's own real, extracted
        // rootfs cache -- the closest real analogue to cri-o's own
        // separate "container filesystem" figure, since running
        // containers are backed by that cache, not the blob store
        // directly (see `filesystem_usage`'s own doc comment for the
        // full reasoning).
        let image_filesystems = vec![filesystem_usage(&store.blobs_dir(), now_nanos)?];
        let container_filesystems =
            vec![filesystem_usage(&oci_store::cache_root(&store), now_nanos)?];
        Ok(Response::new(cri::ImageFsInfoResponse {
            image_filesystems,
            container_filesystems,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cri::image_service_server::ImageService as _;

    #[test]
    fn uid_and_username_handles_every_real_shape() {
        assert_eq!(uid_and_username(None), (None, String::new()));
        assert_eq!(uid_and_username(Some("")), (None, String::new()));
        assert_eq!(uid_and_username(Some("1000")), (Some(1000), String::new()));
        assert_eq!(
            uid_and_username(Some("1000:1000")),
            (Some(1000), String::new())
        );
        assert_eq!(uid_and_username(Some("nginx")), (None, "nginx".to_string()));
        assert_eq!(
            uid_and_username(Some("nginx:nginx")),
            (None, "nginx".to_string())
        );
    }

    // `image_status`'s own "empty store" / "resolves a real image" /
    // "verbose info" cases are all covered by the real, socket-
    // connecting integration tests in `tests/tests/ocicri_image_
    // service.rs` instead of here: `open_store()` reads the real
    // process-global `OCI_TOOLS_STORAGE_ROOT` environment variable
    // directly (no dependency injection), which every other in-
    // process unit test in this crate avoids mutating precisely to
    // stay safe under `cargo test`'s own default parallel-within-one-
    // binary execution -- a real, separate subprocess (what the
    // integration tests actually spawn) has no such cross-test
    // interference risk at all.

    #[tokio::test]
    async fn image_status_with_no_image_at_all_is_invalid_argument() {
        let service = ImageServiceImpl;
        let status = service
            .image_status(Request::new(cri::ImageStatusRequest {
                image: None,
                verbose: false,
            }))
            .await
            .unwrap_err();
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
    }

    // `pull_image`'s own real network-attempt/success cases are
    // covered by the real, socket-connecting integration tests in
    // `tests/tests/ocicri_pull_image.rs` instead of here, for the same
    // "avoid touching the real process-global storage-root env var in
    // an in-process, potentially-parallel unit test" reason given
    // above -- the "no image specified" argument check alone needs
    // neither a store nor a real network attempt, so it's safe here.
    #[tokio::test]
    async fn pull_image_with_no_image_at_all_is_invalid_argument() {
        let service = ImageServiceImpl;
        let status = service
            .pull_image(Request::new(cri::PullImageRequest {
                image: None,
                auth: None,
                sandbox_config: None,
            }))
            .await
            .unwrap_err();
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
    }

    // `remove_image`'s own real removal/idempotency/sibling-tag cases
    // are covered by the real, socket-connecting integration tests in
    // `tests/tests/ocicri_image_service.rs` instead of here, for the
    // same reason given above -- the "no image specified" argument
    // check alone needs no store access at all, so it's safe here.
    #[tokio::test]
    async fn remove_image_with_no_image_at_all_is_invalid_argument() {
        let service = ImageServiceImpl;
        let status = service
            .remove_image(Request::new(cri::RemoveImageRequest { image: None }))
            .await
            .unwrap_err();
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
    }
}
