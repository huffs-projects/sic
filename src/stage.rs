//! Stage plan artifacts into the transaction staging directory.
//!
//! Fetches each package source to cache, extracts the tarball, and copies
//! the package-listed files (with glob expansion) into prefix/tmp/<tx-id>/<pkg>/.

use std::fs;
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use tar::Archive;
use uuid::Uuid;

use crate::fetch::{fetch_to_cache, format_bytes, FetchError};
use crate::resolver::{Plan, PlanAction, PlanStep};
use crate::transaction::staging_path;

/// Errors from staging.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StageError {
    Fetch(FetchError),
    Extract(String),
    Copy(String),
    Io(String),
    /// Invalid path or glob (e.g. path traversal).
    Path(String),
}

impl std::fmt::Display for StageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StageError::Fetch(e) => write!(f, "stage fetch: {}", e),
            StageError::Extract(msg) => write!(f, "stage extract: {}", msg),
            StageError::Copy(msg) => write!(f, "stage copy: {}", msg),
            StageError::Io(msg) => write!(f, "stage io: {}", msg),
            StageError::Path(msg) => write!(f, "stage path: {}", msg),
        }
    }
}

impl std::error::Error for StageError {}

impl From<FetchError> for StageError {
    fn from(e: FetchError) -> Self {
        StageError::Fetch(e)
    }
}

/// Package directory name for staging (e.g. "foo-1.0"). Must match transaction convention.
fn pkg_dir_name(step: &PlanStep) -> Result<String, StageError> {
    let s = format!("{}-{}", step.name.as_str(), step.version.as_str());
    if s.contains("..") || s.contains('/') || s.contains('\\') {
        return Err(StageError::Path(
            "package name or version must not contain path separators or ..".to_string(),
        ));
    }
    Ok(s)
}

/// Extracts a .tar.gz file to a directory. Returns the path to the unpacked content.
/// If the tarball has a single top-level directory, returns that path; otherwise returns dest.
fn extract_tarball(archive_path: &Path, dest: &Path) -> Result<PathBuf, StageError> {
    let file = fs::File::open(archive_path).map_err(|e| StageError::Io(e.to_string()))?;
    let dec = GzDecoder::new(BufReader::new(file));
    let mut archive = Archive::new(dec);
    archive
        .unpack(dest)
        .map_err(|e| StageError::Extract(e.to_string()))?;

    let entries: Vec<_> = fs::read_dir(dest).map_err(|e| StageError::Io(e.to_string()))?.collect();
    if entries.len() == 1 {
        if let Some(Ok(entry)) = entries.into_iter().next() {
            let path = entry.path();
            if let Ok(meta) = fs::symlink_metadata(&path) {
                if meta.is_dir() && !meta.is_symlink() {
                    return Ok(path);
                }
            }
        }
    }
    Ok(dest.to_path_buf())
}

/// Resolves a single file pattern against the extract root: either a literal path or a glob (*).
/// Returns relative paths (with forward slashes) that exist under extract_root.
fn resolve_pattern(extract_root: &Path, pattern: &str) -> Result<Vec<PathBuf>, StageError> {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return Ok(vec![]);
    }
    let pattern_normalized = pattern.replace('\\', "/");
    if pattern_normalized.contains("..") {
        return Err(StageError::Path("pattern must not contain ..".to_string()));
    }
    if pattern_normalized.contains('*') {
        let prefix = pattern_normalized
            .strip_suffix("/*")
            .unwrap_or(&pattern_normalized);
        let prefix = prefix.strip_suffix('*').unwrap_or(prefix);
        let prefix_path = Path::new(prefix);
        let mut out = vec![];
        walk_under(extract_root, extract_root, prefix_path, &mut out, 0)?;
        return Ok(out);
    }
    // Single path (no glob)
    let full = extract_root.join(&pattern_normalized);
    if full.exists() && full.is_file() {
        let rel = full
            .strip_prefix(extract_root)
            .map_err(|_| StageError::Path("pattern path outside extract".to_string()))?;
        return Ok(vec![rel.to_path_buf()]);
    }
    if full.is_dir() {
        let mut out = vec![];
        for entry in fs::read_dir(&full).map_err(|e| StageError::Io(e.to_string()))? {
            let entry = entry.map_err(|e| StageError::Io(e.to_string()))?;
            let path = entry.path();
            if path.is_file() {
                if let Ok(rel) = path.strip_prefix(extract_root) {
                    out.push(rel.to_path_buf());
                }
            }
        }
        return Ok(out);
    }
    Ok(vec![])
}

/// Maximum depth when walking extract tree (prevents stack overflow from deep nesting or symlink cycles).
const WALK_UNDER_MAX_DEPTH: u32 = 256;

fn walk_under(
    extract_root: &Path,
    current: &Path,
    prefix: &Path,
    out: &mut Vec<PathBuf>,
    depth: u32,
) -> Result<(), StageError> {
    if depth >= WALK_UNDER_MAX_DEPTH {
        return Err(StageError::Path(
            "extract tree too deep when resolving file pattern".to_string(),
        ));
    }
    for entry in fs::read_dir(current).map_err(|e| StageError::Io(e.to_string()))? {
        let entry = entry.map_err(|e| StageError::Io(e.to_string()))?;
        let path = entry.path();
        let rel = path
            .strip_prefix(extract_root)
            .map_err(|_| StageError::Path("path outside extract".to_string()))?;
        if path.is_dir() {
            walk_under(extract_root, &path, prefix, out, depth + 1)?;
        } else {
            let rel_normalized = rel.to_string_lossy().replace('\\', "/");
            let prefix_str = prefix.to_string_lossy().replace('\\', "/");
            let prefix_with_slash = format!("{}/", prefix_str);
            let matches = prefix_str.is_empty()
                || rel_normalized == prefix_str
                || rel_normalized.starts_with(&prefix_with_slash);
            if matches {
                out.push(rel.to_path_buf());
            }
        }
    }
    Ok(())
}

/// Copies a file (or symlink) from extract_root/rel to staging_pkg/rel, creating parent dirs.
/// Symlinks are recreated as symlinks; absolute targets are rejected to avoid copying from
/// outside the extract directory.
fn copy_relative_file(
    extract_root: &Path,
    staging_pkg: &Path,
    rel: &Path,
) -> Result<(), StageError> {
    let src = extract_root.join(rel);
    let meta = match fs::symlink_metadata(&src) {
        Ok(m) => m,
        Err(_) => return Ok(()),
    };
    let rel_str = rel.to_string_lossy().replace('\\', "/");
    if rel_str.contains("..") {
        return Err(StageError::Path("path traversal not allowed".to_string()));
    }
    let dest = staging_pkg.join(rel);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| StageError::Io(e.to_string()))?;
    }
    if meta.is_symlink() {
        let target = fs::read_link(&src).map_err(|e| StageError::Io(e.to_string()))?;
        if target.is_absolute() {
            return Err(StageError::Path(
                "symlink with absolute target not allowed".to_string(),
            ));
        }
        if target.components().any(|c| c == std::path::Component::ParentDir) {
            return Err(StageError::Path(
                "symlink target must not contain ..".to_string(),
            ));
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &dest).map_err(|e| StageError::Copy(e.to_string()))?;
        #[cfg(not(unix))]
        {
            let _ = (target, dest);
            return Err(StageError::Path(
                "symlinks in package artifacts are not supported on this platform".to_string(),
            ));
        }
    } else if meta.is_file() {
        fs::copy(&src, &dest).map_err(|e| StageError::Copy(e.to_string()))?;
    }
    Ok(())
}

/// Stages the plan into the transaction staging directory: for each step, fetches the
/// artifact to cache, extracts it, and copies the listed files (with glob expansion)
/// into prefix/tmp/<tx-id>/<name-version>/.
/// When `show_progress` is true (e.g. stderr is a TTY), download progress is shown.
pub fn stage_plan(
    prefix: &Path,
    tx_id: Uuid,
    plan: &Plan,
    verbose: bool,
    show_progress: bool,
) -> Result<(), StageError> {
    let staging = staging_path(prefix, tx_id);
    fs::create_dir_all(&staging).map_err(|e| StageError::Io(e.to_string()))?;

    let install_steps: Vec<_> = plan
        .steps
        .iter()
        .filter(|s| s.action != PlanAction::Remove)
        .collect();
    let total_packages = install_steps.len();

    for (idx, step) in install_steps.into_iter().enumerate() {
        let step_num = idx + 1;
        if verbose || (show_progress && total_packages > 1) {
            eprintln!(
                "stage: {}/{} packages: {}-{}",
                step_num,
                total_packages,
                step.name.as_str(),
                step.version.as_str()
            );
        }
        let pkg_dir = pkg_dir_name(step)?;
        let staging_pkg = staging.join(&pkg_dir);
        fs::create_dir_all(&staging_pkg).map_err(|e| StageError::Io(e.to_string()))?;

        let progress_cb: Option<crate::fetch::ProgressCb> = if show_progress {
            Some(Box::new(move |downloaded: u64, total: Option<u64>| {
                let line = if let Some(t) = total {
                    let pct = (downloaded * 100).checked_div(t).unwrap_or(0);
                    format!(
                        "fetch: {} / {} ({}%)\r",
                        format_bytes(downloaded),
                        format_bytes(t),
                        pct
                    )
                } else {
                    format!("fetch: {}\r", format_bytes(downloaded))
                };
                let _ = std::io::stderr().write_all(line.as_bytes());
            }))
        } else {
            None
        };

        let artifact_path = fetch_to_cache(prefix, &step.source, verbose, progress_cb)?;
        let extract_temp = tempfile::TempDir::new().map_err(|e| StageError::Io(e.to_string()))?;
        let extract_root = extract_tarball(&artifact_path, extract_temp.path())?;

        for file_pattern in &step.files {
            let paths = resolve_pattern(&extract_root, file_pattern)?;
            for rel in paths {
                copy_relative_file(&extract_root, &staging_pkg, &rel)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;
    use crate::package_name::PackageName;
    use crate::resolver::{PlanAction, PlanStep};
    use crate::source::{Source, SourceHash};
    use crate::version::Version;

    /// Build a minimal .tar.gz with one top-level dir and given relative paths (file content = path string).
    fn build_tarball(paths: &[&str]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let enc = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::default());
            let mut tar_builder = tar::Builder::new(enc);
            for path in paths {
                let data = path.as_bytes();
                let mut header = tar::Header::new_gnu();
                header.set_size(data.len() as u64);
                header.set_cksum();
                tar_builder
                    .append_data(&mut header, path, io::Cursor::new(data))
                    .unwrap();
            }
            tar_builder.finish().unwrap();
        }
        buf
    }

    #[test]
    fn stage_plan_extracts_and_copies_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let prefix = tmp.path();
        crate::prefix::ensure_layout(prefix).unwrap();

        let content = build_tarball(&["pkg-1.0/bin/foo", "pkg-1.0/share/bar.txt"]);
        let hash_hex = {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(&content);
            format!("{:x}", h.finalize())
        };
        let source = Source {
            type_name: "tarball".to_string(),
            url: "https://example.com/pkg.tar.gz".to_string(),
            hash: SourceHash::parse(&format!("sha256:{}", hash_hex)).unwrap(),
        };
        let cache_path = crate::storage::cache_path(prefix, &source);
        fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
        fs::write(&cache_path, &content).unwrap();

        let step = PlanStep {
            name: PackageName::new("pkg").unwrap(),
            version: Version::new("1.0").unwrap(),
            revision: 0,
            source: source.clone(),
            files: vec!["bin/foo".to_string(), "share/bar.txt".to_string()],
            commands: vec![],
            action: PlanAction::Install,
        };
        let plan = Plan {
            steps: vec![step],
            satisfied_by_system: vec![],
        };
        let tx_id = Uuid::new_v4();
        stage_plan(prefix, tx_id, &plan, false, false).unwrap();

        let staging = staging_path(prefix, tx_id);
        let pkg_staging = staging.join("pkg-1.0");
        assert!(pkg_staging.join("bin/foo").is_file());
        assert!(pkg_staging.join("share/bar.txt").is_file());
        assert_eq!(
            fs::read_to_string(pkg_staging.join("bin/foo")).unwrap(),
            "pkg-1.0/bin/foo"
        );
        assert_eq!(
            fs::read_to_string(pkg_staging.join("share/bar.txt")).unwrap(),
            "pkg-1.0/share/bar.txt"
        );
    }

    #[test]
    fn stage_plan_glob_expands() {
        let tmp = tempfile::TempDir::new().unwrap();
        let prefix = tmp.path();
        crate::prefix::ensure_layout(prefix).unwrap();

        let content = build_tarball(&[
            "pkg-1.0/share/helix/a",
            "pkg-1.0/share/helix/b",
        ]);
        let hash_hex = {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(&content);
            format!("{:x}", h.finalize())
        };
        let source = Source {
            type_name: "tarball".to_string(),
            url: "https://example.com/pkg.tar.gz".to_string(),
            hash: SourceHash::parse(&format!("sha256:{}", hash_hex)).unwrap(),
        };
        let cache_path = crate::storage::cache_path(prefix, &source);
        fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
        fs::write(&cache_path, &content).unwrap();

        let step = PlanStep {
            name: PackageName::new("pkg").unwrap(),
            version: Version::new("1.0").unwrap(),
            revision: 0,
            source,
            files: vec!["share/helix/*".to_string()],
            commands: vec![],
            action: PlanAction::Install,
        };
        let plan = Plan {
            steps: vec![step],
            satisfied_by_system: vec![],
        };
        let tx_id = Uuid::new_v4();
        stage_plan(prefix, tx_id, &plan, false, false).unwrap();

        let staging = staging_path(prefix, tx_id);
        let pkg_staging = staging.join("pkg-1.0");
        assert!(pkg_staging.join("share/helix/a").is_file());
        assert!(pkg_staging.join("share/helix/b").is_file());
    }

    #[test]
    fn glob_does_not_match_similar_prefix() {
        let tmp = tempfile::TempDir::new().unwrap();
        let prefix = tmp.path();
        crate::prefix::ensure_layout(prefix).unwrap();

        let content = build_tarball(&[
            "pkg-1.0/share/helix/a",
            "pkg-1.0/share/helix-something/x",
        ]);
        let hash_hex = {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(&content);
            format!("{:x}", h.finalize())
        };
        let source = Source {
            type_name: "tarball".to_string(),
            url: "https://example.com/pkg.tar.gz".to_string(),
            hash: SourceHash::parse(&format!("sha256:{}", hash_hex)).unwrap(),
        };
        let cache_path = crate::storage::cache_path(prefix, &source);
        fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
        fs::write(&cache_path, &content).unwrap();

        let step = PlanStep {
            name: PackageName::new("pkg").unwrap(),
            version: Version::new("1.0").unwrap(),
            revision: 0,
            source,
            files: vec!["share/helix/*".to_string()],
            commands: vec![],
            action: PlanAction::Install,
        };
        let plan = Plan {
            steps: vec![step],
            satisfied_by_system: vec![],
        };
        let tx_id = Uuid::new_v4();
        stage_plan(prefix, tx_id, &plan, false, false).unwrap();

        let staging = staging_path(prefix, tx_id);
        let pkg_staging = staging.join("pkg-1.0");
        assert!(pkg_staging.join("share/helix/a").is_file());
        assert!(
            !pkg_staging.join("share/helix-something/x").exists(),
            "share/helix/* must not match share/helix-something/"
        );
    }

    #[test]
    #[cfg(unix)]
    fn symlink_relative_recreated_absolute_rejected() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::TempDir::new().unwrap();
        let extract_root = tmp.path().join("extract");
        let staging_pkg = tmp.path().join("staging");
        fs::create_dir_all(&extract_root).unwrap();
        fs::create_dir_all(&staging_pkg).unwrap();
        fs::write(extract_root.join("real"), b"content").unwrap();
        symlink("real", extract_root.join("link_rel")).unwrap();
        symlink("/etc/passwd", extract_root.join("link_abs")).unwrap();

        copy_relative_file(&extract_root, &staging_pkg, Path::new("link_rel")).unwrap();
        assert!(staging_pkg.join("link_rel").is_symlink());
        assert_eq!(
            fs::read_link(staging_pkg.join("link_rel")).unwrap(),
            Path::new("real")
        );

        let r = copy_relative_file(&extract_root, &staging_pkg, Path::new("link_abs"));
        assert!(matches!(r, Err(StageError::Path(_))));

        symlink("share/../etc/passwd", extract_root.join("link_traversal")).unwrap();
        let r = copy_relative_file(&extract_root, &staging_pkg, Path::new("link_traversal"));
        assert!(matches!(r, Err(StageError::Path(_))));
    }
}
