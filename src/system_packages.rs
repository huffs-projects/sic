//! Read-only view of system-installed packages (e.g. from dpkg).
//!
//! Used by the resolver to treat system packages as satisfied_system for implicit
//! dependencies. No writes to system dirs. Debian and derivatives; if dpkg is
//! not present or unreadable, treat as empty set.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::package_name::PackageName;
use crate::version::Version;

/// Default path to dpkg status file (Debian and derivatives).
pub const DPKG_STATUS_PATH: &str = "/var/lib/dpkg/status";

/// Read-only set of system packages: name -> version. Used to satisfy implicit deps.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SystemPackages(BTreeMap<PackageName, Version>);

impl SystemPackages {
    /// Returns the version of the system package with the given name, if any.
    pub fn get(&self, name: &PackageName) -> Option<Version> {
        self.0.get(name).cloned()
    }

    /// Returns true if the system has a package with this name.
    pub fn contains(&self, name: &PackageName) -> bool {
        self.0.contains_key(name)
    }

    /// Build from a map (e.g. for tests or mock). Only entries with valid
    /// PackageName and Version are included.
    pub fn from_map(entries: impl IntoIterator<Item = (impl AsRef<str>, impl AsRef<str>)>) -> Self {
        let mut map = BTreeMap::new();
        for (name, version) in entries {
            if let (Ok(n), Ok(v)) = (
                PackageName::new(name.as_ref()),
                Version::new(version.as_ref()),
            ) {
                map.insert(n, v);
            }
        }
        SystemPackages(map)
    }

    /// Load system packages from the dpkg status file at the given path.
    /// Missing file or parse errors return an empty set (no error).
    /// Best-effort on non-Debian; document as Debian and derivatives.
    pub fn load(path: &Path) -> Self {
        let contents = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => return SystemPackages::default(),
        };
        Self::parse_status(&contents)
    }

    /// Load from default dpkg status path.
    pub fn load_default() -> Self {
        Self::load(Path::new(DPKG_STATUS_PATH))
    }

    /// Parse status file contents. Paragraphs separated by blank lines; each
    /// paragraph has "Package: name", "Version: value", and "Status: ...".
    /// Only packages with Status indicating installed (e.g. "install ok installed")
    /// and names valid for sic (PackageName) are included.
    fn parse_status(contents: &str) -> Self {
        let mut map = BTreeMap::new();
        for block in contents.split("\n\n") {
            let block = block.trim();
            if block.is_empty() {
                continue;
            }
            let mut name: Option<PackageName> = None;
            let mut version: Option<Version> = None;
            let mut status_installed = false;
            for line in block.lines() {
                if let Some(rest) = line.strip_prefix("Package:") {
                    let s = rest.trim().to_lowercase();
                    if let Ok(n) = PackageName::new(s) {
                        name = Some(n);
                    }
                } else if let Some(rest) = line.strip_prefix("Version:") {
                    let s = rest.trim();
                    if let Ok(v) = Version::new(s) {
                        version = Some(v);
                    }
                } else if let Some(rest) = line.strip_prefix("Status:") {
                    let s = rest.trim().to_lowercase();
                    status_installed = s == "install ok installed";
                }
            }
            if status_installed {
                if let (Some(n), Some(v)) = (name, version) {
                    map.insert(n, v);
                }
            }
        }
        SystemPackages(map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_map_empty() {
        let s = SystemPackages::from_map(std::iter::empty::<(&str, &str)>());
        assert!(s.0.is_empty());
    }

    #[test]
    fn from_map_ripgrep() {
        let s = SystemPackages::from_map([("ripgrep", "13.0")]);
        let name = PackageName::new("ripgrep").unwrap();
        assert_eq!(s.get(&name).map(|v| v.as_str().to_string()), Some("13.0".to_string()));
    }

    #[test]
    fn parse_status_single_paragraph() {
        let contents = "Package: ripgrep\nVersion: 13.0\nStatus: install ok installed\n";
        let s = SystemPackages::parse_status(contents);
        let name = PackageName::new("ripgrep").unwrap();
        assert_eq!(s.get(&name).map(|v| v.as_str().to_string()), Some("13.0".to_string()));
    }

    #[test]
    fn parse_status_two_paragraphs() {
        let contents = "Package: a\nVersion: 1.0\nStatus: install ok installed\n\n\
                        Package: b\nVersion: 2.0\nStatus: install ok installed\n";
        let s = SystemPackages::parse_status(contents);
        assert_eq!(
            s.get(&PackageName::new("a").unwrap()).map(|v| v.as_str().to_string()),
            Some("1.0".to_string())
        );
        assert_eq!(
            s.get(&PackageName::new("b").unwrap()).map(|v| v.as_str().to_string()),
            Some("2.0".to_string())
        );
    }

    #[test]
    fn parse_status_empty_returns_default() {
        let s = SystemPackages::parse_status("");
        assert!(s.0.is_empty());
    }

    #[test]
    fn parse_status_invalid_name_skipped() {
        // Package name with invalid char (e.g. +) is skipped by PackageName::new
        let contents = "Package: libstdc++\nVersion: 1.0\nStatus: install ok installed\n\n\
                        Package: ripgrep\nVersion: 13.0\nStatus: install ok installed\n";
        let s = SystemPackages::parse_status(contents);
        assert_eq!(
            s.get(&PackageName::new("ripgrep").unwrap()).map(|v| v.as_str().to_string()),
            Some("13.0".to_string())
        );
        assert_eq!(s.0.len(), 1);
    }

    #[test]
    fn parse_status_only_installed_included() {
        // Only paragraphs with Status indicating installed are included.
        let contents = "Package: old\nVersion: 1.0\nStatus: deinstall ok config-files\n\n\
                        Package: ripgrep\nVersion: 13.0\nStatus: install ok installed\n";
        let s = SystemPackages::parse_status(contents);
        assert!(s.get(&PackageName::new("old").unwrap()).is_none());
        assert_eq!(
            s.get(&PackageName::new("ripgrep").unwrap()).map(|v| v.as_str().to_string()),
            Some("13.0".to_string())
        );
        assert_eq!(s.0.len(), 1);
    }
}
