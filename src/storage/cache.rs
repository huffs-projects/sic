//! Cache layout contract: deterministic path under prefix/var/cache for fetched artifacts.
//!
//! **Contract:** `var/cache/` under prefix stores fetched package artifacts. Naming is
//! deterministic from (url, hash) so the same source always maps to the same path,
//! enabling lookup without re-download (Phase 8 implements download). No download or
//! filesystem write is performed here; only the path contract and helper.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::source::Source;

/// Returns the path under `prefix/var/cache/` where the artifact for `source` should be
/// stored or found. Deterministic: same (url, hash) always yields the same path.
///
/// Layout: `prefix/var/cache/<sha256(url+hash)/artifact` so that:
/// - Same source (url + hash) maps to one cache entry.
/// - No collision between different sources.
pub fn cache_path(prefix: &Path, source: &Source) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(source.url.as_bytes());
    hasher.update(source.hash.to_string().as_bytes());
    let digest = hasher.finalize();
    let hex = format!("{:x}", digest);
    prefix.join("var").join("cache").join(hex).join("artifact")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::SourceHash;

    #[test]
    fn same_url_hash_same_path() {
        let prefix = Path::new("/tmp/sic");
        let source = Source {
            type_name: "tarball".to_string(),
            url: "https://example.com/foo.tar.gz".to_string(),
            hash: SourceHash::parse("sha256:deadbeef").unwrap(),
        };
        let p1 = cache_path(prefix, &source);
        let p2 = cache_path(prefix, &source);
        assert_eq!(p1, p2);
    }

    #[test]
    fn path_under_prefix_var_cache() {
        let prefix = Path::new("/home/user/.local/sic");
        let source = Source {
            type_name: "tarball".to_string(),
            url: "https://x.com/a.tar.gz".to_string(),
            hash: SourceHash::parse("sha256:abc").unwrap(),
        };
        let p = cache_path(prefix, &source);
        assert!(p.starts_with(prefix));
        assert!(p.to_string_lossy().contains("var"));
        assert!(p.to_string_lossy().contains("cache"));
    }

    #[test]
    fn different_sources_different_paths() {
        let prefix = Path::new("/tmp/sic");
        let s1 = Source {
            type_name: "tarball".to_string(),
            url: "https://a.com/x.tar.gz".to_string(),
            hash: SourceHash::parse("sha256:aaa").unwrap(),
        };
        let s2 = Source {
            type_name: "tarball".to_string(),
            url: "https://b.com/y.tar.gz".to_string(),
            hash: SourceHash::parse("sha256:bbb").unwrap(),
        };
        assert_ne!(cache_path(prefix, &s1), cache_path(prefix, &s2));
    }
}
