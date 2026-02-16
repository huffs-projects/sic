//! Source: type, url, hash (algorithm:hex).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Source descriptor: type (e.g. tarball), url, hash.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Source {
    /// Source type, e.g. "tarball".
    #[serde(rename = "type")]
    pub type_name: String,
    /// URL to fetch from.
    pub url: String,
    /// Hash for verification (e.g. sha256:hex).
    #[serde(with = "source_hash_serde")]
    pub hash: SourceHash,
}

mod source_hash_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::SourceHash;

    pub fn serialize<S>(h: &SourceHash, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        h.to_string().serialize(s)
    }

    pub fn deserialize<'de, D>(d: D) -> Result<SourceHash, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(d)?;
        SourceHash::parse(&raw).map_err(serde::de::Error::custom)
    }
}

/// Parsed hash: algorithm and hex digest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceHash {
    /// Algorithm name, e.g. "sha256".
    pub algorithm: String,
    /// Hex-encoded digest (lowercase).
    pub hex: String,
}

/// Allowed hash algorithms.
const ALLOWED_ALGORITHMS: &[&str] = &["sha256", "sha512"];

impl SourceHash {
    /// Parses a string of the form `algorithm:hex` (e.g. `sha256:abc123...`).
    pub fn parse(s: &str) -> Result<Self, InvalidSourceHash> {
        let Some((alg, hex)) = s.split_once(':') else {
            return Err(InvalidSourceHash::MissingColon {
                value: s.to_string(),
            });
        };
        let algorithm = alg.trim().to_lowercase();
        if algorithm.is_empty() {
            return Err(InvalidSourceHash::EmptyAlgorithm {
                value: s.to_string(),
            });
        }
        if !ALLOWED_ALGORITHMS.contains(&algorithm.as_str()) {
            return Err(InvalidSourceHash::UnknownAlgorithm {
                value: s.to_string(),
                algorithm: algorithm.clone(),
            });
        }
        let hex = hex.trim().to_lowercase();
        if hex.is_empty() {
            return Err(InvalidSourceHash::EmptyHex {
                value: s.to_string(),
            });
        }
        if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(InvalidSourceHash::InvalidHex {
                value: s.to_string(),
            });
        }
        Ok(SourceHash { algorithm, hex })
    }
}

impl FromStr for SourceHash {
    type Err = InvalidSourceHash;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl fmt::Display for SourceHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.algorithm, self.hex)
    }
}

/// Error when source hash string is invalid.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InvalidSourceHash {
    MissingColon { value: String },
    EmptyAlgorithm { value: String },
    UnknownAlgorithm { value: String, algorithm: String },
    EmptyHex { value: String },
    InvalidHex { value: String },
}

impl fmt::Display for InvalidSourceHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InvalidSourceHash::MissingColon { value } => {
                write!(f, "source hash must be algorithm:hex, got {:?}", value)
            }
            InvalidSourceHash::EmptyAlgorithm { value } => {
                write!(f, "source hash algorithm is empty in {:?}", value)
            }
            InvalidSourceHash::UnknownAlgorithm { value, algorithm } => {
                write!(f, "unknown hash algorithm {:?} in {:?}", algorithm, value)
            }
            InvalidSourceHash::EmptyHex { value } => {
                write!(f, "source hash hex is empty in {:?}", value)
            }
            InvalidSourceHash::InvalidHex { value } => {
                write!(f, "source hash hex is invalid in {:?}", value)
            }
        }
    }
}

impl std::error::Error for InvalidSourceHash {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_sha256() {
        let h = SourceHash::parse("sha256:deadbeef").unwrap();
        assert_eq!(h.algorithm, "sha256");
        assert_eq!(h.hex, "deadbeef");
    }

    #[test]
    fn parse_valid_sha512() {
        let h = SourceHash::parse("sha512:abc123").unwrap();
        assert_eq!(h.algorithm, "sha512");
        assert_eq!(h.hex, "abc123");
    }

    #[test]
    fn reject_no_colon() {
        assert!(SourceHash::parse("sha256").is_err());
    }

    #[test]
    fn reject_unknown_algorithm() {
        assert!(SourceHash::parse("md5:abc").is_err());
    }

    #[test]
    fn reject_invalid_hex() {
        assert!(SourceHash::parse("sha256:ghijkl").is_err());
    }

    #[test]
    fn reject_empty_hex() {
        assert!(SourceHash::parse("sha256:").is_err());
    }
}
