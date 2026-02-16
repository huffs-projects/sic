//! Version: orderable string (dotted-numeric with optional suffix).

use std::cmp::Ordering;
use std::fmt;

use serde::de::{self, Visitor};
use serde::{Deserialize, Serialize};

/// Orderable version: dotted-numeric segments, then optional lexicographic suffix.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Version(String);

impl<'de> Deserialize<'de> for Version {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct VersionVisitor;
        impl Visitor<'_> for VersionVisitor {
            type Value = Version;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a non-empty version string")
            }
            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Version::new(v).map_err(de::Error::custom)
            }
        }
        deserializer.deserialize_str(VersionVisitor)
    }
}

impl Version {
    /// Creates a version from a non-empty string.
    pub fn new(s: impl Into<String>) -> Result<Self, EmptyVersion> {
        let s = s.into();
        if s.trim().is_empty() {
            return Err(EmptyVersion);
        }
        Ok(Version(s))
    }

    /// Returns the version string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Compares two versions: numeric segments first (by value), then remainder lexicographically.
    fn cmp_segments(a: &str, b: &str) -> Ordering {
        let mut a_parts = a.split('.');
        let mut b_parts = b.split('.');
        loop {
            match (a_parts.next(), b_parts.next()) {
                (None, None) => return Ordering::Equal,
                (None, Some(_)) => return Ordering::Less,
                (Some(_), None) => return Ordering::Greater,
                (Some(sa), Some(sb)) => {
                    let (na, ta) = split_numeric_prefix(sa);
                    let (nb, tb) = split_numeric_prefix(sb);
                    match (na, nb) {
                        (Some(va), Some(vb)) => {
                            let c = va.cmp(&vb);
                            if c != Ordering::Equal {
                                return c;
                            }
                            let c = ta.cmp(tb);
                            if c != Ordering::Equal {
                                return c;
                            }
                        }
                        (Some(_), None) => return Ordering::Greater,
                        (None, Some(_)) => return Ordering::Less,
                        (None, None) => {
                            let c = sa.cmp(sb);
                            if c != Ordering::Equal {
                                return c;
                            }
                        }
                    }
                }
            }
        }
    }
}

fn split_numeric_prefix(s: &str) -> (Option<u64>, &str) {
    let digits_end = s
        .char_indices()
        .take_while(|(_, c)| c.is_ascii_digit())
        .last()
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    let num_str = &s[..digits_end];
    let rest = &s[digits_end..];
    if num_str.is_empty() {
        (None, s)
    } else {
        match num_str.parse::<u64>() {
            Ok(n) => (Some(n), rest),
            Err(_) => (None, s),
        }
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        Self::cmp_segments(&self.0, &other.0)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl AsRef<str> for Version {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Error when version string is empty.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmptyVersion;

impl fmt::Display for EmptyVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "version must be non-empty")
    }
}

impl std::error::Error for EmptyVersion {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_ordering() {
        assert!(Version::new("13").unwrap() < Version::new("24.03").unwrap());
        assert!(Version::new("24.03").unwrap() > Version::new("13").unwrap());
        assert!(Version::new("1.0").unwrap() < Version::new("1.0.0").unwrap());
        assert!(Version::new("1.0.0").unwrap() > Version::new("1.0").unwrap());
        assert_eq!(
            Version::new("24.03")
                .unwrap()
                .cmp(&Version::new("24.03").unwrap()),
            Ordering::Equal
        );
    }

    #[test]
    fn empty_rejected() {
        assert!(Version::new("").is_err());
        assert!(Version::new("   ").is_err());
    }
}
