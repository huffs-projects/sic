//! Integration tests: parse manifest from file.

use sic::manifest::parse_path;
use std::io::Write;
use std::path::Path;

const VALID_MANIFEST: &str = r#"
[sic]
name = "helix"
version = "24.03"
source = { type = "tarball", url = "https://helix-editor.com/helix.tar.gz", hash = "sha256:abcd1234" }
depends = ["ripgrep >= 13"]
provides = ["editor"]
files = ["bin/helix"]
commands = ["helix"]
"#;

#[test]
fn parse_from_path_valid() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    tmp.as_file().write_all(VALID_MANIFEST.as_bytes()).unwrap();
    tmp.as_file().flush().unwrap();
    let path = tmp.path();
    let m = parse_path(path).unwrap();
    assert_eq!(m.name.as_str(), "helix");
    assert_eq!(m.version.as_str(), "24.03");
    assert_eq!(m.depends.len(), 1);
}

#[test]
fn parse_from_path_missing_file() {
    let r = parse_path(Path::new("/nonexistent/manifest.toml"));
    assert!(r.is_err());
}
