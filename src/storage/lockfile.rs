//! sic.lock: resolved package set with exact versions and sources.

use std::fmt;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::package_name::PackageName;
use crate::source::Source;
use crate::version::Version;

/// One locked package in sic.lock.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LockfilePackage {
    pub name: PackageName,
    pub version: Version,
    pub revision: u32,
    pub source: Source,
}

/// Root lockfile: [[packages]] array.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Lockfile {
    pub packages: Vec<LockfilePackage>,
}

impl Lockfile {
    /// Loads lockfile from path. Missing file or non-file (e.g. directory) returns Ok(None).
    /// Parse error returns Err.
    pub fn load(path: &Path) -> Result<Option<Lockfile>, LockfileLoadError> {
        if !path.exists() || !path.is_file() {
            return Ok(None);
        }
        let s = fs::read_to_string(path).map_err(|e| LockfileLoadError::Io(e.to_string()))?;
        let lf: Lockfile =
            toml::from_str(&s).map_err(|e| LockfileLoadError::Parse(e.to_string()))?;
        Ok(Some(lf))
    }

    /// Writes lockfile atomically: temp file in same dir then rename to target path.
    pub fn write(&self, path: &Path) -> Result<(), LockfileWriteError> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).map_err(|e| LockfileWriteError::Io(e.to_string()))?;
        let contents = toml::to_string_pretty(self)
            .map_err(|e| LockfileWriteError::Serialize(e.to_string()))?;
        let mut temp_path = parent.join(path.file_name().unwrap_or_default());
        temp_path.set_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        fs::write(&temp_path, contents).map_err(|e| LockfileWriteError::Io(e.to_string()))?;
        fs::rename(&temp_path, path).map_err(|e| LockfileWriteError::Io(e.to_string()))?;
        Ok(())
    }

    /// Returns all locked packages with the given name (for resolver lookup).
    pub fn packages_for_name(&self, name: &PackageName) -> Vec<&LockfilePackage> {
        self.packages.iter().filter(|p| &p.name == name).collect()
    }
}

/// Error loading lockfile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LockfileLoadError {
    Io(String),
    Parse(String),
}

impl fmt::Display for LockfileLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LockfileLoadError::Io(e) => write!(f, "lockfile read error: {}", e),
            LockfileLoadError::Parse(e) => write!(f, "lockfile parse error: {}", e),
        }
    }
}

impl std::error::Error for LockfileLoadError {}

/// Error writing lockfile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LockfileWriteError {
    Io(String),
    Serialize(String),
}

impl fmt::Display for LockfileWriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LockfileWriteError::Io(e) => write!(f, "lockfile write error: {}", e),
            LockfileWriteError::Serialize(e) => write!(f, "lockfile serialize error: {}", e),
        }
    }
}

impl std::error::Error for LockfileWriteError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::SourceHash;

    const EXAMPLE_LOCKFILE: &str = r#"
[[packages]]
name = "helix"
version = "24.03"
revision = 1
source = { type = "tarball", url = "https://helix-editor.com/helix.tar.gz", hash = "sha256:abcd1234" }

[[packages]]
name = "ripgrep"
version = "13.0"
revision = 2
source = { type = "tarball", url = "https://github.com/BurntSushi/ripgrep/releases/download/13.0/ripgrep.tar.gz", hash = "sha256:deadbeef" }
"#;

    #[test]
    fn parse_example_from_design() {
        let lf: Lockfile = toml::from_str(EXAMPLE_LOCKFILE).unwrap();
        assert_eq!(lf.packages.len(), 2);
        assert_eq!(lf.packages[0].name.as_str(), "helix");
        assert_eq!(lf.packages[0].version.as_str(), "24.03");
        assert_eq!(lf.packages[0].revision, 1);
        assert_eq!(lf.packages[0].source.type_name, "tarball");
        assert_eq!(
            lf.packages[0].source.url,
            "https://helix-editor.com/helix.tar.gz"
        );
        assert_eq!(lf.packages[0].source.hash.algorithm, "sha256");
        assert_eq!(lf.packages[0].source.hash.hex, "abcd1234");
        assert_eq!(lf.packages[1].name.as_str(), "ripgrep");
    }

    #[test]
    fn write_then_read_roundtrip() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("sic.lock");
        let lf = Lockfile {
            packages: vec![LockfilePackage {
                name: PackageName::new("foo").unwrap(),
                version: Version::new("1.0").unwrap(),
                revision: 0,
                source: Source {
                    type_name: "tarball".to_string(),
                    url: "https://example.com/foo.tar.gz".to_string(),
                    hash: SourceHash::parse("sha256:abc123").unwrap(),
                },
            }],
        };
        lf.write(&path).unwrap();
        let loaded = Lockfile::load(&path).unwrap().unwrap();
        assert_eq!(loaded.packages.len(), 1);
        assert_eq!(loaded.packages[0].name.as_str(), "foo");
        assert_eq!(loaded.packages[0].source.hash.hex, "abc123");
    }

    #[test]
    fn missing_file_returns_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("nonexistent.lock");
        let r = Lockfile::load(&path).unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn load_when_path_is_directory_returns_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("sic.lock");
        fs::create_dir(&path).unwrap();
        let r = Lockfile::load(&path).unwrap();
        assert!(r.is_none());
    }
}
