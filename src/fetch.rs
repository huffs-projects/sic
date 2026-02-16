//! Fetch package sources to cache with hash verification.
//!
//! Downloads artifacts to prefix/var/cache (see storage::cache_path), verifies
//! hash (sha256/sha512), and returns the path to the cached file. TLS uses
//! reqwest's rustls and system certificate store.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use sha2::{Digest, Sha256, Sha512};

use crate::source::{Source, SourceHash};
use crate::storage::cache_path;

/// Callback invoked during download with (downloaded_bytes, total_bytes).
pub type ProgressCb = Box<dyn Fn(u64, Option<u64>) + Send>;

/// Minimum bytes between progress updates.
const PROGRESS_THROTTLE_BYTES: u64 = 65536;
/// Minimum interval between progress updates.
const PROGRESS_THROTTLE_MS: u64 = 100;

/// Wraps a writer and invokes an optional callback with (downloaded, total) on progress.
/// Throttles updates to avoid flooding stderr.
struct ProgressWriter<W> {
    inner: W,
    written: u64,
    total: Option<u64>,
    last_reported: u64,
    last_report_time: Instant,
    progress_cb: Option<ProgressCb>,
}

impl<W: Write> ProgressWriter<W> {
    fn new(inner: W, total: Option<u64>, progress_cb: Option<ProgressCb>) -> Self {
        Self {
            inner,
            written: 0,
            total,
            last_reported: 0,
            last_report_time: Instant::now(),
            progress_cb,
        }
    }

    fn maybe_report(&mut self) {
        if let Some(ref cb) = self.progress_cb {
            let bytes_since = self.written.saturating_sub(self.last_reported);
            let elapsed_ms = self.last_report_time.elapsed().as_millis() as u64;
            if bytes_since >= PROGRESS_THROTTLE_BYTES || elapsed_ms >= PROGRESS_THROTTLE_MS {
                self.last_reported = self.written;
                self.last_report_time = Instant::now();
                cb(self.written, self.total);
            }
        }
    }

    /// Report final progress (ignores throttle) so caller can clear the line.
    fn report_final(&mut self) {
        if let Some(ref cb) = self.progress_cb {
            cb(self.written, self.total);
        }
    }

    fn had_progress(&self) -> bool {
        self.progress_cb.is_some()
    }
}

impl<W: Write> Write for ProgressWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.written += n as u64;
        self.maybe_report();
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Formats a byte count for display (e.g. "12.3 MB").
pub(crate) fn format_bytes(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if n >= GB {
        format!("{:.1} GB", n as f64 / GB as f64)
    } else if n >= MB {
        format!("{:.1} MB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{:.1} KB", n as f64 / KB as f64)
    } else {
        format!("{} B", n)
    }
}

/// Errors from fetch or hash verification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FetchError {
    Io(String),
    Http(String),
    HashMismatch {
        expected: String,
        computed: String,
    },
    UnsupportedSourceType(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Io(msg) => write!(f, "fetch io: {}", msg),
            FetchError::Http(msg) => write!(f, "fetch http: {}", msg),
            FetchError::HashMismatch { expected, computed } => {
                write!(f, "hash mismatch: expected {} got {}", expected, computed)
            }
            FetchError::UnsupportedSourceType(t) => {
                write!(f, "unsupported source type: {}", t)
            }
        }
    }
}

impl std::error::Error for FetchError {}

/// Computes the digest of a file using the given algorithm ("sha256" or "sha512").
/// Returns the hex-encoded digest (lowercase).
pub fn compute_file_hash(path: &Path, algorithm: &str) -> Result<String, FetchError> {
    let mut file = fs::File::open(path).map_err(|e| FetchError::Io(e.to_string()))?;
    let mut buf = [0u8; 8192];
    match algorithm.to_lowercase().as_str() {
        "sha256" => {
            let mut hasher = Sha256::new();
            loop {
                let n = file.read(&mut buf).map_err(|e| FetchError::Io(e.to_string()))?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
            }
            Ok(format!("{:x}", hasher.finalize()))
        }
        "sha512" => {
            let mut hasher = Sha512::new();
            loop {
                let n = file.read(&mut buf).map_err(|e| FetchError::Io(e.to_string()))?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
            }
            Ok(format!("{:x}", hasher.finalize()))
        }
        _ => Err(FetchError::UnsupportedSourceType(algorithm.to_string())),
    }
}

/// Verifies that the file at `path` has the digest given by `expected`.
/// Returns `Ok(())` if it matches, `Err(FetchError::HashMismatch)` otherwise.
pub fn verify_file_hash(path: &Path, expected: &SourceHash) -> Result<(), FetchError> {
    let computed = compute_file_hash(path, &expected.algorithm)?;
    if computed != expected.hex {
        return Err(FetchError::HashMismatch {
            expected: expected.hex.clone(),
            computed,
        });
    }
    Ok(())
}

/// Fetches the artifact for `source` into the cache under `prefix`, verifying its hash.
/// If the file already exists at the cache path and the hash matches, returns that path
/// without re-downloading. If the hash does not match, removes the file and re-downloads.
/// Supports `type = "tarball"` with HTTP/HTTPS URLs or file:// URLs (for fixtures/tests).
/// When `progress_cb` is provided, it is called with (downloaded_bytes, total_bytes) during HTTP downloads.
pub fn fetch_to_cache(
    prefix: &Path,
    source: &Source,
    verbose: bool,
    progress_cb: Option<ProgressCb>,
) -> Result<PathBuf, FetchError> {
    if source.type_name != "tarball" {
        return Err(FetchError::UnsupportedSourceType(source.type_name.clone()));
    }

    let dest = cache_path(prefix, source);
    let parent = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| FetchError::Io("cache path has no parent".to_string()))?;

    if dest.exists() {
        if dest.is_file() {
            if verify_file_hash(&dest, &source.hash).is_ok() {
                if verbose {
                    eprintln!("fetch: using cache for {}", source.url);
                }
                return Ok(dest);
            }
            let _ = fs::remove_file(&dest);
        } else {
            let _ = fs::remove_dir_all(&dest);
        }
    }

    if verbose {
        eprintln!("fetch: downloading {}", source.url);
    }

    fs::create_dir_all(parent).map_err(|e: io::Error| FetchError::Io(e.to_string()))?;
    let temp_path = parent.join(format!("artifact.{}.tmp", uuid::Uuid::new_v4()));

    if source.url.starts_with("file://") {
        let path_str = source.url.trim_start_matches("file://");
        let src_path = Path::new(path_str);
        if !src_path.exists() || !src_path.is_file() {
            return Err(FetchError::Io(format!("file URL not found or not a file: {}", source.url)));
        }
        fs::copy(src_path, &temp_path).map_err(|e| FetchError::Io(e.to_string()))?;
        if let Err(e) = verify_file_hash(&temp_path, &source.hash) {
            let _ = fs::remove_file(&temp_path);
            return Err(e);
        }
        fs::rename(&temp_path, &dest).map_err(|e| FetchError::Io(e.to_string()))?;
        if verbose {
            eprintln!("fetch: verified {}", source.hash.algorithm);
        }
        return Ok(dest);
    }

    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(10))
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .map_err(|e| FetchError::Http(e.to_string()))?;

    let mut response = client
        .get(&source.url)
        .send()
        .map_err(|e| FetchError::Http(e.to_string()))?;

    if !response.status().is_success() {
        return Err(FetchError::Http(format!(
            "{} {}",
            response.status().as_u16(),
            response.status().as_str()
        )));
    }

    let content_length = response.content_length();
    let file = fs::File::create(&temp_path).map_err(|e| FetchError::Io(e.to_string()))?;
    let mut writer = ProgressWriter::new(file, content_length, progress_cb);
    response
        .copy_to(&mut writer)
        .map_err(|e| FetchError::Http(e.to_string()))?;

    // Report final progress so callback can clear the line before we print "verified"
    if writer.had_progress() {
        writer.report_final();
    }

    if let Err(e) = verify_file_hash(&temp_path, &source.hash) {
        let _ = fs::remove_file(&temp_path);
        return Err(e);
    }

    fs::rename(&temp_path, &dest).map_err(|e| FetchError::Io(e.to_string()))?;
    if writer.had_progress() {
        // Clear progress line before printing verified (or before next output)
        let _ = std::io::stderr().write_all(b"\r\x1b[K");
    }
    if verbose {
        eprintln!("fetch: verified {}", source.hash.algorithm);
    }
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_file_hash_sha256_returns_64_hex_chars() {
        let tmp = tempfile::TempDir::new().unwrap();
        let f = tmp.path().join("f");
        fs::write(&f, b"hello\n").unwrap();
        let hex = compute_file_hash(&f, "sha256").unwrap();
        assert_eq!(hex.len(), 64);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn verify_file_hash_matches() {
        let tmp = tempfile::TempDir::new().unwrap();
        let f = tmp.path().join("f");
        fs::write(&f, b"x").unwrap();
        let computed = compute_file_hash(&f, "sha256").unwrap();
        let expected = SourceHash::parse(&format!("sha256:{}", computed)).unwrap();
        assert!(verify_file_hash(&f, &expected).is_ok());
    }

    #[test]
    fn verify_file_hash_mismatch() {
        let tmp = tempfile::TempDir::new().unwrap();
        let f = tmp.path().join("f");
        fs::write(&f, b"x").unwrap();
        let expected = SourceHash::parse("sha256:0000000000000000000000000000000000000000000000000000000000000000").unwrap();
        let r = verify_file_hash(&f, &expected);
        assert!(matches!(r, Err(FetchError::HashMismatch { .. })));
    }

    #[test]
    fn fetch_to_cache_when_already_cached() {
        let tmp = tempfile::TempDir::new().unwrap();
        let prefix = tmp.path();
        crate::prefix::ensure_layout(prefix).unwrap();
        let content = b"cached artifact";
        let hash_hex = {
            let mut h = Sha256::new();
            h.update(content);
            format!("{:x}", h.finalize())
        };
        let source = Source {
            type_name: "tarball".to_string(),
            url: "https://example.com/foo.tar.gz".to_string(),
            hash: SourceHash::parse(&format!("sha256:{}", hash_hex)).unwrap(),
        };
        let dest = cache_path(prefix, &source);
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        fs::write(&dest, content).unwrap();

        let path = fetch_to_cache(prefix, &source, false, None).unwrap();
        assert_eq!(path, dest);
        assert_eq!(fs::read(&path).unwrap(), content);
    }

    #[test]
    fn fetch_to_cache_wrong_hash_removes_and_fails() {
        let tmp = tempfile::TempDir::new().unwrap();
        let prefix = tmp.path();
        crate::prefix::ensure_layout(prefix).unwrap();
        let source = Source {
            type_name: "tarball".to_string(),
            url: "https://example.com/bar.tar.gz".to_string(),
            hash: SourceHash::parse("sha256:0000000000000000000000000000000000000000000000000000000000000001").unwrap(),
        };
        let dest = cache_path(prefix, &source);
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        fs::write(&dest, b"wrong content").unwrap();

        let r = fetch_to_cache(prefix, &source, false, None);
        assert!(r.is_err(), "wrong-hash cache should trigger re-download which fails");
        assert!(!dest.exists(), "wrong-hash file should be removed");
    }

    #[test]
    fn unsupported_source_type() {
        let tmp = tempfile::TempDir::new().unwrap();
        let source = Source {
            type_name: "git".to_string(),
            url: "https://example.com/repo".to_string(),
            hash: SourceHash::parse("sha256:abc").unwrap(),
        };
        let r = fetch_to_cache(tmp.path(), &source, false, None);
        assert!(matches!(r, Err(FetchError::UnsupportedSourceType(_))));
    }
}
