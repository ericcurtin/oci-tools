//! Downloading a remote URL source for `ADD` — matches real BuildKit's
//! own `downloadSource`/`getFilenameForDownload`
//! (`~/git/moby/daemon/builder/dockerfile/copy.go`), checked directly:
//!
//! - The response body is fetched in full and never auto-extracted,
//!   even if it happens to look like a tar archive — checked directly
//!   against the real source's own `noDecompress = true // data from
//!   http shouldn't be extracted even on ADD`, a deliberate difference
//!   from a *local* archive `ADD` source (`oci_layer::detect_archive`,
//!   0068), which *is* auto-extracted.
//! - A suggested destination file name is derived, in the same
//!   priority order as the real source: first the URL's own path
//!   (its final segment, unless the path is empty or ends in `/`),
//!   then the response's own `Content-Disposition` header's
//!   `filename=` parameter, `None` if neither gives a usable name (in
//!   which case the caller's own `dest` must already be a real,
//!   explicit, non-`/`-ending file name — matching real BuildKit's own
//!   `"cannot determine filename for source"` error for a destination
//!   that ends in `/` with no derivable name).
//!
//! One deliberate simplification, not present in the real source:
//! `Content-Disposition` parsing here only ever recognizes the
//! ordinary, unquoted-or-quoted `filename=...` parameter — not the
//! full RFC 6266/2183 grammar real Go's `mime.ParseMediaType` handles
//! (escaped quotes, the extended `filename*=UTF-8''...` form). This is
//! only ever a *fallback* path, reached solely when the URL's own path
//! gives no usable name at all — the overwhelming majority of real
//! `ADD <url>` Containerfile lines use a URL whose own path already
//! ends in a real file name, never reaching this fallback in the first
//! place.

/// Downloaded response bytes are refused past this size — no real
/// `ADD <url>` use case (a config file, a small script, a small
/// archive) approaches this; this project's own defensive bound
/// against a misbehaving or hostile server, not a limit real Docker
/// itself imposes (it doesn't document one at all).
const MAX_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;

/// A real, fetched `ADD <url>` source.
#[derive(Debug, Clone)]
pub struct Downloaded {
    /// The response body, verbatim — never decompressed or otherwise
    /// interpreted, matching real BuildKit's own `noDecompress` for
    /// exactly this source kind.
    pub bytes: Vec<u8>,
    /// A file name suggested by the URL's own path, or (failing that)
    /// the response's own `Content-Disposition` header — `None` if
    /// neither gave a usable one.
    pub suggested_file_name: Option<String>,
}

/// A real, distinguishable failure fetching `url`.
#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    /// The request itself failed (DNS, connect, TLS, timeout, ...).
    #[error("fetching {url}: {message}")]
    Request {
        /// The URL that was being fetched.
        url: String,
        /// The underlying transport error, as text.
        message: String,
    },
    /// The server responded, but with a real HTTP error status.
    #[error("fetching {url}: HTTP {status}")]
    Status {
        /// The URL that was being fetched.
        url: String,
        /// The HTTP status code the server responded with.
        status: u16,
    },
    /// The response body couldn't be read in full (a real I/O error,
    /// or it exceeded [`MAX_DOWNLOAD_BYTES`]).
    #[error("reading response body from {url}: {message}")]
    Body {
        /// The URL that was being fetched.
        url: String,
        /// The underlying I/O error, as text.
        message: String,
    },
}

/// Fetch `url` for a real `ADD <url>` source.
pub fn download(url: &str) -> Result<Downloaded, DownloadError> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(std::time::Duration::from_secs(30)))
        .build()
        .into();
    let mut response = agent.get(url).call().map_err(|e| DownloadError::Request {
        url: url.to_string(),
        message: e.to_string(),
    })?;

    let status = response.status().as_u16();
    if status >= 400 {
        return Err(DownloadError::Status {
            url: url.to_string(),
            status,
        });
    }

    let content_disposition = response
        .headers()
        .get("content-disposition")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    let bytes = response
        .body_mut()
        .with_config()
        .limit(MAX_DOWNLOAD_BYTES)
        .read_to_vec()
        .map_err(|e| DownloadError::Body {
            url: url.to_string(),
            message: e.to_string(),
        })?;

    let suggested_file_name = filename_from_url_path(url).or_else(|| {
        content_disposition
            .as_deref()
            .and_then(filename_from_content_disposition)
    });

    Ok(Downloaded {
        bytes,
        suggested_file_name,
    })
}

/// The final path segment of `url`'s own path component, if it's
/// non-empty and doesn't end in `/` — matching real BuildKit's own
/// first `getFilenameForDownload` check
/// (`filepath.Base(filepath.FromSlash(path))` on the URL's own path).
fn filename_from_url_path(url: &str) -> Option<String> {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let path_start = after_scheme.find('/')?;
    let path = &after_scheme[path_start..];
    let path = path.split(['?', '#']).next().unwrap_or(path);
    if path.is_empty() || path.ends_with('/') {
        return None;
    }
    let name = path.rsplit('/').next()?;
    (!name.is_empty()).then(|| name.to_string())
}

/// The `filename=` parameter of a `Content-Disposition` header value,
/// if present and usable — see this module's own top doc comment for
/// the deliberate scope limit (only the plain `filename=` parameter,
/// not the full RFC grammar).
fn filename_from_content_disposition(value: &str) -> Option<String> {
    for part in value.split(';') {
        let part = part.trim();
        if let Some(raw) = part.strip_prefix("filename=") {
            let name = raw.trim_matches('"');
            if !name.is_empty() && !name.ends_with('/') {
                return Some(name.rsplit('/').next().unwrap_or(name).to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_from_url_path_takes_the_final_segment() {
        assert_eq!(
            filename_from_url_path("https://example.com/a/b/file.txt"),
            Some("file.txt".to_string())
        );
        assert_eq!(
            filename_from_url_path("https://example.com/file.txt?x=1#frag"),
            Some("file.txt".to_string())
        );
        assert_eq!(filename_from_url_path("https://example.com/"), None);
        assert_eq!(filename_from_url_path("https://example.com"), None);
        assert_eq!(filename_from_url_path("https://example.com/a/"), None);
    }

    #[test]
    fn filename_from_content_disposition_recognizes_quoted_and_unquoted_forms() {
        assert_eq!(
            filename_from_content_disposition("attachment; filename=\"report.pdf\""),
            Some("report.pdf".to_string())
        );
        assert_eq!(
            filename_from_content_disposition("attachment; filename=report.pdf"),
            Some("report.pdf".to_string())
        );
        assert_eq!(filename_from_content_disposition("attachment"), None);
        assert_eq!(
            filename_from_content_disposition("attachment; filename=\"\""),
            None
        );
    }

    // A tiny, single-response HTTP/1.1 mock, the same established
    // pattern `oci-registry`'s own `crates/oci-registry/src/client.rs`
    // test module uses for a real server rather than a mocked
    // transport -- real, byte-level HTTP over a real loopback socket,
    // not a fake `ureq` layer.
    fn serve_one_response(response: &'static str) -> std::net::SocketAddr {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            use std::io::{BufRead as _, BufReader, Write as _};
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            loop {
                let mut header_line = String::new();
                reader.read_line(&mut header_line).unwrap();
                if header_line.trim().is_empty() {
                    break;
                }
            }
            stream.write_all(response.as_bytes()).unwrap();
        });
        addr
    }

    #[test]
    fn download_fetches_the_real_body_and_derives_a_filename_from_the_path() {
        let addr = serve_one_response(
            "HTTP/1.1 200 OK\r\nContent-Length: 12\r\nConnection: close\r\n\r\nhello world!",
        );
        let downloaded = download(&format!("http://{addr}/dir/report.txt")).unwrap();
        assert_eq!(downloaded.bytes, b"hello world!");
        assert_eq!(
            downloaded.suggested_file_name.as_deref(),
            Some("report.txt")
        );
    }

    #[test]
    fn download_falls_back_to_content_disposition_when_the_path_has_no_name() {
        let addr = serve_one_response(
            "HTTP/1.1 200 OK\r\nContent-Disposition: attachment; filename=\"data.bin\"\r\n\
             Content-Length: 3\r\nConnection: close\r\n\r\nabc",
        );
        let downloaded = download(&format!("http://{addr}/")).unwrap();
        assert_eq!(downloaded.bytes, b"abc");
        assert_eq!(downloaded.suggested_file_name.as_deref(), Some("data.bin"));
    }

    #[test]
    fn download_surfaces_a_real_http_error_status() {
        let addr = serve_one_response(
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        let err = download(&format!("http://{addr}/missing")).unwrap_err();
        assert!(
            matches!(err, DownloadError::Status { status: 404, .. }),
            "{err}"
        );
    }
}
