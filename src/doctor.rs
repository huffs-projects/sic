//! sic doctor: check layout, lockfile vs installed, broken symlinks.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::prefix::check_layout;
use crate::storage::{InstalledDb, Lockfile};

/// Per-check result for JSON/TOML output.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorCheckResult {
    pub ok: bool,
    pub issues: Vec<String>,
}

/// Full doctor result: layout, lockfile vs installed, symlinks.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorResult {
    pub layout: DoctorCheckResult,
    pub lockfile: DoctorCheckResult,
    pub symlinks: DoctorCheckResult,
}

impl DoctorResult {
    pub fn overall_ok(&self) -> bool {
        self.layout.ok && self.lockfile.ok && self.symlinks.ok
    }
}

/// Runs layout check and returns (ok, issues).
pub fn run_layout_check(prefix: &Path) -> (bool, Vec<String>) {
    check_layout(prefix)
}

/// Resolves lockfile path: explicit path if present, else prefix/sic.lock if it is a file.
pub fn resolve_lockfile_path(prefix: &Path, lockfile_override: Option<&PathBuf>) -> Option<PathBuf> {
    if let Some(p) = lockfile_override {
        return Some(p.clone());
    }
    let lf = prefix.join("sic.lock");
    if lf.is_file() {
        Some(lf)
    } else {
        None
    }
}

/// Runs lockfile vs installed check. If lockfile_path is None, returns (true, []).
/// Otherwise loads Lockfile and InstalledDb and reports mismatches / not in lockfile.
pub fn run_lockfile_check(
    prefix: &Path,
    lockfile_path: Option<PathBuf>,
) -> (bool, Vec<String>) {
    let Some(path) = lockfile_path else {
        return (true, vec![]);
    };
    let lockfile = match Lockfile::load(&path) {
        Ok(Some(lf)) => lf,
        Ok(None) => return (true, vec![]),
        Err(e) => {
            return (false, vec![format!("lockfile error: {}", e)]);
        }
    };
    let installed = match InstalledDb::load(prefix) {
        Ok(db) => db,
        Err(e) => {
            return (
                false,
                vec![format!("failed to load installed.toml: {}", e)],
            );
        }
    };
    let mut issues = Vec::new();
    for entry in installed.list_all() {
        let locked = lockfile.packages_for_name(&entry.name);
        match locked.first() {
            None => {
                issues.push(format!(
                    "{}: not in lockfile",
                    entry.name.as_str(),
                ));
            }
            Some(p) => {
                if p.version != entry.version {
                    issues.push(format!(
                        "{}: version mismatch (installed {}, locked {})",
                        entry.name.as_str(),
                        entry.version.as_str(),
                        p.version.as_str(),
                    ));
                }
            }
        }
    }
    let ok = issues.is_empty();
    (ok, issues)
}

/// Runs broken symlinks check: package files under pkgs and symlinks under bin.
#[cfg(unix)]
pub fn run_symlinks_check(prefix: &Path) -> (bool, Vec<String>) {
    use std::fs;

    let mut issues = Vec::new();
    let installed = match InstalledDb::load(prefix) {
        Ok(db) => db,
        Err(_) => return (true, vec![]),
    };
    for entry in installed.list_all() {
        let pkg_root = prefix.join(&entry.install_path);
        if !pkg_root.is_dir() {
            issues.push(format!("package dir missing: {}", entry.install_path));
            continue;
        }
        for file in &entry.files {
            let path = pkg_root.join(file);
            let meta = match fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(_) => {
                    issues.push(format!("missing: {}/{}", entry.install_path, file));
                    continue;
                }
            };
            if meta.is_symlink() {
                let target = match fs::read_link(&path) {
                    Ok(t) => t,
                    Err(_) => {
                        issues.push(format!("broken symlink: {}/{} (read failed)", entry.install_path, file));
                        continue;
                    }
                };
                let resolved = if target.is_absolute() {
                    target.clone()
                } else {
                    path.parent().unwrap_or(&pkg_root).join(&target)
                };
                if !resolved.exists() {
                    issues.push(format!(
                        "broken symlink: {}/{} -> {}",
                        entry.install_path,
                        file,
                        target.display()
                    ));
                }
            }
        }
    }
    let bin_dir = prefix.join("bin");
    if bin_dir.is_dir() {
        let entries = match fs::read_dir(&bin_dir) {
            Ok(e) => e,
            Err(_) => return (issues.is_empty(), issues),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let meta = match fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if !meta.is_symlink() {
                continue;
            }
            let target = match fs::read_link(&path) {
                Ok(t) => t,
                Err(_) => {
                    issues.push(format!("broken bin symlink: bin/{} (read failed)", path.file_name().unwrap_or_default().to_string_lossy()));
                    continue;
                }
            };
            let resolved = if target.is_absolute() {
                target.clone()
            } else {
                path.parent().unwrap_or(prefix).join(&target)
            };
            if !resolved.exists() {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                issues.push(format!(
                    "broken bin symlink: bin/{} -> {}",
                    name,
                    target.display()
                ));
            }
        }
    }
    let ok = issues.is_empty();
    (ok, issues)
}

#[cfg(not(unix))]
pub fn run_symlinks_check(prefix: &Path) -> (bool, Vec<String>) {
    let _ = prefix;
    (true, vec![])
}

/// Runs all three checks and returns a DoctorResult.
pub fn run_doctor_checks(
    prefix: &Path,
    lockfile_path: Option<PathBuf>,
) -> DoctorResult {
    let (layout_ok, layout_issues) = run_layout_check(prefix);
    let (lockfile_ok, lockfile_issues) = run_lockfile_check(prefix, lockfile_path);
    let (symlinks_ok, symlinks_issues) = run_symlinks_check(prefix);
    DoctorResult {
        layout: DoctorCheckResult {
            ok: layout_ok,
            issues: layout_issues,
        },
        lockfile: DoctorCheckResult {
            ok: lockfile_ok,
            issues: lockfile_issues,
        },
        symlinks: DoctorCheckResult {
            ok: symlinks_ok,
            issues: symlinks_issues,
        },
    }
}
