//! Read-only view of system-installed packages from common Unix package databases.
//!
//! Used by the resolver to treat system packages as satisfied_system for implicit
//! dependencies. No writes to system dirs. On Unix, [`SystemPackages::load_default`]
//! merges best-effort reads from dpkg status, pacman local metadata, Alpine apk
//! `installed`, and Homebrew Cellar layouts. Missing files or parse errors for a
//! source are skipped. On non-Unix platforms the set is empty.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::package_name::PackageName;
use crate::version::Version;

/// Path to the dpkg status file on Debian-based systems (one source used by [`SystemPackages::load_default`]).
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
    /// [`PackageName`] and [`Version`] are included.
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

    /// Load system packages from a dpkg status file at the given path.
    /// Missing file or parse errors return an empty contribution (no error).
    pub fn load_dpkg_status(path: &Path) -> Self {
        let contents = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => return SystemPackages::default(),
        };
        Self::parse_dpkg_status(&contents)
    }

    /// Load from the default dpkg status path ([`DPKG_STATUS_PATH`]).
    pub fn load_dpkg_default() -> Self {
        Self::load_dpkg_status(Path::new(DPKG_STATUS_PATH))
    }

    /// Same as [`Self::load_dpkg_status`].
    pub fn load(path: &Path) -> Self {
        Self::load_dpkg_status(path)
    }

    /// Merge entries from `other` into `self`. Existing names keep the version already in `self`.
    pub fn merge_from(&mut self, other: SystemPackages) {
        for (k, v) in other.0 {
            self.0.entry(k).or_insert(v);
        }
    }

    /// Best-effort load for the current platform.
    ///
    /// On Unix, merges dpkg, pacman (`/var/lib/pacman/local`), Alpine apk (`/lib/apk/db/installed`),
    /// and Homebrew (`/opt/homebrew/Cellar`, `/usr/local/Cellar`). Earlier sources win on duplicate
    /// package names. On non-Unix, returns an empty set.
    pub fn load_default() -> Self {
        #[cfg(unix)]
        {
            let mut acc = SystemPackages::default();
            acc.merge_from(Self::load_dpkg_default());
            acc.merge_from(Self::load_pacman_local());
            acc.merge_from(Self::load_apk_installed());
            acc.merge_from(Self::load_homebrew_cellar());
            acc
        }
        #[cfg(not(unix))]
        {
            SystemPackages::default()
        }
    }

    /// Parse dpkg status file contents. Paragraphs separated by blank lines; each
    /// paragraph has "Package: name", "Version: value", and "Status: ...".
    /// Only packages with Status indicating installed (e.g. "install ok installed")
    /// and names valid for sic ([`PackageName`]) are included.
    fn parse_dpkg_status(contents: &str) -> Self {
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

    fn load_pacman_local() -> Self {
        let base = Path::new("/var/lib/pacman/local");
        let Ok(entries) = fs::read_dir(base) else {
            return SystemPackages::default();
        };
        let mut map = BTreeMap::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let desc = path.join("desc");
            let Ok(text) = fs::read_to_string(&desc) else {
                continue;
            };
            if let Some((name, ver)) = parse_pacman_desc(&text) {
                if let (Ok(n), Ok(v)) = (PackageName::new(&name), Version::new(&ver)) {
                    map.entry(n).or_insert(v);
                }
            }
        }
        SystemPackages(map)
    }

    fn load_apk_installed() -> Self {
        let path = Path::new("/lib/apk/db/installed");
        let Ok(contents) = fs::read_to_string(path) else {
            return SystemPackages::default();
        };
        Self::parse_apk_installed(&contents)
    }

    fn parse_apk_installed(contents: &str) -> Self {
        let mut map = BTreeMap::new();
        let mut name: Option<String> = None;
        let mut version: Option<String> = None;

        let flush = |pn: &mut Option<String>, pv: &mut Option<String>, m: &mut BTreeMap<PackageName, Version>| {
            if let (Some(n), Some(v)) = (pn.take(), pv.take()) {
                if let (Ok(pkg), Ok(ver)) = (PackageName::new(&n), Version::new(&v)) {
                    m.entry(pkg).or_insert(ver);
                }
            }
        };

        for line in contents.lines() {
            if line.starts_with("C:") {
                flush(&mut name, &mut version, &mut map);
                continue;
            }
            if let Some(r) = line.strip_prefix("P:") {
                name = Some(r.trim().to_lowercase());
            } else if let Some(r) = line.strip_prefix("V:") {
                version = Some(r.trim().to_string());
            }
        }
        flush(&mut name, &mut version, &mut map);

        SystemPackages(map)
    }

    fn load_homebrew_cellar() -> Self {
        let mut map = BTreeMap::new();
        for base in [Path::new("/opt/homebrew/Cellar"), Path::new("/usr/local/Cellar")] {
            let Ok(entries) = fs::read_dir(base) else {
                continue;
            };
            for entry in entries.flatten() {
                let pkg_dir = entry.path();
                if !pkg_dir.is_dir() {
                    continue;
                }
                let Some(raw_name) = pkg_dir.file_name().and_then(|s| s.to_str()) else {
                    continue;
                };
                let name_lc = raw_name.to_lowercase();
                let Ok(version_dir) = best_child_dir(&pkg_dir) else {
                    continue;
                };
                let Some(ver_str) = version_dir.file_name().and_then(|s| s.to_str()) else {
                    continue;
                };
                if let (Ok(n), Ok(v)) = (PackageName::new(&name_lc), Version::new(ver_str)) {
                    map.entry(n).or_insert(v);
                }
            }
        }
        SystemPackages(map)
    }
}

fn parse_pacman_desc(text: &str) -> Option<(String, String)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut name: Option<String> = None;
    let mut version: Option<String> = None;
    let mut i = 0;
    while i < lines.len() {
        match lines[i] {
            "%NAME%" if i + 1 < lines.len() => {
                name = Some(lines[i + 1].trim().to_lowercase());
                i += 2;
            }
            "%VERSION%" if i + 1 < lines.len() => {
                version = Some(lines[i + 1].trim().to_string());
                i += 2;
            }
            _ => i += 1,
        }
    }
    name.zip(version)
}

fn best_child_dir(pkg_dir: &Path) -> Result<PathBuf, ()> {
    let mut best: Option<(String, PathBuf)> = None;
    for entry in fs::read_dir(pkg_dir).map_err(|_| ())? {
        let entry = entry.map_err(|_| ())?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(ver) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        best = Some(match best {
            None => (ver.to_string(), path),
            Some((b, _)) if ver > b.as_str() => (ver.to_string(), path),
            Some(prev) => prev,
        });
    }
    best.map(|(_, p)| p).ok_or(())
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
    fn parse_dpkg_status_single_paragraph() {
        let contents = "Package: ripgrep\nVersion: 13.0\nStatus: install ok installed\n";
        let s = SystemPackages::parse_dpkg_status(contents);
        let name = PackageName::new("ripgrep").unwrap();
        assert_eq!(s.get(&name).map(|v| v.as_str().to_string()), Some("13.0".to_string()));
    }

    #[test]
    fn parse_dpkg_status_two_paragraphs() {
        let contents = "Package: a\nVersion: 1.0\nStatus: install ok installed\n\n\
                        Package: b\nVersion: 2.0\nStatus: install ok installed\n";
        let s = SystemPackages::parse_dpkg_status(contents);
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
    fn parse_dpkg_status_empty_returns_default() {
        let s = SystemPackages::parse_dpkg_status("");
        assert!(s.0.is_empty());
    }

    #[test]
    fn parse_dpkg_status_invalid_name_skipped() {
        let contents = "Package: libstdc++\nVersion: 1.0\nStatus: install ok installed\n\n\
                        Package: ripgrep\nVersion: 13.0\nStatus: install ok installed\n";
        let s = SystemPackages::parse_dpkg_status(contents);
        assert_eq!(
            s.get(&PackageName::new("ripgrep").unwrap()).map(|v| v.as_str().to_string()),
            Some("13.0".to_string())
        );
        assert_eq!(s.0.len(), 1);
    }

    #[test]
    fn parse_dpkg_status_only_installed_included() {
        let contents = "Package: old\nVersion: 1.0\nStatus: deinstall ok config-files\n\n\
                        Package: ripgrep\nVersion: 13.0\nStatus: install ok installed\n";
        let s = SystemPackages::parse_dpkg_status(contents);
        assert!(s.get(&PackageName::new("old").unwrap()).is_none());
        assert_eq!(
            s.get(&PackageName::new("ripgrep").unwrap()).map(|v| v.as_str().to_string()),
            Some("13.0".to_string())
        );
        assert_eq!(s.0.len(), 1);
    }

    #[test]
    fn parse_pacman_desc_sample() {
        let text = "%NAME%\nripgrep\n%VERSION%\n14.1.0-1\n";
        let (n, v) = parse_pacman_desc(text).unwrap();
        assert_eq!(n, "ripgrep");
        assert_eq!(v, "14.1.0-1");
    }

    #[test]
    fn parse_apk_installed_sample() {
        let contents = "C:Q\nP:busybox\nV:1.36.1-r15\n\nC:Q\nP:ripgrep\nV:13.0-r0\n";
        let s = SystemPackages::parse_apk_installed(contents);
        assert_eq!(
            s.get(&PackageName::new("ripgrep").unwrap()).map(|v| v.as_str().to_string()),
            Some("13.0-r0".to_string())
        );
    }

    #[test]
    fn merge_prefers_first_source() {
        let mut a = SystemPackages::from_map([("curl", "8.0")]);
        let b = SystemPackages::from_map([("curl", "7.0")]);
        a.merge_from(b);
        assert_eq!(
            a.get(&PackageName::new("curl").unwrap()).map(|v| v.as_str().to_string()),
            Some("8.0".to_string())
        );
    }
}
