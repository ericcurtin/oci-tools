//! Content digests (`<algorithm>:<hex>`), per the OCI image-spec's
//! [descriptor digest][spec] grammar.
//!
//! [spec]: https://github.com/opencontainers/image-spec/blob/main/descriptor.md#digests

use std::fmt;
use std::io::Write;
use std::str::FromStr;

use sha2::{Digest as _, Sha256};

/// A digest algorithm. Only `sha256` is required by the OCI distribution
/// spec for registry interoperability; `sha512` is accepted on parse (some
/// registries and tools emit it) but oci-tools never *produces* it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Algorithm {
    /// SHA-256, 32 bytes / 64 hex characters. The only algorithm oci-tools
    /// writes.
    Sha256,
    /// SHA-512, 64 bytes / 128 hex characters. Parse-only.
    Sha512,
}

impl Algorithm {
    /// The lowercase name used in the wire format (`sha256:...`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Algorithm::Sha256 => "sha256",
            Algorithm::Sha512 => "sha512",
        }
    }

    /// Expected hex-digest length for this algorithm.
    const fn hex_len(self) -> usize {
        match self {
            Algorithm::Sha256 => 64,
            Algorithm::Sha512 => 128,
        }
    }
}

impl fmt::Display for Algorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A parsed and validated content digest, e.g.
/// `sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855`.
///
/// The hex part is stored lowercase (as required by the spec) and validated
/// to be the exact length the algorithm expects.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Digest {
    algorithm: Algorithm,
    hex: String,
}

/// Error returned by [`Digest::parse`] / [`str::parse`].
#[derive(Debug, thiserror::Error)]
pub enum DigestParseError {
    /// Missing the `<algorithm>:` prefix.
    #[error("digest {0:?} has no algorithm prefix (expected \"sha256:...\")")]
    NoAlgorithm(String),
    /// Algorithm is not one oci-tools understands.
    #[error("unsupported digest algorithm {0:?}")]
    UnsupportedAlgorithm(String),
    /// Hex part has the wrong length or contains non-hex characters.
    #[error(
        "digest {digest:?}: invalid hex encoding for {algorithm} (expected {expected} lowercase hex characters)"
    )]
    InvalidHex {
        /// The full offending digest string.
        digest: String,
        /// The algorithm whose hex length requirement was violated.
        algorithm: Algorithm,
        /// The expected hex character count.
        expected: usize,
    },
}

impl Digest {
    /// Parse and validate a digest string.
    pub fn parse(s: &str) -> Result<Self, DigestParseError> {
        let Some((alg, hex)) = s.split_once(':') else {
            return Err(DigestParseError::NoAlgorithm(s.to_string()));
        };
        let algorithm = match alg {
            "sha256" => Algorithm::Sha256,
            "sha512" => Algorithm::Sha512,
            other => return Err(DigestParseError::UnsupportedAlgorithm(other.to_string())),
        };
        let valid_hex = hex.len() == algorithm.hex_len()
            && hex.bytes().all(|b| b.is_ascii_hexdigit())
            && hex.bytes().all(|b| !b.is_ascii_uppercase());
        if !valid_hex {
            return Err(DigestParseError::InvalidHex {
                digest: s.to_string(),
                algorithm,
                expected: algorithm.hex_len(),
            });
        }
        Ok(Digest {
            algorithm,
            hex: hex.to_string(),
        })
    }

    /// The digest algorithm.
    pub fn algorithm(&self) -> Algorithm {
        self.algorithm
    }

    /// The lowercase hex-encoded hash, without the algorithm prefix.
    pub fn hex(&self) -> &str {
        &self.hex
    }

    /// sha256-of-empty-input, used as a well-known placeholder (e.g. the
    /// `EmptyLayer` history entries carry no blob).
    pub fn empty_sha256() -> Self {
        Sha256Writer::new().finish_digest()
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.algorithm, self.hex)
    }
}

impl FromStr for Digest {
    type Err = DigestParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Digest::parse(s)
    }
}

impl serde::Serialize for Digest {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> serde::Deserialize<'de> for Digest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Digest::parse(&s).map_err(serde::de::Error::custom)
    }
}

/// Streaming SHA-256 hasher that also implements [`std::io::Write`], so it
/// can sit in a `tee`-style pipeline while content is copied to disk.
pub struct Sha256Writer {
    hasher: Sha256,
}

impl Default for Sha256Writer {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256Writer {
    /// Start a new incremental hash.
    pub fn new() -> Self {
        Sha256Writer {
            hasher: Sha256::new(),
        }
    }

    /// Consume the hasher and produce the final [`Digest`].
    pub fn finish_digest(self) -> Digest {
        let bytes = self.hasher.finalize();
        Digest {
            algorithm: Algorithm::Sha256,
            hex: hex_encode(&bytes),
        }
    }
}

impl Write for Sha256Writer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.hasher.update(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Hash `bytes` and return the `sha256:...` digest directly (for small
/// in-memory buffers such as manifests and configs; streamed content should
/// use [`Sha256Writer`] instead).
pub fn sha256(bytes: &[u8]) -> Digest {
    let mut hasher = Sha256Writer::new();
    // Writing to an in-memory hasher cannot fail.
    let _ = hasher.write_all(bytes);
    hasher.finish_digest()
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_sha256() {
        let d = Digest::parse(
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        )
        .unwrap();
        assert_eq!(d.algorithm(), Algorithm::Sha256);
        assert_eq!(d.to_string().len(), "sha256:".len() + 64);
    }

    #[test]
    fn rejects_missing_algorithm() {
        assert!(matches!(
            Digest::parse("deadbeef"),
            Err(DigestParseError::NoAlgorithm(_))
        ));
    }

    #[test]
    fn rejects_unsupported_algorithm() {
        assert!(matches!(
            Digest::parse("md5:d41d8cd98f00b204e9800998ecf8427e"),
            Err(DigestParseError::UnsupportedAlgorithm(_))
        ));
    }

    #[test]
    fn rejects_wrong_length_hex() {
        assert!(matches!(
            Digest::parse("sha256:abcd"),
            Err(DigestParseError::InvalidHex { .. })
        ));
    }

    #[test]
    fn rejects_uppercase_hex() {
        let s = format!("sha256:{}", "A".repeat(64));
        assert!(matches!(
            Digest::parse(&s),
            Err(DigestParseError::InvalidHex { .. })
        ));
    }

    #[test]
    fn sha256_of_empty_matches_known_value() {
        assert_eq!(
            sha256(b"").to_string(),
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(Digest::empty_sha256(), sha256(b""));
    }

    #[test]
    fn sha256_of_known_string() {
        // echo -n "hello" | sha256sum
        assert_eq!(
            sha256(b"hello").to_string(),
            "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn streaming_matches_one_shot() {
        let mut w = Sha256Writer::new();
        w.write_all(b"hel").unwrap();
        w.write_all(b"lo").unwrap();
        assert_eq!(w.finish_digest(), sha256(b"hello"));
    }

    #[test]
    fn round_trips_through_json() {
        let d = sha256(b"hello");
        let json = serde_json::to_string(&d).unwrap();
        assert_eq!(json, format!("\"{d}\""));
        let back: Digest = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d);
    }

    #[test]
    fn rejects_bad_json_digest() {
        let err = serde_json::from_str::<Digest>("\"not-a-digest\"");
        assert!(err.is_err());
    }
}
