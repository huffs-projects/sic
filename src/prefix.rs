//! Prefix resolution and filesystem layout.

use std::io;
use std::path::{Path, PathBuf};

/// Directory names under prefix (design section 4).
const DIRS: &[&str] = &[
    "pkgs",
    "bin",
    "share",
    "share/man",
    "var",
    "var/cache",
    "var/backups",
    "var/transactions",
    "tmp",
    "backups",
];

/// Resolves the sic root prefix: `SIC_ROOT` env if set, else `~/.local/sic`.
/// Returns an absolute path when possible (expands `~` to user home; when HOME
/// is unset, uses current directory if available).
pub fn resolve_root() -> PathBuf {
    if let Ok(root) = std::env::var("SIC_ROOT") {
        let p = PathBuf::from(&root);
        if p.is_absolute() {
            return p;
        }
        if let Ok(cwd) = std::env::current_dir() {
            return cwd.join(&p);
        }
        return p;
    }
    let base = match home_dir() {
        Some(h) => h,
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };
    base.join(".local").join("sic")
}

/// Returns user home directory (e.g. from `HOME` env).
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Ensures the standard layout exists under `prefix`: pkgs/, bin/, var/, var/cache/,
/// var/backups/, var/transactions/, tmp/, backups/. Idempotent; safe to call multiple times.
pub fn ensure_layout(prefix: &Path) -> io::Result<()> {
    for dir in DIRS {
        let path = prefix.join(dir);
        std::fs::create_dir_all(path)?;
    }
    Ok(())
}

/// Checks that the standard layout exists under `prefix`. Returns (true, []) if all required
/// directories exist and are directories; otherwise (false, issues) with one string per issue.
pub fn check_layout(prefix: &Path) -> (bool, Vec<String>) {
    let mut issues = Vec::new();
    for dir in DIRS {
        let path = prefix.join(dir);
        if !path.exists() {
            issues.push(format!("missing: {}", dir));
        } else if !path.is_dir() {
            issues.push(format!("not a directory: {}", dir));
        }
    }
    let ok = issues.is_empty();
    (ok, issues)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_root_uses_env_when_set() {
        std::env::set_var("SIC_ROOT", "/custom/sic");
        let root = resolve_root();
        std::env::remove_var("SIC_ROOT");
        assert!(root.to_string_lossy().contains("custom"));
        assert!(root.is_absolute() || root.to_string_lossy().starts_with("/"));
    }

    #[test]
    fn ensure_layout_creates_all_dirs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let prefix = tmp.path();
        ensure_layout(prefix).unwrap();
        for dir in DIRS {
            let p = prefix.join(dir);
            assert!(p.is_dir(), "expected dir: {:?}", p);
        }
    }

    #[test]
    fn ensure_layout_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let prefix = tmp.path();
        ensure_layout(prefix).unwrap();
        ensure_layout(prefix).unwrap();
        for dir in DIRS {
            assert!(prefix.join(dir).is_dir());
        }
    }

    #[test]
    fn check_layout_empty_prefix_reports_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let prefix = tmp.path();
        let (ok, issues) = check_layout(prefix);
        assert!(!ok);
        assert!(!issues.is_empty());
        assert!(issues.iter().any(|s| s.starts_with("missing:")));
    }

    #[test]
    fn check_layout_after_ensure_layout_ok() {
        let tmp = tempfile::TempDir::new().unwrap();
        let prefix = tmp.path();
        ensure_layout(prefix).unwrap();
        let (ok, issues) = check_layout(prefix);
        assert!(ok, "issues: {:?}", issues);
        assert!(issues.is_empty());
    }

    #[test]
    fn check_layout_file_instead_of_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let prefix = tmp.path();
        ensure_layout(prefix).unwrap();
        let bin_path = prefix.join("bin");
        std::fs::remove_dir(&bin_path).unwrap();
        std::fs::write(&bin_path, "not a dir").unwrap();
        let (ok, issues) = check_layout(prefix);
        assert!(!ok);
        assert!(issues.iter().any(|s| s.contains("not a directory") && s.contains("bin")));
    }
}
