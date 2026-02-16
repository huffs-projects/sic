//! Smoke test: lib is linked and CLI runs.

use sic::run_with_args;

#[test]
fn lib_run_returns_zero() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::env::set_var("SIC_ROOT", tmp.path());
    let code = run_with_args(["sic", "status"]);
    std::env::remove_var("SIC_ROOT");
    assert_eq!(code, 0, "sic status should return 0 with empty prefix");
}
