//! Manifest schema, parsing, and validation.

use std::fmt;
use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::dep_constraint::DepConstraint;
use crate::package_name::PackageName;
use crate::source::{InvalidSourceHash, Source, SourceHash};
use crate::version::Version;

/// Root TOML document: top-level key `[sic]` maps to manifest.
#[derive(Deserialize)]
pub struct ManifestRoot {
    #[serde(rename = "sic")]
    pub sic: RawManifest,
}

/// Raw manifest as read from TOML (all string/primitive types).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawManifest {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub revision: Option<u32>,
    pub source: RawSource,
    pub depends: Vec<String>,
    #[serde(default)]
    pub depends_any: Vec<Vec<String>>,
    #[serde(default)]
    pub recommends: Vec<String>,
    #[serde(default)]
    pub conflicts: Vec<String>,
    #[serde(default)]
    pub provides: Vec<String>,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub commands: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawSource {
    #[serde(rename = "type")]
    pub type_name: String,
    pub url: String,
    pub hash: String,
}

/// Validated in-memory manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Manifest {
    pub name: PackageName,
    pub version: Version,
    pub revision: u32,
    pub source: Source,
    pub depends: Vec<DepConstraint>,
    pub depends_any: Vec<Vec<DepConstraint>>,
    pub recommends: Vec<DepConstraint>,
    pub conflicts: Vec<PackageName>,
    pub provides: Vec<String>,
    pub files: Vec<String>,
    pub commands: Vec<String>,
}

impl TryFrom<RawManifest> for Manifest {
    type Error = ParseError;

    fn try_from(raw: RawManifest) -> Result<Self, Self::Error> {
        let name = PackageName::new(&raw.name).map_err(ParseError::InvalidName)?;
        let version = Version::new(&raw.version).map_err(|_| ParseError::InvalidVersion)?;
        let revision = raw.revision.unwrap_or(0);
        let hash = SourceHash::parse(&raw.source.hash).map_err(ParseError::InvalidHash)?;
        let source = Source {
            type_name: raw.source.type_name,
            url: raw.source.url,
            hash,
        };
        let mut depends = Vec::with_capacity(raw.depends.len());
        for s in &raw.depends {
            depends.push(DepConstraint::parse(s).map_err(ParseError::InvalidDepConstraint)?);
        }
        let mut depends_any = Vec::with_capacity(raw.depends_any.len());
        for group in &raw.depends_any {
            let mut g = Vec::with_capacity(group.len());
            for s in group {
                g.push(DepConstraint::parse(s).map_err(ParseError::InvalidDepConstraint)?);
            }
            depends_any.push(g);
        }
        let mut recommends = Vec::with_capacity(raw.recommends.len());
        for s in &raw.recommends {
            recommends.push(DepConstraint::parse(s).map_err(ParseError::InvalidDepConstraint)?);
        }
        let mut conflicts = Vec::with_capacity(raw.conflicts.len());
        for s in &raw.conflicts {
            conflicts.push(PackageName::new(s).map_err(ParseError::InvalidName)?);
        }
        Ok(Manifest {
            name,
            version,
            revision,
            source,
            depends,
            depends_any,
            recommends,
            conflicts,
            provides: raw.provides,
            files: raw.files,
            commands: raw.commands,
        })
    }
}

/// Error from manifest parsing (TOML or conversion).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParseError {
    /// TOML parse/deserialize error (message only for determinism).
    Toml(String),
    /// Invalid package name.
    InvalidName(crate::package_name::InvalidPackageName),
    /// Invalid version (e.g. empty).
    InvalidVersion,
    /// Invalid source hash.
    InvalidHash(InvalidSourceHash),
    /// Invalid dependency constraint.
    InvalidDepConstraint(crate::dep_constraint::InvalidDepConstraint),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Toml(msg) => write!(f, "manifest parse error: {}", msg),
            ParseError::InvalidName(e) => write!(f, "{}", e),
            ParseError::InvalidVersion => write!(f, "invalid or empty version"),
            ParseError::InvalidHash(e) => write!(f, "{}", e),
            ParseError::InvalidDepConstraint(e) => write!(f, "{}", e),
        }
    }
}

impl std::error::Error for ParseError {}

/// Error from manifest validation (paths, source URL, etc.).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValidationError {
    /// Invalid source URL.
    InvalidSourceUrl { value: String, reason: String },
    /// Absolute path or path traversal in files or commands.
    InvalidPath { value: String, reason: String },
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ValidationError::InvalidSourceUrl { value, reason } => {
                write!(f, "invalid source URL {:?}: {}", value, reason)
            }
            ValidationError::InvalidPath { value, reason } => {
                write!(f, "invalid path {:?}: {}", value, reason)
            }
        }
    }
}

impl std::error::Error for ValidationError {}

/// Parses manifest from string (TOML). Does not run validation.
pub fn parse(s: &str) -> Result<Manifest, ParseError> {
    let root: ManifestRoot = toml::from_str(s).map_err(|e| ParseError::Toml(e.to_string()))?;
    root.sic.try_into()
}

/// Parses manifest from file at path.
pub fn parse_path(path: &Path) -> Result<Manifest, ParseError> {
    let s = std::fs::read_to_string(path).map_err(|e| ParseError::Toml(e.to_string()))?;
    parse(&s)
}

/// Parses and validates manifest from file at path.
pub fn parse_path_and_validate(path: &Path) -> Result<Manifest, ParseOrValidationError> {
    let s = fs::read_to_string(path).map_err(|e| ParseOrValidationError::Parse(ParseError::Toml(e.to_string())))?;
    parse_and_validate(&s)
}

/// Loads all valid packages from a directory (files with `.toml` extension).
/// Skips files that fail to parse or validate.
pub fn load_packages_from_dir(dir: &Path) -> std::io::Result<Vec<Manifest>> {
    let mut out = Vec::new();
    if !dir.is_dir() {
        return Ok(out);
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().map_or(true, |e| e != "toml") {
            continue;
        }
        if path.is_file() {
            if let Ok(m) = parse_path_and_validate(&path) {
                out.push(m);
            }
        }
    }
    Ok(out)
}

/// Validates manifest (paths: no absolute, no traversal; source URL format).
pub fn validate(m: &Manifest) -> Result<(), ValidationError> {
    if m.source.url.trim().is_empty() {
        return Err(ValidationError::InvalidSourceUrl {
            value: m.source.url.clone(),
            reason: "source URL must be non-empty".to_string(),
        });
    }
    let u = m.source.url.trim();
    if !u.starts_with("http://") && !u.starts_with("https://") && !u.starts_with("file://") {
        return Err(ValidationError::InvalidSourceUrl {
            value: m.source.url.clone(),
            reason: "source URL must start with http://, https://, or file://".to_string(),
        });
    }
    for entry in m.files.iter().chain(m.commands.iter()) {
        validate_path_entry(entry)?;
    }
    Ok(())
}

fn validate_path_entry(entry: &str) -> Result<(), ValidationError> {
    let entry = entry.trim();
    if entry.is_empty() {
        return Ok(());
    }
    if entry.starts_with('/') {
        return Err(ValidationError::InvalidPath {
            value: entry.to_string(),
            reason: "absolute path not allowed".to_string(),
        });
    }
    #[cfg(windows)]
    if entry.len() >= 2 && entry.as_bytes()[1] == b':' {
        return Err(ValidationError::InvalidPath {
            value: entry.to_string(),
            reason: "Windows drive path not allowed".to_string(),
        });
    }
    for segment in entry.split('/') {
        if segment == ".." {
            return Err(ValidationError::InvalidPath {
                value: entry.to_string(),
                reason: "path traversal (..) not allowed".to_string(),
            });
        }
    }
    Ok(())
}

/// Parses and validates manifest.
pub fn parse_and_validate(s: &str) -> Result<Manifest, ParseOrValidationError> {
    let m = parse(s).map_err(ParseOrValidationError::Parse)?;
    validate(&m).map_err(ParseOrValidationError::Validation)?;
    Ok(m)
}

/// Combined parse or validation error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParseOrValidationError {
    Parse(ParseError),
    Validation(ValidationError),
}

impl fmt::Display for ParseOrValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseOrValidationError::Parse(e) => write!(f, "{}", e),
            ParseOrValidationError::Validation(e) => write!(f, "{}", e),
        }
    }
}

impl std::error::Error for ParseOrValidationError {}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL_MANIFEST: &str = r#"
[sic]
name = "foo"
version = "1.0"
source = { type = "tarball", url = "https://example.com/foo.tar.gz", hash = "sha256:deadbeef" }
depends = []
"#;

    const FULL_MANIFEST: &str = r#"
[sic]
name = "helix"
version = "24.03"
revision = 0
source = { type = "tarball", url = "https://helix-editor.com/helix.tar.gz", hash = "sha256:abcd1234" }
depends = ["ripgrep >= 13"]
depends_any = []
recommends = []
conflicts = []
provides = ["editor"]
files = ["bin/helix", "share/helix/*"]
commands = ["helix"]
"#;

    #[test]
    fn parse_valid_minimal() {
        let m = parse(MINIMAL_MANIFEST).unwrap();
        assert_eq!(m.name.as_str(), "foo");
        assert_eq!(m.version.as_str(), "1.0");
        assert_eq!(m.revision, 0);
        assert_eq!(m.source.type_name, "tarball");
        assert_eq!(m.source.url, "https://example.com/foo.tar.gz");
        assert_eq!(m.depends.len(), 0);
    }

    #[test]
    fn parse_valid_full() {
        let m = parse(FULL_MANIFEST).unwrap();
        assert_eq!(m.name.as_str(), "helix");
        assert_eq!(m.version.as_str(), "24.03");
        assert_eq!(m.revision, 0);
        assert_eq!(m.depends.len(), 1);
        assert_eq!(m.depends[0].name.as_str(), "ripgrep");
        assert_eq!(m.provides, &["editor"]);
        assert_eq!(m.files, &["bin/helix", "share/helix/*"]);
        assert_eq!(m.commands, &["helix"]);
    }

    #[test]
    fn parse_invalid_missing_name() {
        let s = r#"
[sic]
version = "1.0"
source = { type = "tarball", url = "https://x.com/x.tar.gz", hash = "sha256:ab" }
depends = []
"#;
        let r = parse(s);
        assert!(r.is_err());
        if let Err(ParseError::Toml(_)) = r {
        } else {
            panic!("expected Toml error, got {:?}", r);
        }
    }

    #[test]
    fn parse_invalid_name_uppercase() {
        let s = r#"
[sic]
name = "Helix"
version = "24.03"
source = { type = "tarball", url = "https://x.com/x.tar.gz", hash = "sha256:abcd" }
depends = []
"#;
        let r = parse(s);
        assert!(r.is_err());
        if let Err(ParseError::InvalidName(_)) = r {
        } else {
            panic!("expected InvalidName, got {:?}", r);
        }
    }

    #[test]
    fn parse_invalid_hash_format() {
        let s = r#"
[sic]
name = "foo"
version = "1.0"
source = { type = "tarball", url = "https://x.com/x.tar.gz", hash = "not-algorithm-hex" }
depends = []
"#;
        let r = parse(s);
        assert!(r.is_err());
        if let Err(ParseError::InvalidHash(_)) = r {
        } else {
            panic!("expected InvalidHash, got {:?}", r);
        }
    }

    #[test]
    fn validate_rejects_absolute_path() {
        let s = r#"
[sic]
name = "foo"
version = "1.0"
source = { type = "tarball", url = "https://x.com/x.tar.gz", hash = "sha256:abcd" }
depends = []
files = ["/absolute/path"]
"#;
        let m = parse(s).unwrap();
        let r = validate(&m);
        assert!(r.is_err());
        if let Err(ValidationError::InvalidPath { .. }) = r {
        } else {
            panic!("expected InvalidPath, got {:?}", r);
        }
    }

    #[test]
    fn validate_rejects_path_traversal() {
        let s = r#"
[sic]
name = "foo"
version = "1.0"
source = { type = "tarball", url = "https://x.com/x.tar.gz", hash = "sha256:abcd" }
depends = []
files = ["bin/foo", "../etc/passwd"]
"#;
        let m = parse(s).unwrap();
        let r = validate(&m);
        assert!(r.is_err());
        if let Err(ValidationError::InvalidPath { .. }) = r {
        } else {
            panic!("expected InvalidPath, got {:?}", r);
        }
    }

    #[test]
    fn validate_allows_segment_containing_dotdot() {
        // Segment "a..b" is not traversal; only ".." as a component is rejected.
        let s = r#"
[sic]
name = "foo"
version = "1.0"
source = { type = "tarball", url = "https://x.com/x.tar.gz", hash = "sha256:abcd" }
depends = []
files = ["share/helix/a..b"]
"#;
        let m = parse(s).unwrap();
        let r = validate(&m);
        assert!(r.is_ok(), "a..b in path segment should be allowed: {:?}", r);
    }

    #[test]
    fn parse_and_validate_success() {
        let m = parse_and_validate(MINIMAL_MANIFEST).unwrap();
        assert_eq!(m.name.as_str(), "foo");
    }

    #[test]
    fn parse_and_validate_fails_on_invalid_url() {
        let s = r#"
[sic]
name = "foo"
version = "1.0"
source = { type = "tarball", url = "", hash = "sha256:abcd" }
depends = []
"#;
        let m = parse(s).unwrap();
        let r = validate(&m);
        assert!(r.is_err());
        if let Err(ValidationError::InvalidSourceUrl { .. }) = r {
        } else {
            panic!("expected InvalidSourceUrl, got {:?}", r);
        }
    }
}
