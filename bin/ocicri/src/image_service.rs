//! The real `ImageService` gRPC implementation — this increment's own
//! narrow first slice: `ListImages`/`ImageStatus`, the two read-only
//! RPCs, reusing this project's own already-tested `oci_store`
//! resolution/summary primitives directly (the same ones `ociman
//! images`/`ociman inspect` already use) rather than anything new.
//! `PullImage`/`RemoveImage`/`ImageFsInfo`/`StreamImages` each return a
//! real, honest `Status::unimplemented` naming itself — matching this
//! project's own established "narrow first slice, document the rest"
//! pattern (see `runtime_service.rs`'s own module doc comment for the
//! identical reasoning applied to `RuntimeService`).
//!
//! Behavior checked directly against real `cri-o`'s own
//! implementation (`~/git/cri-o/server/image_list.go`/`image_status.go`):
//! a filter naming one specific image resolves just that one (0 or 1
//! results, never an error for "not found"); `ImageStatus` of an
//! unresolvable image returns an empty response (`image: None`), not
//! an error either — only a request naming no image at all is a real
//! error.

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

/// A real, honest "not implemented yet" error for every RPC this first
/// slice doesn't answer — see [`crate::runtime_service::unimplemented`]'s
/// own identical doc comment.
fn unimplemented<T>(name: &str) -> Result<Response<T>, Status> {
    Err(Status::unimplemented(format!(
        "ocicri: {name} is not implemented yet (milestone 7, a real, narrow first slice: only \
         ListImages/ImageStatus are answered so far)"
    )))
}

#[tonic::async_trait]
impl cri::image_service_server::ImageService for ImageServiceImpl {
    async fn list_images(
        &self,
        request: Request<cri::ListImagesRequest>,
    ) -> Result<Response<cri::ListImagesResponse>, Status> {
        let store = open_store()?;
        let filter_spec = request
            .into_inner()
            .filter
            .and_then(|f| f.image)
            .map(|s| s.image)
            .filter(|s| !s.is_empty());

        // A filter naming one specific image resolves just that one
        // real image (0 or 1 results) -- matching real `cri-o`'s own
        // identical "historically interpreted as a single image
        // lookup" behavior (`listImages`, `image_list.go`), never an
        // error for "doesn't exist".
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
            return Ok(Response::new(cri::ListImagesResponse { images }));
        }

        let records = store
            .list_images()
            .map_err(|e| store_error("listing images", e))?;
        let mut images = Vec::new();
        for group in group_by_digest(records).into_values() {
            images.push(build_image(&store, &group)?);
        }
        Ok(Response::new(cri::ListImagesResponse { images }))
    }

    type StreamImagesStream = tonic::codegen::BoxStream<cri::StreamImagesResponse>;

    async fn stream_images(
        &self,
        _request: Request<cri::StreamImagesRequest>,
    ) -> Result<Response<Self::StreamImagesStream>, Status> {
        unimplemented("StreamImages")
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
        _request: Request<cri::PullImageRequest>,
    ) -> Result<Response<cri::PullImageResponse>, Status> {
        unimplemented("PullImage")
    }

    async fn remove_image(
        &self,
        _request: Request<cri::RemoveImageRequest>,
    ) -> Result<Response<cri::RemoveImageResponse>, Status> {
        unimplemented("RemoveImage")
    }

    async fn image_fs_info(
        &self,
        _request: Request<cri::ImageFsInfoRequest>,
    ) -> Result<Response<cri::ImageFsInfoResponse>, Status> {
        unimplemented("ImageFsInfo")
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
}
