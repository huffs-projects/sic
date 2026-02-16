//! Transaction layer: staging dir, per-user lock, commit, rollback, backups, logs.
//!
//! Single-machine, single-user; lock is process-scoped, not cross-machine.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use fs2::FileExt;
use uuid::Uuid;

use crate::resolver::{Plan, PlanStep};
use crate::storage::{InstalledDb, InstalledEntry};
use crate::prefix;

/// Transaction type: install, upgrade, or remove.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransactionType {
    Install,
    Upgrade,
    Remove,
}

/// Transaction state: only Pending -> Committed or Pending -> RolledBack.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransactionState {
    Pending,
    Committed,
    RolledBack,
}

/// A transaction: id, type, plan, and state.
#[derive(Clone, Debug)]
pub struct Transaction {
    pub id: Uuid,
    pub tx_type: TransactionType,
    pub plan: Plan,
    pub state: TransactionState,
}

/// Lock file path under prefix.
pub const LOCK_FILENAME: &str = "var/sic.lock";

/// Acquires an exclusive lock on prefix/var/sic.lock. Block until acquired.
/// The returned guard holds the file handle; drop to release.
pub fn acquire_lock(prefix: &Path) -> Result<LockGuard, TransactionError> {
    prefix::ensure_layout(prefix).map_err(TransactionError::io_err)?;
    let lock_path = prefix.join(LOCK_FILENAME);
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| TransactionError::lock_err(e.to_string()))?;
    file.lock_exclusive()
        .map_err(|e| TransactionError::lock_err(e.to_string()))?;
    Ok(LockGuard { file: Some(file) })
}

/// Guard that holds the lock; drop to release.
pub struct LockGuard {
    file: Option<fs::File>,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        // Dropping the file handle releases the lock on Unix/Windows.
        drop(self.file.take());
    }
}

/// Returns the staging directory path for a transaction: prefix/tmp/<tx-id>/.
pub fn staging_path(prefix: &Path, tx_id: Uuid) -> PathBuf {
    prefix.join("tmp").join(tx_id.to_string())
}

/// Returns the backup directory for replaced files: prefix/backups/<tx-id>/.
pub fn backup_dir(prefix: &Path, tx_id: Uuid) -> PathBuf {
    prefix.join("backups").join(tx_id.to_string())
}

/// Returns the path to the installed.toml backup for a transaction.
pub fn installed_backup_path(prefix: &Path, tx_id: Uuid) -> PathBuf {
    prefix
        .join("var")
        .join("backups")
        .join(format!("installed.{}.toml", tx_id))
}

/// Returns the transaction log path: prefix/var/transactions/<uuid>.log.
pub fn transaction_log_path(prefix: &Path, tx_id: Uuid) -> PathBuf {
    prefix
        .join("var")
        .join("transactions")
        .join(format!("{}.log", tx_id))
}

#[derive(Clone, Debug)]
pub struct TransactionError(String);

impl std::fmt::Display for TransactionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for TransactionError {}

impl TransactionError {
    fn io_err(e: io::Error) -> Self {
        TransactionError(format!("transaction io: {}", e))
    }
    fn lock_err(msg: String) -> Self {
        TransactionError(format!("lock: {}", msg))
    }
    /// Creates a transaction error from an arbitrary message (e.g. from stage/layout).
    pub fn msg(s: impl Into<String>) -> Self {
        TransactionError(s.into())
    }
}

impl Transaction {
    /// Creates a new transaction with Pending state, creates the staging directory and log file.
    pub fn new(tx_type: TransactionType, plan: Plan, prefix: &Path) -> Result<Self, TransactionError> {
        prefix::ensure_layout(prefix).map_err(TransactionError::io_err)?;
        let id = Uuid::new_v4();
        let staging = staging_path(prefix, id);
        fs::create_dir_all(&staging).map_err(TransactionError::io_err)?;
        let log_path = transaction_log_path(prefix, id);
        if let Some(parent) = log_path.parent() {
            fs::create_dir_all(parent).map_err(TransactionError::io_err)?;
        }
        let tx = Transaction {
            id,
            tx_type,
            plan,
            state: TransactionState::Pending,
        };
        tx.log_state(prefix, "created")?;
        Ok(tx)
    }

    fn log_state(&self, prefix: &Path, state_label: &str) -> Result<(), TransactionError> {
        let log_path = transaction_log_path(prefix, self.id);
        let timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let line = format!("{} tx={} state={}\n", timestamp, self.id, state_label);
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(TransactionError::io_err)?;
        f.write_all(line.as_bytes())
            .map_err(|e| TransactionError(format!("log write: {}", e)))?;
        Ok(())
    }

    /// Commits the transaction: acquire lock, backup installed.toml, apply plan (copy from staging,
    /// backup overwrites), write new installed.toml, remove staging, log, release lock.
    /// On any error after applying steps, runs rollback before returning.
    pub fn commit(&mut self, prefix: &Path) -> Result<(), TransactionError> {
        if self.state != TransactionState::Pending {
            return Err(TransactionError(
                "commit only allowed when state is Pending".to_string(),
            ));
        }
        let _guard = acquire_lock(prefix)?;
        let staging = staging_path(prefix, self.id);
        let backup = backup_dir(prefix, self.id);

        let current = InstalledDb::load(prefix).map_err(|e| {
            TransactionError(format!("load installed.toml: {}", e))
        })?;

        if let Err(e) = self.do_commit_steps(prefix, &staging, &backup, &current, &self.plan) {
            let _ = self.rollback(prefix, Some("commit failure"));
            return Err(e);
        }

        remove_dir_all_if_exists(&staging)?;
        self.state = TransactionState::Committed;
        self.log_state(prefix, "committed")?;
        Ok(())
    }

    fn do_commit_steps(
        &self,
        prefix: &Path,
        staging: &Path,
        backup: &Path,
        current: &InstalledDb,
        plan: &Plan,
    ) -> Result<(), TransactionError> {
        for step in &plan.steps {
            apply_step(prefix, staging, backup, step, current)?;
        }

        let installed_path = prefix.join("var").join("installed.toml");
        let backup_installed = installed_backup_path(prefix, self.id);
        if installed_path.exists() {
            fs::copy(&installed_path, &backup_installed).map_err(TransactionError::io_err)?;
        } else {
            if let Some(p) = backup_installed.parent() {
                fs::create_dir_all(p).map_err(TransactionError::io_err)?;
            }
            // Valid empty installed.toml so rollback can restore and load it.
            fs::write(&backup_installed, "[[packages]]\n")
                .map_err(TransactionError::io_err)?;
        }

        let new_db = compute_new_installed_db(prefix, current, plan, staging)?;
        new_db.write(prefix).map_err(|e| {
            TransactionError(format!("write installed.toml: {}", e))
        })?;
        Ok(())
    }

    /// Rollback: delete newly installed files, restore from backups, restore installed.toml,
    /// remove staging and backup dirs, log.
    pub fn rollback(&mut self, prefix: &Path, reason: Option<&str>) -> Result<(), TransactionError> {
        let staging = staging_path(prefix, self.id);
        let backup = backup_dir(prefix, self.id);
        let backup_installed = installed_backup_path(prefix, self.id);
        let installed_path = prefix.join("var").join("installed.toml");

        for step in &self.plan.steps {
            let dir_name = pkg_dir_name(step)?;
            let install_path = prefix.join("pkgs").join(&dir_name);
            if install_path.exists() {
                remove_dir_all_if_exists(&install_path)?;
            }
        }

        if backup_installed.exists() {
            if let Some(parent) = installed_path.parent() {
                fs::create_dir_all(parent).map_err(TransactionError::io_err)?;
            }
            fs::copy(&backup_installed, &installed_path).map_err(TransactionError::io_err)?;
        }

        restore_backup_dir(prefix, &backup)?;
        remove_dir_all_if_exists(&backup)?;
        remove_dir_all_if_exists(&staging)?;

        self.state = TransactionState::RolledBack;
        let label = reason
            .map(|r| format!("rolled_back reason={}", r))
            .unwrap_or_else(|| "rolled_back".to_string());
        self.log_state(prefix, &label)?;
        Ok(())
    }
}

/// Builds the package directory name (e.g. "foo-1.0"). Errors if it would allow path traversal.
fn pkg_dir_name(step: &PlanStep) -> Result<String, TransactionError> {
    let s = format!("{}-{}", step.name.as_str(), step.version.as_str());
    if s.contains("..") || s.contains('/') || s.contains('\\') {
        return Err(TransactionError(
            "package name or version must not contain path separators or ..".to_string(),
        ));
    }
    Ok(s)
}

/// Returns paths under prefix/bin/ that are symlinks pointing under prefix/install_path (e.g. "pkgs/foo-1.0").
fn bin_symlinks_pointing_to(prefix: &Path, install_path: &str) -> Result<Vec<PathBuf>, TransactionError> {
    let bin_dir = prefix.join("bin");
    if !bin_dir.is_dir() {
        return Ok(vec![]);
    }
    let pkg_abs = prefix.join(install_path);
    let mut out = Vec::new();
    for entry in fs::read_dir(&bin_dir).map_err(TransactionError::io_err)? {
        let entry = entry.map_err(TransactionError::io_err)?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path).map_err(TransactionError::io_err)?;
        if !meta.is_symlink() {
            continue;
        }
        let target = fs::read_link(&path).map_err(TransactionError::io_err)?;
        let resolved = if target.is_absolute() {
            target
        } else {
            path.parent().unwrap_or(prefix).join(&target)
        };
        if resolved.starts_with(&pkg_abs) {
            out.push(path);
        }
    }
    Ok(out)
}

/// Backs up pkgs/<pkg_dir_name> and bin symlinks pointing to it; then removes bin symlinks and pkg dir.
fn backup_and_remove_pkg(
    prefix: &Path,
    backup: &Path,
    pkg_dir_name: &str,
    install_path: &str,
) -> Result<(), TransactionError> {
    let pkg_abs = prefix.join("pkgs").join(pkg_dir_name);
    if pkg_abs.exists() {
        let backup_pkg = backup.join("pkgs").join(pkg_dir_name);
        if let Some(p) = backup_pkg.parent() {
            fs::create_dir_all(p).map_err(TransactionError::io_err)?;
        }
        copy_dir_all(&pkg_abs, &backup_pkg).map_err(TransactionError::io_err)?;
    }
    let bin_links = bin_symlinks_pointing_to(prefix, install_path)?;
    let backup_bin = backup.join("bin");
    for link_path in &bin_links {
        if let Some(name) = link_path.file_name() {
            let backup_link = backup_bin.join(name);
            if let Some(p) = backup_link.parent() {
                fs::create_dir_all(p).map_err(TransactionError::io_err)?;
            }
            #[cfg(unix)]
            {
                let target = fs::read_link(link_path).map_err(TransactionError::io_err)?;
                std::os::unix::fs::symlink(&target, &backup_link)
                    .map_err(|e| TransactionError(format!("backup symlink: {}", e)))?;
            }
            #[cfg(not(unix))]
            {
                fs::copy(link_path, &backup_link).map_err(TransactionError::io_err)?;
            }
        }
        remove_dir_all_if_exists(link_path)?;
    }
    remove_dir_all_if_exists(&pkg_abs)?;
    Ok(())
}

fn copy_dir_all(src: &Path, dest: &Path) -> io::Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let dest_path = dest.join(&name);
        let meta = fs::symlink_metadata(&path)?;
        if meta.is_dir() && !meta.is_symlink() {
            copy_dir_all(&path, &dest_path)?;
        } else if meta.is_symlink() {
            #[cfg(unix)]
            {
                let target = fs::read_link(&path)?;
                std::os::unix::fs::symlink(&target, &dest_path)?;
            }
            #[cfg(not(unix))]
            {
                fs::copy(&path, &dest_path)?;
            }
        } else {
            fs::copy(&path, &dest_path)?;
        }
    }
    Ok(())
}

fn apply_step(
    prefix: &Path,
    staging: &Path,
    backup: &Path,
    step: &PlanStep,
    current: &InstalledDb,
) -> Result<(), TransactionError> {
    use crate::resolver::PlanAction;
    let pkg_dir_name = pkg_dir_name(step)?;
    let install_path = format!("pkgs/{}", pkg_dir_name);

    match step.action {
        PlanAction::Remove => {
            backup_and_remove_pkg(prefix, backup, &pkg_dir_name, &install_path)?;
            return Ok(());
        }
        PlanAction::Upgrade => {
            if let Some(old_entry) = current.get_by_name(&step.name) {
                let old_dir = format!("{}-{}", step.name.as_str(), old_entry.version.as_str());
                backup_and_remove_pkg(
                    prefix,
                    backup,
                    &old_dir,
                    &old_entry.install_path,
                )?;
            }
        }
        PlanAction::Install => {}
    }

    let staging_pkg = staging.join(&pkg_dir_name);
    if !staging_pkg.exists() {
        return Ok(());
    }
    if !staging_pkg.is_dir() {
        return Err(TransactionError(
            "staging package path must be a directory".to_string(),
        ));
    }
    let dest_pkg = prefix.join("pkgs").join(&pkg_dir_name);
    fs::create_dir_all(&dest_pkg).map_err(TransactionError::io_err)?;
    copy_tree_with_backup_at_depth(&staging_pkg, &dest_pkg, prefix, backup, 0)?;
    Ok(())
}

/// Maximum tree depth when copying (prevents symlink loops / stack overflow).
const COPY_TREE_MAX_DEPTH: u32 = 256;

fn copy_tree_with_backup_at_depth(
    src: &Path,
    dest: &Path,
    _prefix: &Path,
    backup_root: &Path,
    depth: u32,
) -> Result<(), TransactionError> {
    if depth >= COPY_TREE_MAX_DEPTH {
        return Err(TransactionError(
            "staging tree too deep when copying package files".to_string(),
        ));
    }
    for entry in fs::read_dir(src).map_err(TransactionError::io_err)? {
        let entry = entry.map_err(TransactionError::io_err)?;
        let path = entry.path();
        let name = entry.file_name();
        let dest_path = dest.join(&name);
        let meta = fs::symlink_metadata(&path).map_err(TransactionError::io_err)?;
        if meta.is_dir() {
            fs::create_dir_all(&dest_path).map_err(TransactionError::io_err)?;
            copy_tree_with_backup_at_depth(&path, &dest_path, _prefix, backup_root, depth + 1)?;
        } else if meta.is_symlink() {
            #[cfg(unix)]
            {
                let target = fs::read_link(&path).map_err(TransactionError::io_err)?;
                if dest_path.exists() {
                    let rel = dest_path.strip_prefix(_prefix).map_err(|_| {
                        TransactionError("dest path must be under prefix".to_string())
                    })?;
                    let backup_path = backup_root.join(rel);
                    if let Some(p) = backup_path.parent() {
                        fs::create_dir_all(p).map_err(TransactionError::io_err)?;
                    }
                    fs::copy(&dest_path, &backup_path).map_err(TransactionError::io_err)?;
                }
                std::os::unix::fs::symlink(&target, &dest_path)
                    .map_err(|e| TransactionError(format!("symlink: {}", e)))?;
            }
            #[cfg(not(unix))]
            {
                fs::copy(&path, &dest_path).map_err(TransactionError::io_err)?;
            }
        } else {
            if dest_path.exists() {
                let rel = dest_path.strip_prefix(_prefix).map_err(|_| {
                    TransactionError("dest path must be under prefix".to_string())
                })?;
                let backup_path = backup_root.join(rel);
                if let Some(p) = backup_path.parent() {
                    fs::create_dir_all(p).map_err(TransactionError::io_err)?;
                }
                fs::copy(&dest_path, &backup_path).map_err(TransactionError::io_err)?;
            }
            fs::copy(&path, &dest_path).map_err(TransactionError::io_err)?;
        }
    }
    Ok(())
}

fn restore_backup_dir(prefix: &Path, backup: &Path) -> Result<(), TransactionError> {
    if !backup.exists() || !backup.is_dir() {
        return Ok(());
    }
    restore_backup_dir_at_depth(prefix, backup, backup, 0).map_err(TransactionError::io_err)
}

/// Maximum tree depth when restoring backup (prevents symlink loops / stack overflow).
const RESTORE_BACKUP_MAX_DEPTH: u32 = 256;

fn restore_backup_dir_at_depth(
    prefix: &Path,
    backup_root: &Path,
    current: &Path,
    depth: u32,
) -> io::Result<()> {
    use std::path::Component;
    if depth >= RESTORE_BACKUP_MAX_DEPTH {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "backup tree too deep when restoring",
        ));
    }
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let rel = path.strip_prefix(backup_root).map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidData, e.to_string())
        })?;
        if rel.components().any(|c| c == Component::ParentDir) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "backup path must not contain ..",
            ));
        }
        let dest = prefix.join(rel);
        let meta = fs::symlink_metadata(&path)?;
        if meta.is_dir() && !meta.is_symlink() {
            fs::create_dir_all(&dest)?;
            restore_backup_dir_at_depth(prefix, backup_root, &path, depth + 1)?;
        } else if meta.is_symlink() {
            if let Some(p) = dest.parent() {
                fs::create_dir_all(p)?;
            }
            #[cfg(unix)]
            {
                let target = fs::read_link(&path)?;
                std::os::unix::fs::symlink(&target, &dest)?;
            }
            #[cfg(not(unix))]
            {
                fs::copy(&path, &dest)?;
            }
        } else {
            if let Some(p) = dest.parent() {
                fs::create_dir_all(p)?;
            }
            fs::copy(&path, &dest)?;
        }
    }
    Ok(())
}

fn remove_dir_all_if_exists(p: &Path) -> Result<(), TransactionError> {
    if p.exists() {
        if p.is_dir() {
            fs::remove_dir_all(p).map_err(TransactionError::io_err)?;
        } else {
            fs::remove_file(p).map_err(TransactionError::io_err)?;
        }
    }
    Ok(())
}

fn compute_new_installed_db(
    _prefix: &Path,
    current: &InstalledDb,
    plan: &Plan,
    staging: &Path,
) -> Result<InstalledDb, TransactionError> {
    use crate::resolver::PlanAction;
    let mut by_name: std::collections::BTreeMap<
        crate::package_name::PackageName,
        InstalledEntry,
    > = current
        .list_all()
        .iter()
        .map(|e| (e.name.clone(), e.clone()))
        .collect();

    for step in &plan.steps {
        if step.action == PlanAction::Remove {
            by_name.remove(&step.name);
            continue;
        }
        let dir_name = pkg_dir_name(step)?;
        let pkg_dir = staging.join(&dir_name);
        let install_path = format!("pkgs/{}", dir_name);
        let files = if pkg_dir.exists() {
            list_relative_files(&pkg_dir, &pkg_dir)?
        } else {
            vec![]
        };
        let entry = InstalledEntry {
            name: step.name.clone(),
            version: step.version.clone(),
            revision: step.revision,
            install_path,
            files,
            file_checksums: vec![],
        };
        by_name.insert(step.name.clone(), entry);
    }

    Ok(InstalledDb::from(by_name.into_values().collect::<Vec<_>>()))
}

/// Maximum directory depth when listing files (prevents symlink loops / stack overflow).
const LIST_FILES_MAX_DEPTH: u32 = 256;

fn list_relative_files(dir: &Path, base: &Path) -> Result<Vec<String>, TransactionError> {
    list_relative_files_at_depth(dir, base, 0)
}

fn list_relative_files_at_depth(
    dir: &Path,
    base: &Path,
    depth: u32,
) -> Result<Vec<String>, TransactionError> {
    if depth >= LIST_FILES_MAX_DEPTH {
        return Err(TransactionError(
            "directory tree too deep when listing package files".to_string(),
        ));
    }
    let mut out = vec![];
    for entry in fs::read_dir(dir).map_err(TransactionError::io_err)? {
        let entry = entry.map_err(TransactionError::io_err)?;
        let path = entry.path();
        let rel = path.strip_prefix(base).map_err(|e| {
            TransactionError(format!("strip_prefix: {}", e))
        })?;
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        let meta = fs::symlink_metadata(&path).map_err(TransactionError::io_err)?;
        if meta.is_dir() && !meta.is_symlink() {
            let sub = list_relative_files_at_depth(&path, base, depth + 1)?;
            out.extend(sub);
        } else {
            out.push(rel_str);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package_name::PackageName;
    use crate::resolver::{Plan, PlanAction, PlanStep};
    use crate::source::{Source, SourceHash};
    use crate::version::Version;

    fn fake_plan_step(name: &str, version: &str) -> PlanStep {
        PlanStep {
            name: PackageName::new(name).unwrap(),
            version: Version::new(version).unwrap(),
            revision: 0,
            source: Source {
                type_name: "tarball".to_string(),
                url: format!("https://example.com/{}.tar.gz", name),
                hash: SourceHash::parse("sha256:deadbeef").unwrap(),
            },
            files: vec![],
            action: PlanAction::Install,
        }
    }

    fn fake_remove_step(name: &str, version: &str) -> PlanStep {
        PlanStep {
            name: PackageName::new(name).unwrap(),
            version: Version::new(version).unwrap(),
            revision: 0,
            source: Source {
                type_name: "none".to_string(),
                url: String::new(),
                hash: SourceHash::parse("sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
                    .unwrap(),
            },
            files: vec![],
            action: PlanAction::Remove,
        }
    }

    #[test]
    fn transaction_new_creates_staging_and_log() {
        let tmp = tempfile::TempDir::new().unwrap();
        let prefix = tmp.path();
        prefix::ensure_layout(prefix).unwrap();
        let plan = Plan::default();
        let tx = Transaction::new(TransactionType::Install, plan, prefix).unwrap();
        assert_eq!(tx.state, TransactionState::Pending);
        let staging = staging_path(prefix, tx.id);
        assert!(staging.is_dir());
        let log_path = transaction_log_path(prefix, tx.id);
        assert!(log_path.is_file());
        let content = fs::read_to_string(&log_path).unwrap();
        assert!(content.contains("created"));
        assert!(content.contains(&tx.id.to_string()));
    }

    #[test]
    fn commit_with_one_staged_file_updates_installed_toml() {
        let tmp = tempfile::TempDir::new().unwrap();
        let prefix = tmp.path();
        prefix::ensure_layout(prefix).unwrap();
        let step = fake_plan_step("foo", "1.0");
        let plan = Plan {
            steps: vec![step.clone()],
            satisfied_by_system: vec![],
        };
        let mut tx = Transaction::new(TransactionType::Install, plan, prefix).unwrap();
        let staging = staging_path(prefix, tx.id);
        let pkg_staging = staging.join("foo-1.0");
        fs::create_dir_all(pkg_staging.join("bin")).unwrap();
        fs::write(pkg_staging.join("bin/foo"), "binary").unwrap();

        tx.commit(prefix).unwrap();
        assert_eq!(tx.state, TransactionState::Committed);

        let dest = prefix.join("pkgs/foo-1.0/bin/foo");
        assert!(dest.is_file());
        assert_eq!(fs::read_to_string(&dest).unwrap(), "binary");

        let db = InstalledDb::load(prefix).unwrap();
        let entry = db.get_by_name(&PackageName::new("foo").unwrap()).unwrap();
        assert_eq!(entry.version.as_str(), "1.0");
        assert!(entry.files.iter().any(|f| f.contains("bin/foo")));

        let log_path = transaction_log_path(prefix, tx.id);
        let content = fs::read_to_string(&log_path).unwrap();
        assert!(content.contains("committed"));
    }

    #[test]
    #[cfg(unix)]
    fn commit_preserves_symlink_in_staging() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::TempDir::new().unwrap();
        let prefix = tmp.path();
        prefix::ensure_layout(prefix).unwrap();
        let step = fake_plan_step("pkg", "1.0");
        let plan = Plan {
            steps: vec![step],
            satisfied_by_system: vec![],
        };
        let mut tx = Transaction::new(TransactionType::Install, plan, prefix).unwrap();
        let staging = staging_path(prefix, tx.id);
        let pkg_staging = staging.join("pkg-1.0");
        fs::create_dir_all(pkg_staging.join("bin")).unwrap();
        fs::write(pkg_staging.join("bin/real"), "binary").unwrap();
        symlink("real", pkg_staging.join("bin/link")).unwrap();

        tx.commit(prefix).unwrap();
        let installed_link = prefix.join("pkgs/pkg-1.0/bin/link");
        assert!(
            installed_link.is_symlink(),
            "commit must preserve symlinks"
        );
        assert_eq!(fs::read_link(&installed_link).unwrap(), Path::new("real"));
    }

    #[test]
    fn rollback_restores_installed_toml_and_removes_staging() {
        let tmp = tempfile::TempDir::new().unwrap();
        let prefix = tmp.path();
        prefix::ensure_layout(prefix).unwrap();
        let step = fake_plan_step("bar", "1.0");
        let plan = Plan {
            steps: vec![step],
            satisfied_by_system: vec![],
        };
        let mut tx = Transaction::new(TransactionType::Install, plan, prefix).unwrap();
        let staging = staging_path(prefix, tx.id);
        let pkg_staging = staging.join("bar-1.0");
        fs::create_dir_all(&pkg_staging).unwrap();
        fs::write(pkg_staging.join("file.txt"), "data").unwrap();

        let before_db = InstalledDb::from(vec![InstalledEntry {
            name: PackageName::new("other").unwrap(),
            version: Version::new("1.0").unwrap(),
            revision: 0,
            install_path: "pkgs/other-1.0".to_string(),
            files: vec![],
            file_checksums: vec![],
        }]);
        before_db.write(prefix).unwrap();

        tx.commit(prefix).unwrap();
        let after_commit = prefix.join("pkgs/bar-1.0/file.txt");
        assert!(after_commit.exists());

        tx.state = TransactionState::Pending;
        tx.rollback(prefix, Some("test")).unwrap();
        assert_eq!(tx.state, TransactionState::RolledBack);
        assert!(!staging.exists());
        let db = InstalledDb::load(prefix).unwrap();
        assert!(db.get_by_name(&PackageName::new("bar").unwrap()).is_none());
        assert!(db.get_by_name(&PackageName::new("other").unwrap()).is_some());
    }

    #[test]
    fn lock_blocks_second_until_first_released() {
        let tmp = tempfile::TempDir::new().unwrap();
        let prefix = tmp.path();
        prefix::ensure_layout(prefix).unwrap();
        let g1 = acquire_lock(prefix).unwrap();
        let _start = std::time::Instant::now();
        let _timeout = std::time::Duration::from_millis(500);
        let (tx, rx) = std::sync::mpsc::channel();
        let prefix_clone = prefix.to_path_buf();
        let t = std::thread::spawn(move || {
            let g2 = acquire_lock(prefix_clone.as_path());
            tx.send(g2).unwrap();
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
        drop(g1);
        let g2 = rx.recv().unwrap().unwrap();
        drop(g2);
        t.join().unwrap();
    }

    #[test]
    fn commit_failure_triggers_rollback() {
        let tmp = tempfile::TempDir::new().unwrap();
        let prefix = tmp.path();
        prefix::ensure_layout(prefix).unwrap();
        let plan = Plan {
            steps: vec![
                fake_plan_step("a", "1.0"),
                fake_plan_step("b", "1.0"),
            ],
            satisfied_by_system: vec![],
        };
        let mut tx = Transaction::new(TransactionType::Install, plan, prefix).unwrap();
        let staging = staging_path(prefix, tx.id);
        fs::create_dir_all(staging.join("a-1.0")).unwrap();
        fs::write(staging.join("a-1.0/file"), "a").unwrap();
        // Make b-1.0 a file so read_dir(b-1.0) fails in apply_step; commit fails and rollback runs.
        fs::write(staging.join("b-1.0"), "not-a-dir").unwrap();
        let result = tx.commit(prefix);
        assert!(result.is_err());
        assert_eq!(tx.state, TransactionState::RolledBack);
        assert!(
            !prefix.join("pkgs/a-1.0").exists(),
            "rollback should remove partially installed files"
        );
    }

    #[test]
    fn log_contains_tx_id_and_committed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let prefix = tmp.path();
        prefix::ensure_layout(prefix).unwrap();
        let plan = Plan {
            steps: vec![fake_plan_step("qux", "1.0")],
            satisfied_by_system: vec![],
        };
        let mut tx = Transaction::new(TransactionType::Install, plan, prefix).unwrap();
        let staging = staging_path(prefix, tx.id);
        fs::create_dir_all(staging.join("qux-1.0")).unwrap();
        tx.commit(prefix).unwrap();
        let log_path = transaction_log_path(prefix, tx.id);
        let content = fs::read_to_string(&log_path).unwrap();
        assert!(content.contains(&tx.id.to_string()));
        assert!(content.contains("committed"));
    }

    #[test]
    fn remove_commit_removes_pkg_and_updates_installed_toml() {
        let tmp = tempfile::TempDir::new().unwrap();
        let prefix = tmp.path();
        prefix::ensure_layout(prefix).unwrap();
        let pkg_dir = prefix.join("pkgs/foo-1.0");
        fs::create_dir_all(pkg_dir.join("bin")).unwrap();
        fs::write(pkg_dir.join("bin/foo"), "binary").unwrap();
        let db = InstalledDb::from(vec![InstalledEntry {
            name: PackageName::new("foo").unwrap(),
            version: Version::new("1.0").unwrap(),
            revision: 0,
            install_path: "pkgs/foo-1.0".to_string(),
            files: vec!["bin/foo".to_string()],
            file_checksums: vec![],
        }]);
        db.write(prefix).unwrap();

        let plan = Plan {
            steps: vec![fake_remove_step("foo", "1.0")],
            satisfied_by_system: vec![],
        };
        let mut tx = Transaction::new(TransactionType::Remove, plan, prefix).unwrap();
        tx.commit(prefix).unwrap();
        assert_eq!(tx.state, TransactionState::Committed);
        assert!(!pkg_dir.exists(), "pkg dir should be removed");
        let after_db = InstalledDb::load(prefix).unwrap();
        assert!(after_db.get_by_name(&PackageName::new("foo").unwrap()).is_none());
    }

    #[test]
    fn rollback_remove_restores_pkg() {
        let tmp = tempfile::TempDir::new().unwrap();
        let prefix = tmp.path();
        prefix::ensure_layout(prefix).unwrap();
        let pkg_dir = prefix.join("pkgs/foo-1.0");
        fs::create_dir_all(pkg_dir.join("bin")).unwrap();
        fs::write(pkg_dir.join("bin/foo"), "binary").unwrap();
        let db = InstalledDb::from(vec![InstalledEntry {
            name: PackageName::new("foo").unwrap(),
            version: Version::new("1.0").unwrap(),
            revision: 0,
            install_path: "pkgs/foo-1.0".to_string(),
            files: vec!["bin/foo".to_string()],
            file_checksums: vec![],
        }]);
        db.write(prefix).unwrap();

        let plan = Plan {
            steps: vec![fake_remove_step("foo", "1.0")],
            satisfied_by_system: vec![],
        };
        let mut tx = Transaction::new(TransactionType::Remove, plan, prefix).unwrap();
        tx.commit(prefix).unwrap();
        assert!(!pkg_dir.exists());
        tx.state = TransactionState::Pending;
        tx.rollback(prefix, Some("test")).unwrap();
        assert_eq!(tx.state, TransactionState::RolledBack);
        assert!(pkg_dir.exists(), "rollback should restore pkg dir");
        assert_eq!(fs::read_to_string(pkg_dir.join("bin/foo")).unwrap(), "binary");
        let after_db = InstalledDb::load(prefix).unwrap();
        let entry = after_db.get_by_name(&PackageName::new("foo").unwrap()).unwrap();
        assert_eq!(entry.version.as_str(), "1.0");
    }

    #[test]
    fn upgrade_commit_removes_old_then_installs_new_rollback_restores_old() {
        let tmp = tempfile::TempDir::new().unwrap();
        let prefix = tmp.path();
        prefix::ensure_layout(prefix).unwrap();
        let old_dir = prefix.join("pkgs/foo-1.0");
        fs::create_dir_all(old_dir.join("bin")).unwrap();
        fs::write(old_dir.join("bin/foo"), "v1").unwrap();
        let db = InstalledDb::from(vec![InstalledEntry {
            name: PackageName::new("foo").unwrap(),
            version: Version::new("1.0").unwrap(),
            revision: 0,
            install_path: "pkgs/foo-1.0".to_string(),
            files: vec!["bin/foo".to_string()],
            file_checksums: vec![],
        }]);
        db.write(prefix).unwrap();

        let step = PlanStep {
            name: PackageName::new("foo").unwrap(),
            version: Version::new("2.0").unwrap(),
            revision: 0,
            source: Source {
                type_name: "tarball".to_string(),
                url: "https://example.com/foo.tar.gz".to_string(),
                hash: SourceHash::parse("sha256:deadbeef").unwrap(),
            },
            files: vec![],
            action: PlanAction::Upgrade,
        };
        let plan = Plan {
            steps: vec![step],
            satisfied_by_system: vec![],
        };
        let mut tx = Transaction::new(TransactionType::Upgrade, plan, prefix).unwrap();
        let staging = staging_path(prefix, tx.id);
        let new_staging = staging.join("foo-2.0");
        fs::create_dir_all(new_staging.join("bin")).unwrap();
        fs::write(new_staging.join("bin/foo"), "v2").unwrap();

        tx.commit(prefix).unwrap();
        assert!(!old_dir.exists());
        let new_dir = prefix.join("pkgs/foo-2.0");
        assert!(new_dir.exists());
        assert_eq!(fs::read_to_string(new_dir.join("bin/foo")).unwrap(), "v2");

        tx.state = TransactionState::Pending;
        tx.rollback(prefix, Some("test")).unwrap();
        assert_eq!(tx.state, TransactionState::RolledBack);
        assert!(old_dir.exists(), "rollback should restore old pkg");
        assert_eq!(fs::read_to_string(old_dir.join("bin/foo")).unwrap(), "v1");
        assert!(!new_dir.exists());
    }
}
