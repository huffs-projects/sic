//! Integration tests: failure reporting via public API (Phase 4).
//! Verifies TOML/JSON serialization and human output using library exports.

use sic::{emit_failure, format_human, to_json, to_toml, OutputFormat, ResolverFailure};

#[test]
fn public_api_unsatisfiable_toml_has_failure_section() {
    let f = ResolverFailure::unsatisfiable(
        "helix",
        Some(">= 25"),
        Some(vec![sic::Version::new("24.03").unwrap(), sic::Version::new("24.02").unwrap()]),
        Some(vec!["ripgrep >= 13".to_string()]),
    );
    let s = to_toml(&f).unwrap();
    assert!(s.contains("[failure]"));
    assert!(s.contains("type = \"unsatisfiable\""));
    assert!(s.contains("package = \"helix\""));
}

#[test]
fn public_api_conflict_json_valid() {
    let f = ResolverFailure::conflict("a", Some(vec!["b".to_string()]));
    let s = to_json(&f).unwrap();
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert_eq!(v.get("type").and_then(|t| t.as_str()), Some("conflict"));
    assert_eq!(v.get("package").and_then(|p| p.as_str()), Some("a"));
}

#[test]
fn public_api_format_human_contains_suggested() {
    let f = ResolverFailure::cycle("pkg");
    let human = format_human(&f);
    assert!(human.contains("Cycle"));
    assert!(human.contains("pkg"));
    assert!(human.contains("Suggested:"));
    assert!(human.contains("circular dependency"));
}

#[test]
fn public_api_emit_failure_all_formats() {
    let f = ResolverFailure::unsatisfiable("x", None::<&str>, None, None);
    let mut out = Vec::new();
    emit_failure(&f, OutputFormat::Human, &mut out, false).unwrap();
    assert!(!out.is_empty());
    out.clear();
    emit_failure(&f, OutputFormat::Toml, &mut out, false).unwrap();
    assert!(String::from_utf8(out.clone()).unwrap().contains("[failure]"));
    out.clear();
    emit_failure(&f, OutputFormat::Json, &mut out, false).unwrap();
    let _: serde_json::Value = serde_json::from_slice(&out).unwrap();
}
