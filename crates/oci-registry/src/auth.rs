//! Docker/OCI distribution-spec token authentication (the `WWW-Authenticate:
//! Bearer ...` challenge/response flow used by Docker Hub, quay.io, GHCR,
//! and effectively every registry that isn't fully anonymous).
//!
//! Flow: an unauthenticated request gets `401` with a `WWW-Authenticate:
//! Bearer realm="...",service="...",scope="..."` header; the client GETs
//! `realm?service=...&scope=...` (optionally with HTTP Basic credentials)
//! and receives `{"token": "..."}` (or `{"access_token": "..."}`, an
//! accepted synonym); that token is sent back as `Authorization: Bearer
//! <token>` on the retried request.

use crate::RegistryError;

/// A parsed `WWW-Authenticate: Bearer ...` challenge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BearerChallenge {
    /// The token endpoint to GET.
    pub realm: String,
    /// The `service` parameter to send back to the token endpoint.
    pub service: Option<String>,
    /// The `scope` parameter the server is asking for (e.g.
    /// `repository:library/ubuntu:pull`); overrides whatever scope the
    /// client originally guessed.
    pub scope: Option<String>,
}

/// Parse a `WWW-Authenticate` header value, returning `None` if it is not a
/// `Bearer` challenge (e.g. `Basic realm="..."`, which oci-tools does not
/// speak directly — only via a pre-configured credential's Basic header).
pub fn parse_bearer_challenge(header: &str) -> Option<BearerChallenge> {
    let rest = header.strip_prefix("Bearer ")?;
    let mut realm = None;
    let mut service = None;
    let mut scope = None;
    for part in rest.split(',') {
        let Some((key, value)) = part.trim().split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"');
        match key.trim() {
            "realm" => realm = Some(value.to_string()),
            "service" => service = Some(value.to_string()),
            "scope" => scope = Some(value.to_string()),
            _ => {}
        }
    }
    Some(BearerChallenge {
        realm: realm?,
        service,
        scope,
    })
}

/// Fetch a bearer token from `challenge.realm`, requesting `scope`
/// (overriding the client's guessed scope with whatever the server's
/// challenge itself asked for). `basic_auth` is a full `Authorization:
/// Basic ...` header value, when credentials are configured for this
/// registry.
pub fn fetch_token(
    agent: &ureq::Agent,
    challenge: &BearerChallenge,
    scope: &str,
    basic_auth: Option<&str>,
) -> Result<String, RegistryError> {
    let mut url = challenge.realm.clone();
    url.push('?');
    if let Some(service) = &challenge.service {
        url.push_str("service=");
        url.push_str(&percent_encode(service));
        url.push('&');
    }
    url.push_str("scope=");
    url.push_str(&percent_encode(scope));

    let mut req = agent.get(&url);
    if let Some(basic_auth) = basic_auth {
        req = req.header("Authorization", basic_auth);
    }
    let mut resp = req
        .call()
        .map_err(|e| RegistryError::Transport(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(RegistryError::Auth(format!(
            "token request to {} failed: HTTP {}",
            challenge.realm,
            resp.status()
        )));
    }
    let body = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| RegistryError::Transport(e.to_string()))?;
    let value: serde_json::Value = serde_json::from_str(&body)?;
    value
        .get("token")
        .or_else(|| value.get("access_token"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| {
            RegistryError::Auth(format!(
                "token response from {} has no token/access_token field",
                challenge.realm
            ))
        })
}

/// Minimal percent-encoding sufficient for registry `service`/`scope`
/// query parameters (repository paths, tags, and the fixed action/scope
/// vocabulary): passes common URL-safe characters through and
/// percent-encodes everything else.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'~'
            | b'/'
            | b':'
            | b','
            | b'*' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_docker_hub_challenge() {
        let header = r#"Bearer realm="https://auth.docker.io/token",service="registry.docker.io",scope="repository:library/ubuntu:pull""#;
        let challenge = parse_bearer_challenge(header).unwrap();
        assert_eq!(challenge.realm, "https://auth.docker.io/token");
        assert_eq!(challenge.service.as_deref(), Some("registry.docker.io"));
        assert_eq!(
            challenge.scope.as_deref(),
            Some("repository:library/ubuntu:pull")
        );
    }

    #[test]
    fn parses_challenge_without_scope() {
        let header = r#"Bearer realm="https://ghcr.io/token",service="ghcr.io""#;
        let challenge = parse_bearer_challenge(header).unwrap();
        assert_eq!(challenge.scope, None);
    }

    #[test]
    fn rejects_non_bearer_challenges() {
        assert_eq!(parse_bearer_challenge(r#"Basic realm="example""#), None);
    }

    #[test]
    fn percent_encode_passes_common_chars_and_escapes_the_rest() {
        assert_eq!(
            percent_encode("repository:foo/bar:pull"),
            "repository:foo/bar:pull"
        );
        assert_eq!(percent_encode("a b"), "a%20b");
    }
}
