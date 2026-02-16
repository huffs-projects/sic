//! installed.toml: installed package database under prefix/var/installed.toml.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::package_name::PackageName;
use crate::version::Version;

const INSTALLED_FILENAME: &str = "installed.toml";

/// One installed package entry in the database.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InstalledEntry {
    pub name: PackageName,
    pub version: Version,
    pub revision: u32,
    /// Path under prefix, e.g. "pkgs/helix-24.03".
    pub install_path: String,
    /// Relative paths of installed files.
    pub files: Vec<String>,
    /// Optional checksums per file for rollback/verification (Phase 7).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_checksums: Vec<String>,
}

/// Root TOML structure: [[packages]] array.
#[derive(Serialize, Deserialize)]
struct InstalledRoot {
    packages: Vec<InstalledEntry>,
}

/// Installed package database. Supports lookup by name and list all; safe replacement via atomic write.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InstalledDb(Vec<InstalledEntry>);

impl InstalledDb {
    /// Loads the database from `prefix/var/installed.toml`. Missing file or non-file (e.g. directory)
    /// at that path returns empty db (no error).
    pub fn load(prefix: &Path) -> Result<Self, InstalledLoadError> {
        let path = prefix.join("var").join(INSTALLED_FILENAME);
        if !path.exists() || !path.is_file() {
            return Ok(InstalledDb::default());
        }
        let s = fs::read_to_string(&path).map_err(|e| InstalledLoadError::Io(e.to_string()))?;
        let root: InstalledRoot =
            toml::from_str(&s).map_err(|e| InstalledLoadError::Parse(e.to_string()))?;
        // Dedupe by package name (last wins) so get_by_name and resolver see one entry per name.
        let mut by_name: BTreeMap<PackageName, InstalledEntry> = BTreeMap::new();
        for e in root.packages {
            by_name.insert(e.name.clone(), e);
        }
        Ok(InstalledDb(by_name.into_values().collect()))
    }

    /// Writes the database atomically: temp file in same dir then rename to installed.toml.
    pub fn write(&self, prefix: &Path) -> Result<(), InstalledWriteError> {
        let var_dir = prefix.join("var");
        fs::create_dir_all(&var_dir).map_err(|e| InstalledWriteError::Io(e.to_string()))?;
        let target = var_dir.join(INSTALLED_FILENAME);
        let root = InstalledRoot {
            packages: self.0.clone(),
        };
        let contents = toml::to_string_pretty(&root)
            .map_err(|e| InstalledWriteError::Serialize(e.to_string()))?;
        let mut temp_path = var_dir.join(INSTALLED_FILENAME);
        temp_path.set_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        fs::write(&temp_path, contents).map_err(|e| InstalledWriteError::Io(e.to_string()))?;
        fs::rename(&temp_path, &target).map_err(|e| InstalledWriteError::Io(e.to_string()))?;
        Ok(())
    }

    /// Returns all installed entries.
    pub fn list_all(&self) -> &[InstalledEntry] {
        &self.0
    }

    /// Lookup by package name.
    pub fn get_by_name(&self, name: &PackageName) -> Option<&InstalledEntry> {
        self.0.iter().find(|e| &e.name == name)
    }
}

impl From<Vec<InstalledEntry>> for InstalledDb {
    fn from(entries: Vec<InstalledEntry>) -> Self {
        let mut by_name: BTreeMap<PackageName, InstalledEntry> = BTreeMap::new();
        for e in entries {
            by_name.insert(e.name.clone(), e);
        }
        InstalledDb(by_name.into_values().collect())
    }
}

/// Error loading installed.toml.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstalledLoadError {
    Io(String),
    Parse(String),
}

impl fmt::Display for InstalledLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InstalledLoadError::Io(e) => write!(f, "installed.toml read error: {}", e),
            InstalledLoadError::Parse(e) => write!(f, "installed.toml parse error: {}", e),
        }
    }
}

impl std::error::Error for InstalledLoadError {}

/// Error writing installed.toml.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstalledWriteError {
    Io(String),
    Serialize(String),
}

impl fmt::Display for InstalledWriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InstalledWriteError::Io(e) => write!(f, "installed.toml write error: {}", e),
            InstalledWriteError::Serialize(e) => write!(f, "installed.toml serialize error: {}", e),
        }
    }
}

impl std::error::Error for InstalledWriteError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package_name::PackageName;
    use crate::version::Version;

    #[test]
    fn load_missing_file_returns_empty() {
        let tmp = tempfile::TempDir::new().unwrap();
        let prefix = tmp.path();
        let db = InstalledDb::load(prefix).unwrap();
        assert!(db.list_all().is_empty());
        assert!(db
            .get_by_name(&PackageName::new("helix").unwrap())
            .is_none());
    }

    #[test]
    fn load_when_path_is_directory_returns_empty() {
        let tmp = tempfile::TempDir::new().unwrap();
        let prefix = tmp.path();
        let var = prefix.join("var");
        fs::create_dir_all(&var).unwrap();
        let installed_path = var.join("installed.toml");
        fs::create_dir(installed_path).unwrap();
        let db = InstalledDb::load(prefix).unwrap();
        assert!(db.list_all().is_empty());
    }

    #[test]
    fn load_one_package() {
        let tmp = tempfile::TempDir::new().unwrap();
        let prefix = tmp.path();
        let var = prefix.join("var");
        fs::create_dir_all(&var).unwrap();
        let content = r#"
[[packages]]
name = "helix"
version = "24.03"
revision = 1
install_path = "pkgs/helix-24.03"
files = ["bin/helix", "share/helix/"]
"#;
        fs::write(var.join("installed.toml"), content).unwrap();
        let db = InstalledDb::load(prefix).unwrap();
        let entries = db.list_all();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name.as_str(), "helix");
        assert_eq!(entries[0].version.as_str(), "24.03");
        assert_eq!(entries[0].revision, 1);
        assert_eq!(entries[0].install_path, "pkgs/helix-24.03");
        assert_eq!(entries[0].files, &["bin/helix", "share/helix/"]);
        let helix = db.get_by_name(&PackageName::new("helix").unwrap()).unwrap();
        assert_eq!(helix.version.as_str(), "24.03");
    }

    #[test]
    fn write_then_read_roundtrip() {
        let tmp = tempfile::TempDir::new().unwrap();
        let prefix = tmp.path();
        crate::prefix::ensure_layout(prefix).unwrap();
        let db = InstalledDb::from(vec![InstalledEntry {
            name: PackageName::new("ripgrep").unwrap(),
            version: Version::new("13.0").unwrap(),
            revision: 0,
            install_path: "pkgs/ripgrep-13.0".to_string(),
            files: vec!["bin/rg".to_string()],
            file_checksums: vec![],
        }]);
        db.write(prefix).unwrap();
        let loaded = InstalledDb::load(prefix).unwrap();
        assert_eq!(loaded.list_all().len(), 1);
        assert_eq!(loaded.list_all()[0].name.as_str(), "ripgrep");
        assert_eq!(loaded.list_all()[0].version.as_str(), "13.0");
        assert_eq!(loaded.list_all()[0].install_path, "pkgs/ripgrep-13.0");
    }

    #[test]
    fn load_duplicate_package_name_keeps_last() {
        let tmp = tempfile::TempDir::new().unwrap();
        let prefix = tmp.path();
        let var = prefix.join("var");
        fs::create_dir_all(&var).unwrap();
        let content = r#"
[[packages]]
name = "foo"
version = "1.0"
revision = 0
install_path = "pkgs/foo-1.0"
files = []

[[packages]]
name = "foo"
version = "2.0"
revision = 0
install_path = "pkgs/foo-2.0"
files = []
"#;
        fs::write(var.join("installed.toml"), content).unwrap();
        let db = InstalledDb::load(prefix).unwrap();
        let entries = db.list_all();
        assert_eq!(entries.len(), 1, "duplicate name should dedupe to one entry");
        assert_eq!(entries[0].version.as_str(), "2.0", "last occurrence wins");
        assert_eq!(
            db.get_by_name(&PackageName::new("foo").unwrap()).unwrap().version.as_str(),
            "2.0"
        );
    }

    #[test]
    fn atomic_write_final_file_is_installed_toml_temp_gone() {
        let tmp = tempfile::TempDir::new().unwrap();
        let prefix = tmp.path();
        crate::prefix::ensure_layout(prefix).unwrap();
        let var = prefix.join("var");
        let db = InstalledDb::from(vec![InstalledEntry {
            name: PackageName::new("foo").unwrap(),
            version: Version::new("1.0").unwrap(),
            revision: 0,
            install_path: "pkgs/foo-1.0".to_string(),
            files: vec![],
            file_checksums: vec![],
        }]);
        db.write(prefix).unwrap();
        let target = var.join("installed.toml");
        assert!(target.exists(), "installed.toml must exist after write");
        let temp_count = fs::read_dir(&var)
            .unwrap()
            .filter(|e| {
                let e = e.as_ref().unwrap();
                e.path().extension().is_some_and(|ext| ext == "tmp")
            })
            .count();
        assert_eq!(temp_count, 0, "no .tmp file should remain after rename");
    }
}
