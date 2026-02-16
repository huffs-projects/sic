//! Package name: validated string, lowercase `[a-z0-9-_]`, non-empty.

use std::fmt;

use serde::de::{self, Visitor};
use serde::{Deserialize, Serialize};

/// Valid package name: non-empty, only `[a-z0-9-_]`.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct PackageName(String);

impl<'de> Deserialize<'de> for PackageName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct PackageNameVisitor;
        impl Visitor<'_> for PackageNameVisitor {
            type Value = PackageName;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a valid package name (lowercase [a-z0-9-_], non-empty)")
            }
            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                PackageName::new(v).map_err(de::Error::custom)
            }
        }
        deserializer.deserialize_str(PackageNameVisitor)
    }
}

/// Error when a string is not a valid package name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvalidPackageName {
    /// The rejected value.
    pub value: String,
}

impl fmt::Display for InvalidPackageName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid package name: {:?}", self.value)
    }
}

impl std::error::Error for InvalidPackageName {}

impl PackageName {
    /// Creates a package name from a string if it is valid.
    ///
    /// Valid: non-empty, only lowercase letters, digits, hyphen, underscore.
    pub fn new(s: impl Into<String>) -> Result<Self, InvalidPackageName> {
        let s = s.into();
        if s.is_empty() {
            return Err(InvalidPackageName { value: s });
        }
        for c in s.chars() {
            if !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '-' && c != '_' {
                return Err(InvalidPackageName { value: s });
            }
        }
        Ok(PackageName(s))
    }

    /// Returns the name as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PackageName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl AsRef<str> for PackageName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_names() {
        assert!(PackageName::new("helix").is_ok());
        assert!(PackageName::new("ripgrep").is_ok());
        assert!(PackageName::new("a").is_ok());
        assert!(PackageName::new("a-b_c").is_ok());
        assert!(PackageName::new("pkg42").is_ok());
    }

    #[test]
    fn empty_rejected() {
        let r = PackageName::new("");
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().value, "");
    }

    #[test]
    fn uppercase_rejected() {
        let r = PackageName::new("Helix");
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().value, "Helix");
    }

    #[test]
    fn invalid_chars_rejected() {
        assert!(PackageName::new("helix.editor").is_err());
        assert!(PackageName::new("helix editor").is_err());
    }
}
