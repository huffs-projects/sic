//! CLI integration tests: run the sic binary and assert exit codes and output.

use std::io::{Cursor, Write};

use assert_cmd::Command;
use flate2::write::GzEncoder;
use flate2::Compression;
use sha2::{Digest, Sha256};
use tar::Builder;
use tempfile::TempDir;

#[allow(deprecated)]
fn sic_cmd() -> Command {
    Command::cargo_bin("sic").unwrap()
}

#[test]
fn status_output_json_valid() {
    let tmp = TempDir::new().unwrap();
    sic::ensure_layout(tmp.path()).unwrap();
    let mut cmd = sic_cmd();
    cmd.args(["--prefix", tmp.path().to_str().unwrap(), "--output", "json", "status"]);
    let output = cmd.output().unwrap();
    assert!(output.status.success(), "status --output json should succeed: {:?}", String::from_utf8_lossy(&output.stderr));
    let s = String::from_utf8(output.stdout).unwrap();
    let _: serde_json::Value = serde_json::from_str(&s).expect("status --output json must emit valid JSON");
}

#[test]
fn status_output_toml_emits_packages_section() {
    let tmp = TempDir::new().unwrap();
    sic::ensure_layout(tmp.path()).unwrap();
    let mut cmd = sic_cmd();
    cmd.args(["--prefix", tmp.path().to_str().unwrap(), "--output", "toml", "status"]);
    let output = cmd.output().unwrap();
    assert!(output.status.success(), "stderr: {:?}", String::from_utf8_lossy(&output.stderr));
    let s = String::from_utf8(output.stdout).unwrap();
    assert!(s.contains("[packages]") || s.contains("packages"), "status --output toml should contain packages");
}

#[test]
fn yes_flag_parses_with_status() {
    let tmp = TempDir::new().unwrap();
    sic::ensure_layout(tmp.path()).unwrap();
    let mut cmd = sic_cmd();
    cmd.args(["--prefix", tmp.path().to_str().unwrap(), "--yes", "status"]);
    let output = cmd.output().unwrap();
    assert!(output.status.success(), "sic --yes status should succeed: {:?}", String::from_utf8_lossy(&output.stderr));
}

#[test]
fn short_y_flag_parses_with_status() {
    let tmp = TempDir::new().unwrap();
    sic::ensure_layout(tmp.path()).unwrap();
    let mut cmd = sic_cmd();
    cmd.args(["--prefix", tmp.path().to_str().unwrap(), "-y", "status"]);
    let output = cmd.output().unwrap();
    assert!(output.status.success(), "sic -y status should succeed: {:?}", String::from_utf8_lossy(&output.stderr));
}

#[test]
fn no_subcommand_exits_non_zero() {
    let mut cmd = sic_cmd();
    cmd.assert().failure();
}

#[test]
fn install_without_packages_exits_non_zero() {
    let tmp = TempDir::new().unwrap();
    sic::ensure_layout(tmp.path()).unwrap();
    let mut cmd = sic_cmd();
    cmd.env("SIC_ROOT", tmp.path())
        .args(["install", "foo"]);
    cmd.assert().failure();
}

#[test]
fn resolver_failure_exits_non_zero_and_emits_failure_on_stderr() {
    let tmp = TempDir::new().unwrap();
    sic::ensure_layout(tmp.path()).unwrap();
    let packages = tmp.path().join("packages");
    std::fs::create_dir_all(&packages).unwrap();
    // foo depends on bar >= 1.0; bar has no manifest -> unsatisfiable
    let foo_manifest = r#"
[sic]
name = "foo"
version = "1.0"
source = { type = "tarball", url = "https://example.com/foo.tar.gz", hash = "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855" }
depends = ["bar >= 1.0"]
files = []
"#;
    std::fs::write(packages.join("foo.toml"), foo_manifest).unwrap();

    let mut cmd = sic_cmd();
    cmd.args([
        "--prefix",
        tmp.path().to_str().unwrap(),
        "--packages",
        packages.to_str().unwrap(),
        "install",
        "foo",
    ]);
    let output = cmd.output().unwrap();
    assert!(
        !output.status.success(),
        "install with unsatisfiable deps should fail: stdout={}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(
        output.status.code(),
        Some(sic::cli::EXIT_RESOLVER),
        "expect exit code 1 (resolver failure)"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unsatisfiable") || stderr.contains("bar"),
        "stderr should contain failure (unsatisfiable or missing package bar): {}",
        stderr
    );
}

#[test]
fn resolver_failure_output_json_valid_on_stderr() {
    let tmp = TempDir::new().unwrap();
    sic::ensure_layout(tmp.path()).unwrap();
    let packages = tmp.path().join("packages");
    std::fs::create_dir_all(&packages).unwrap();
    let foo_manifest = r#"
[sic]
name = "foo"
version = "1.0"
source = { type = "tarball", url = "https://example.com/foo.tar.gz", hash = "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855" }
depends = ["nonexistent-pkg >= 1.0"]
files = []
"#;
    std::fs::write(packages.join("foo.toml"), foo_manifest).unwrap();

    let mut cmd = sic_cmd();
    cmd.args([
        "--prefix",
        tmp.path().to_str().unwrap(),
        "--packages",
        packages.to_str().unwrap(),
        "--output",
        "json",
        "install",
        "foo",
    ]);
    let output = cmd.output().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    let v: serde_json::Value =
        serde_json::from_str(&stderr).expect("--output json must emit valid JSON on stderr");
    assert!(
        v.get("type").is_some() || v.get("kind").is_some() || stderr.contains("unsatisfiable"),
        "JSON should contain failure type: {}",
        stderr
    );
}

#[test]
fn resolve_only_with_packages_prints_plan_or_fails() {
    let tmp = TempDir::new().unwrap();
    sic::ensure_layout(tmp.path()).unwrap();
    let packages = tmp.path().join("packages");
    std::fs::create_dir_all(&packages).unwrap();
    let manifest_path = packages.join("foo.toml");
    let content = r#"
[sic]
name = "foo"
version = "1.0"
source = { type = "tarball", url = "https://example.com/foo.tar.gz", hash = "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855" }
depends = []
files = []
"#;
    std::fs::File::create(&manifest_path)
        .unwrap()
        .write_all(content.as_bytes())
        .unwrap();
    let mut cmd = sic_cmd();
    cmd.env("SIC_ROOT", tmp.path())
        .args(["resolve-only", "foo"]);
    let output = cmd.output().unwrap();
    let s = String::from_utf8(output.stdout).unwrap();
    assert!(s.contains("foo") || output.status.success(), "resolve-only should mention foo or succeed");
}

#[test]
fn verbose_flag_emits_resolver_logging_on_stderr() {
    let tmp = TempDir::new().unwrap();
    sic::ensure_layout(tmp.path()).unwrap();
    let packages = tmp.path().join("packages");
    std::fs::create_dir_all(&packages).unwrap();
    let content = r#"
[sic]
name = "foo"
version = "1.0"
source = { type = "tarball", url = "https://example.com/foo.tar.gz", hash = "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855" }
depends = []
files = []
"#;
    std::fs::write(packages.join("foo.toml"), content).unwrap();
    let mut cmd = sic_cmd();
    cmd.args([
        "--prefix",
        tmp.path().to_str().unwrap(),
        "--packages",
        packages.to_str().unwrap(),
        "-v",
        "resolve-only",
        "foo",
    ]);
    let output = cmd.output().unwrap();
    assert!(output.status.success(), "resolve-only -v should succeed: {}", String::from_utf8_lossy(&output.stderr));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("resolve:"),
        "verbose stderr should contain resolve: logging: {}",
        stderr
    );
}

/// Build a minimal .tar.gz with one top-level dir and given relative paths (content = path string).
fn build_tarball(paths: &[&str]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let enc = GzEncoder::new(&mut buf, Compression::default());
        let mut tar_builder = Builder::new(enc);
        for path in paths {
            let data = path.as_bytes();
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_cksum();
            tar_builder
                .append_data(&mut header, path, Cursor::new(data))
                .unwrap();
        }
        tar_builder.finish().unwrap();
    }
    buf
}

#[test]
fn install_fixture_then_status_then_remove() {
    let tmp = TempDir::new().unwrap();
    let prefix = tmp.path();
    sic::ensure_layout(prefix).unwrap();

    let tarball = build_tarball(&["foo-1.0/bin/foo", "foo-1.0/share/bar.txt"]);
    let hash_hex = {
        let mut h = Sha256::new();
        h.update(&tarball);
        format!("{:x}", h.finalize())
    };
    let tarball_path = prefix.join("fixture-foo-1.0.tar.gz");
    std::fs::write(&tarball_path, &tarball).unwrap();

    let file_url = format!("file://{}", tarball_path.to_string_lossy());
    let packages_dir = prefix.join("packages");
    std::fs::create_dir_all(&packages_dir).unwrap();
    let manifest = format!(
        r#"
[sic]
name = "foo"
version = "1.0"
source = {{ type = "tarball", url = "{}", hash = "sha256:{}" }}
depends = []
files = ["bin/foo", "share/bar.txt"]
"#,
        file_url, hash_hex
    );
    std::fs::write(packages_dir.join("foo.toml"), manifest).unwrap();

    let mut cmd = sic_cmd();
    cmd.args([
        "--prefix",
        prefix.to_str().unwrap(),
        "--packages",
        packages_dir.to_str().unwrap(),
        "install",
        "foo",
    ]);
    let output = cmd.output().unwrap();
    assert!(
        output.status.success(),
        "install should succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut cmd = sic_cmd();
    cmd.args(["--prefix", prefix.to_str().unwrap(), "status"]);
    let output = cmd.output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("foo"), "status should list foo: {}", stdout);

    let mut cmd = sic_cmd();
    cmd.args([
        "--prefix",
        prefix.to_str().unwrap(),
        "--packages",
        packages_dir.to_str().unwrap(),
        "remove",
        "foo",
    ]);
    let output = cmd.output().unwrap();
    assert!(
        output.status.success(),
        "remove should succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut cmd = sic_cmd();
    cmd.args(["--prefix", prefix.to_str().unwrap(), "status"]);
    let output = cmd.output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        !stdout.contains("foo\t") && !stdout.contains("foo "),
        "status after remove should not list foo: {}",
        stdout
    );
}

#[test]
fn upgrade_flow_install_v1_then_upgrade_to_v2() {
    let tmp = TempDir::new().unwrap();
    let prefix = tmp.path();
    sic::ensure_layout(prefix).unwrap();
    let packages_dir = prefix.join("packages");
    std::fs::create_dir_all(&packages_dir).unwrap();

    // v1 tarball and manifest only
    let tarball_v1 = build_tarball(&["foo-1.0/bin/foo", "foo-1.0/share/bar.txt"]);
    let hash_v1 = {
        let mut h = Sha256::new();
        h.update(&tarball_v1);
        format!("{:x}", h.finalize())
    };
    let tarball_v1_path = prefix.join("foo-1.0.tar.gz");
    std::fs::write(&tarball_v1_path, &tarball_v1).unwrap();
    let url_v1 = format!("file://{}", tarball_v1_path.to_string_lossy());
    let manifest_v1 = format!(
        r#"
[sic]
name = "foo"
version = "1.0"
source = {{ type = "tarball", url = "{}", hash = "sha256:{}" }}
depends = []
files = ["bin/foo", "share/bar.txt"]
"#,
        url_v1, hash_v1
    );
    std::fs::write(packages_dir.join("foo-1.toml"), manifest_v1).unwrap();

    let mut cmd = sic_cmd();
    cmd.args([
        "--prefix",
        prefix.to_str().unwrap(),
        "--packages",
        packages_dir.to_str().unwrap(),
        "install",
        "foo",
    ]);
    let output = cmd.output().unwrap();
    assert!(
        output.status.success(),
        "install foo v1 should succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut cmd = sic_cmd();
    cmd.args(["--prefix", prefix.to_str().unwrap(), "status"]);
    let output = cmd.output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("foo\t1.0") || stdout.contains("foo\t1.0\n"), "status should show foo 1.0: {}", stdout);

    // Add v2 manifest and tarball
    let tarball_v2 = build_tarball(&["foo-2.0/bin/foo", "foo-2.0/share/bar.txt"]);
    let hash_v2 = {
        let mut h = Sha256::new();
        h.update(&tarball_v2);
        format!("{:x}", h.finalize())
    };
    let tarball_v2_path = prefix.join("foo-2.0.tar.gz");
    std::fs::write(&tarball_v2_path, &tarball_v2).unwrap();
    let url_v2 = format!("file://{}", tarball_v2_path.to_string_lossy());
    let manifest_v2 = format!(
        r#"
[sic]
name = "foo"
version = "2.0"
source = {{ type = "tarball", url = "{}", hash = "sha256:{}" }}
depends = []
files = ["bin/foo", "share/bar.txt"]
"#,
        url_v2, hash_v2
    );
    std::fs::write(packages_dir.join("foo-2.toml"), manifest_v2).unwrap();

    let mut cmd = sic_cmd();
    cmd.args([
        "--prefix",
        prefix.to_str().unwrap(),
        "--packages",
        packages_dir.to_str().unwrap(),
        "upgrade",
        "foo",
    ]);
    let output = cmd.output().unwrap();
    assert!(
        output.status.success(),
        "upgrade foo should succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Upgraded") && stdout.contains("2.0"), "stdout should say Upgraded to 2.0: {}", stdout);

    let mut cmd = sic_cmd();
    cmd.args(["--prefix", prefix.to_str().unwrap(), "status"]);
    let output = cmd.output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("foo\t2.0") || stdout.contains("foo\t2.0\n"), "status after upgrade should show foo 2.0: {}", stdout);
}

#[test]
fn multiple_packages_b_depends_a_remove_order() {
    let tmp = TempDir::new().unwrap();
    let prefix = tmp.path();
    sic::ensure_layout(prefix).unwrap();
    let packages_dir = prefix.join("packages");
    std::fs::create_dir_all(&packages_dir).unwrap();

    let tarball_a = build_tarball(&["a-1.0/bin/a"]);
    let hash_a = {
        let mut h = Sha256::new();
        h.update(&tarball_a);
        format!("{:x}", h.finalize())
    };
    let tarball_a_path = prefix.join("a-1.0.tar.gz");
    std::fs::write(&tarball_a_path, &tarball_a).unwrap();
    let url_a = format!("file://{}", tarball_a_path.to_string_lossy());
    let manifest_a = format!(
        r#"
[sic]
name = "a"
version = "1.0"
source = {{ type = "tarball", url = "{}", hash = "sha256:{}" }}
depends = []
files = ["bin/a"]
"#,
        url_a, hash_a
    );
    std::fs::write(packages_dir.join("a.toml"), manifest_a).unwrap();

    let tarball_b = build_tarball(&["b-1.0/bin/b"]);
    let hash_b = {
        let mut h = Sha256::new();
        h.update(&tarball_b);
        format!("{:x}", h.finalize())
    };
    let tarball_b_path = prefix.join("b-1.0.tar.gz");
    std::fs::write(&tarball_b_path, &tarball_b).unwrap();
    let url_b = format!("file://{}", tarball_b_path.to_string_lossy());
    let manifest_b = format!(
        r#"
[sic]
name = "b"
version = "1.0"
source = {{ type = "tarball", url = "{}", hash = "sha256:{}" }}
depends = ["a >= 1.0"]
files = ["bin/b"]
"#,
        url_b, hash_b
    );
    std::fs::write(packages_dir.join("b.toml"), manifest_b).unwrap();

    // Install a then b
    let mut cmd = sic_cmd();
    cmd.args([
        "--prefix",
        prefix.to_str().unwrap(),
        "--packages",
        packages_dir.to_str().unwrap(),
        "install",
        "a",
    ]);
    let output = cmd.output().unwrap();
    assert!(output.status.success(), "install a: {}", String::from_utf8_lossy(&output.stderr));

    let mut cmd = sic_cmd();
    cmd.args([
        "--prefix",
        prefix.to_str().unwrap(),
        "--packages",
        packages_dir.to_str().unwrap(),
        "install",
        "b",
    ]);
    let output = cmd.output().unwrap();
    assert!(output.status.success(), "install b: {}", String::from_utf8_lossy(&output.stderr));

    let mut cmd = sic_cmd();
    cmd.args(["--prefix", prefix.to_str().unwrap(), "status"]);
    let output = cmd.output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("a\t"), "status should list a: {}", stdout);
    assert!(stdout.contains("b\t"), "status should list b: {}", stdout);

    // Remove a (has dependant b) should fail
    let mut cmd = sic_cmd();
    cmd.args([
        "--prefix",
        prefix.to_str().unwrap(),
        "--packages",
        packages_dir.to_str().unwrap(),
        "remove",
        "a",
    ]);
    let output = cmd.output().unwrap();
    assert!(
        !output.status.success(),
        "remove a (while b depends on it) should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("depend") || stderr.contains("force"), "stderr should mention dependents: {}", stderr);

    // Remove b then a succeeds
    let mut cmd = sic_cmd();
    cmd.args([
        "--prefix",
        prefix.to_str().unwrap(),
        "--packages",
        packages_dir.to_str().unwrap(),
        "remove",
        "b",
    ]);
    let output = cmd.output().unwrap();
    assert!(output.status.success(), "remove b: {}", String::from_utf8_lossy(&output.stderr));

    let mut cmd = sic_cmd();
    cmd.args([
        "--prefix",
        prefix.to_str().unwrap(),
        "--packages",
        packages_dir.to_str().unwrap(),
        "remove",
        "a",
    ]);
    let output = cmd.output().unwrap();
    assert!(output.status.success(), "remove a: {}", String::from_utf8_lossy(&output.stderr));

    let mut cmd = sic_cmd();
    cmd.args(["--prefix", prefix.to_str().unwrap(), "status"]);
    let output = cmd.output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(!stdout.contains("a\t") && !stdout.contains("b\t"), "status should list neither: {}", stdout);
}

#[test]
fn color_never_parses_and_status_succeeds() {
    let tmp = TempDir::new().unwrap();
    sic::ensure_layout(tmp.path()).unwrap();
    let mut cmd = sic_cmd();
    cmd.args([
        "--prefix",
        tmp.path().to_str().unwrap(),
        "--color",
        "never",
        "status",
    ]);
    let output = cmd.output().unwrap();
    assert!(
        output.status.success(),
        "--color never status should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn color_always_emits_ansi_on_error() {
    let tmp = TempDir::new().unwrap();
    sic::ensure_layout(tmp.path()).unwrap();
    let mut cmd = sic_cmd();
    cmd.args([
        "--prefix",
        tmp.path().to_str().unwrap(),
        "--color",
        "always",
        "install",
        "nonexistent",
    ]);
    let output = cmd.output().unwrap();
    assert!(!output.status.success(), "install without packages should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("\x1b["),
        "--color always should emit ANSI on stderr: {}",
        stderr
    );
}

#[test]
fn doctor_after_ensure_layout_all_ok() {
    let tmp = TempDir::new().unwrap();
    let prefix = tmp.path();
    sic::ensure_layout(prefix).unwrap();
    let mut cmd = sic_cmd();
    cmd.args(["--prefix", prefix.to_str().unwrap(), "doctor"]);
    let output = cmd.output().unwrap();
    assert!(
        output.status.success(),
        "doctor should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Layout: OK"), "stdout: {}", stdout);
    assert!(stdout.contains("Lockfile vs installed: OK"), "stdout: {}", stdout);
    assert!(stdout.contains("Symlinks: OK"), "stdout: {}", stdout);
}

#[test]
fn doctor_exits_non_zero_when_layout_broken() {
    let tmp = TempDir::new().unwrap();
    let prefix = tmp.path();
    sic::ensure_layout(prefix).unwrap();
    let bin_path = prefix.join("bin");
    std::fs::remove_dir(&bin_path).unwrap();
    std::fs::write(&bin_path, "not a dir").unwrap();
    let mut cmd = sic_cmd();
    cmd.args(["--prefix", prefix.to_str().unwrap(), "doctor"]);
    let output = cmd.output().unwrap();
    assert!(
        !output.status.success(),
        "doctor should fail when layout is broken"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Layout:") && stdout.contains("issue"), "stdout: {}", stdout);
}

#[test]
fn doctor_output_json_valid() {
    let tmp = TempDir::new().unwrap();
    let prefix = tmp.path();
    sic::ensure_layout(prefix).unwrap();
    let mut cmd = sic_cmd();
    cmd.args([
        "--prefix",
        prefix.to_str().unwrap(),
        "--output",
        "json",
        "doctor",
    ]);
    let output = cmd.output().unwrap();
    assert!(
        output.status.success(),
        "doctor --output json should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("doctor JSON must be valid");
    assert!(v.get("layout").and_then(|l| l.get("ok")).and_then(|o| o.as_bool()) == Some(true));
    assert!(v.get("lockfile").and_then(|l| l.get("ok")).and_then(|o| o.as_bool()) == Some(true));
    assert!(v.get("symlinks").and_then(|s| s.get("ok")).and_then(|o| o.as_bool()) == Some(true));
}

#[test]
#[cfg(unix)]
fn doctor_broken_symlink_reports_issue() {
    use std::os::unix::fs::symlink;

    let tmp = TempDir::new().unwrap();
    let prefix = tmp.path();
    sic::ensure_layout(prefix).unwrap();
    let packages_dir = prefix.join("packages");
    std::fs::create_dir_all(&packages_dir).unwrap();
    let tarball = build_tarball(&["foo-1.0/bin/foo", "foo-1.0/share/bar.txt"]);
    let hash_hex = {
        let mut h = Sha256::new();
        h.update(&tarball);
        format!("{:x}", h.finalize())
    };
    let tarball_path = prefix.join("fixture-foo-1.0.tar.gz");
    std::fs::write(&tarball_path, &tarball).unwrap();
    let file_url = format!("file://{}", tarball_path.to_string_lossy());
    let manifest = format!(
        r#"
[sic]
name = "foo"
version = "1.0"
source = {{ type = "tarball", url = "{}", hash = "sha256:{}" }}
depends = []
files = ["bin/foo", "share/bar.txt"]
"#,
        file_url, hash_hex
    );
    std::fs::write(packages_dir.join("foo.toml"), manifest).unwrap();

    let mut cmd = sic_cmd();
    cmd.args([
        "--prefix",
        prefix.to_str().unwrap(),
        "--packages",
        packages_dir.to_str().unwrap(),
        "install",
        "foo",
    ]);
    let output = cmd.output().unwrap();
    assert!(output.status.success(), "install should succeed: {}", String::from_utf8_lossy(&output.stderr));

    let pkg_bin_foo = prefix.join("pkgs").join("foo-1.0").join("bin").join("foo");
    assert!(pkg_bin_foo.exists(), "pkgs/foo-1.0/bin/foo should exist after install");
    std::fs::remove_file(&pkg_bin_foo).unwrap();
    symlink("/nonexistent/target", &pkg_bin_foo).unwrap();

    let mut cmd = sic_cmd();
    cmd.args(["--prefix", prefix.to_str().unwrap(), "doctor"]);
    let output = cmd.output().unwrap();
    assert!(
        !output.status.success(),
        "doctor should fail when package symlink is broken"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("broken") && stdout.contains("Symlinks"), "stdout: {}", stdout);
}
