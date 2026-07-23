//! The one shared piece of the CRI's `CRIListStreaming` RPCs
//! (`StreamPodSandboxes`/`StreamImages` — see `docs/design/0234`):
//! turning an already-computed, already-filtered list into a real
//! server stream of chunked responses, matching real cri-o's own
//! implementation shape exactly (`server/sandbox_list.go`/
//! `image_list.go`: the same list computation the plain list RPC
//! uses, sent in chunks of `streamChunkSize`, EOF after the last —
//! zero messages for zero items, since the chunking loop simply
//! never iterates).

use tonic::codegen::BoxStream;

/// Real cri-o's own `streamChunkSize` (`server/server.go`), verbatim.
pub const STREAM_CHUNK_SIZE: usize = 3000;

/// Splits `items` into chunks of at most [`STREAM_CHUNK_SIZE`], wraps
/// each in a response message via `make_response`, and returns the
/// whole thing as the ready-to-serve `BoxStream` `tonic`'s generated
/// server traits expect. The proto's own contract holds by
/// construction: every item lands in exactly one response, nothing is
/// duplicated, and the stream ends (EOF) after the last chunk — with
/// zero messages at all for an empty list, matching real cri-o.
pub fn chunked<T, R, F>(items: Vec<T>, make_response: F) -> BoxStream<R>
where
    T: Send + 'static,
    R: Send + 'static,
    F: Fn(Vec<T>) -> R,
{
    let mut responses = Vec::new();
    let mut remaining = items;
    while !remaining.is_empty() {
        let rest = remaining.split_off(remaining.len().min(STREAM_CHUNK_SIZE));
        responses.push(Ok(make_response(remaining)));
        remaining = rest;
    }
    Box::pin(tokio_stream::iter(responses))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_stream::StreamExt as _;

    /// Collects the chunk *sizes* a given item count produces — the
    /// boundary arithmetic is the only interesting behavior here, and
    /// the one part a socket-level integration test can't practically
    /// exercise without fabricating 3001 real sandboxes.
    async fn chunk_sizes(count: usize) -> Vec<usize> {
        let items: Vec<usize> = (0..count).collect();
        let mut stream = chunked(items, |chunk| chunk.len());
        let mut sizes = Vec::new();
        while let Some(response) = stream.next().await {
            sizes.push(response.expect("chunks are always Ok by construction"));
        }
        sizes
    }

    #[tokio::test]
    async fn zero_items_stream_zero_messages() {
        assert_eq!(chunk_sizes(0).await, Vec::<usize>::new());
    }

    #[tokio::test]
    async fn one_item_is_one_chunk() {
        assert_eq!(chunk_sizes(1).await, vec![1]);
    }

    #[tokio::test]
    async fn exactly_one_chunk_size_is_still_one_chunk() {
        assert_eq!(
            chunk_sizes(STREAM_CHUNK_SIZE).await,
            vec![STREAM_CHUNK_SIZE]
        );
    }

    #[tokio::test]
    async fn one_over_the_chunk_size_splits_into_two_chunks() {
        assert_eq!(
            chunk_sizes(STREAM_CHUNK_SIZE + 1).await,
            vec![STREAM_CHUNK_SIZE, 1]
        );
    }

    /// The proto contract: every item in exactly one response, no
    /// duplicates, order preserved across the chunk boundary.
    #[tokio::test]
    async fn every_item_lands_exactly_once_in_order() {
        let items: Vec<usize> = (0..STREAM_CHUNK_SIZE + 5).collect();
        let mut stream = chunked(items, |chunk| chunk);
        let mut collected = Vec::new();
        while let Some(response) = stream.next().await {
            collected.extend(response.unwrap());
        }
        assert_eq!(collected, (0..STREAM_CHUNK_SIZE + 5).collect::<Vec<_>>());
    }
}
