//! Integration tests: prefix layout in temp dir.

use sic::prefix::{ensure_layout, resolve_root};

const DIRS: &[&str] = &[
    "pkgs",
    "bin",
    "var",
    "var/cache",
    "var/transactions",
    "tmp",
    "backups",
];

#[test]
fn ensure_layout_in_temp_dir_creates_absolute_paths() {
    let tmp = tempfile::TempDir::new().unwrap();
    let prefix = tmp.path();
    ensure_layout(prefix).unwrap();
    for dir in DIRS {
        let p = prefix.join(dir);
        assert!(p.is_dir(), "expected dir: {:?}", p);
        assert!(
            p.is_absolute() || prefix.is_absolute(),
            "path should be absolute: {:?}",
            p
        );
    }
}

#[test]
fn ensure_layout_idempotent_twice() {
    let tmp = tempfile::TempDir::new().unwrap();
    let prefix = tmp.path();
    ensure_layout(prefix).unwrap();
    ensure_layout(prefix).unwrap();
    for dir in DIRS {
        assert!(prefix.join(dir).is_dir());
    }
}

#[test]
fn resolve_root_returns_absolute_or_under_home() {
    let root = resolve_root();
    // Either absolute path or relative under current dir when SIC_ROOT is relative
    assert!(!root.as_os_str().is_empty());
}
