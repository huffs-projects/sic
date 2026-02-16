//! Structured resolver failure reporting (Phase 4).
//!
//! Explainable, deterministic failure output: categories, TOML/JSON serialization,
//! human-readable summary, and suggested actions. Resolver returns this type on
//! failure; CLI or callers use `emit_failure` with desired format (human/toml/json).
//! Scripts should use JSON output; human output format is kept stable but prefer
//! machine format for parsing.

use std::fmt;
use std::io::Write;

use owo_colors::OwoColorize;
use serde::{Deserialize, Serialize};

use crate::version::Version;

/// Failure category; determines default suggested action when not overridden.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// Dependency cannot be satisfied (missing package, version constraint).
    Unsatisfiable,
    /// Two packages conflict.
    Conflict,
    /// Platform (arch/os) not supported (optional in v1).
    PlatformMismatch,
    /// Circular dependency.
    Cycle,
    /// Package or version not in lockfile (strict mode).
    NotInLockfile,
    /// Package has dependents; remove them first or use --force.
    HasDependents,
    /// Catch-all (e.g. invalid input).
    Other,
}

/// Structured resolver failure: category plus optional context fields.
/// Serialized as single `[failure]` section in TOML; same structure in JSON.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResolverFailure {
    /// Category (serialized as "type" in output).
    #[serde(rename = "type")]
    pub kind: FailureKind,
    /// Root cause package name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// Version constraint that could not be satisfied (e.g. ">= 13").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_constraint: Option<String>,
    /// Available versions when constraint failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available_versions: Option<Box<Vec<Version>>>,
    /// Dependency strings (e.g. ["ripgrep >= 13"]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<Box<Vec<String>>>,
    /// Conflicting package names (for Conflict).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conflicting_packages: Option<Box<Vec<String>>>,
    /// Override default suggested action when set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_action: Option<String>,
}

/// Wrapper for TOML root: single `[failure]` section.
#[derive(Serialize, Deserialize)]
struct FailureRoot {
    failure: ResolverFailure,
}

/// Output format for failure reporting. CLI will call `emit_failure` with
/// format from env (e.g. SIC_OUTPUT=json) or flag (e.g. --output=json).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputFormat {
    Human,
    Toml,
    Json,
}

/// Returns the default suggested action for a failure kind.
pub fn default_suggested_action(kind: FailureKind) -> &'static str {
    match kind {
        FailureKind::Unsatisfiable => "downgrade request or wait for newer release",
        FailureKind::Conflict => "remove or change one of the conflicting packages",
        FailureKind::PlatformMismatch => "package not available for this platform",
        FailureKind::Cycle => "circular dependency; remove or relax one dependency",
        FailureKind::NotInLockfile => "add package to lockfile or run without strict mode",
        FailureKind::HasDependents => "remove dependents first or use --force",
        FailureKind::Other => "check input and try again",
    }
}

impl ResolverFailure {
    /// Effective suggested action: override if set and non-empty, else default for kind.
    /// Empty or whitespace-only override is treated as "use default".
    pub fn suggested_action(&self) -> String {
        let s = self.suggested_action.as_deref().unwrap_or("");
        if s.trim().is_empty() {
            default_suggested_action(self.kind).to_string()
        } else {
            s.to_string()
        }
    }

    /// Build an unsatisfiable failure.
    pub fn unsatisfiable(
        package: impl Into<String>,
        version_constraint: Option<impl Into<String>>,
        available_versions: Option<Vec<Version>>,
        dependencies: Option<Vec<String>>,
    ) -> Self {
        ResolverFailure {
            kind: FailureKind::Unsatisfiable,
            package: Some(package.into()),
            version_constraint: version_constraint.map(Into::into),
            available_versions: available_versions.map(Box::new),
            dependencies: dependencies.map(Box::new),
            conflicting_packages: None,
            suggested_action: None,
        }
    }

    /// Build a conflict failure.
    pub fn conflict(
        package: impl Into<String>,
        conflicting_packages: Option<Vec<String>>,
    ) -> Self {
        ResolverFailure {
            kind: FailureKind::Conflict,
            package: Some(package.into()),
            version_constraint: None,
            available_versions: None,
            dependencies: None,
            conflicting_packages: conflicting_packages.map(Box::new),
            suggested_action: None,
        }
    }

    /// Build a cycle failure.
    pub fn cycle(package: impl Into<String>) -> Self {
        ResolverFailure {
            kind: FailureKind::Cycle,
            package: Some(package.into()),
            version_constraint: None,
            available_versions: None,
            dependencies: None,
            conflicting_packages: None,
            suggested_action: None,
        }
    }

    /// Build a platform mismatch failure.
    pub fn platform_mismatch(package: impl Into<String>) -> Self {
        ResolverFailure {
            kind: FailureKind::PlatformMismatch,
            package: Some(package.into()),
            version_constraint: None,
            available_versions: None,
            dependencies: None,
            conflicting_packages: None,
            suggested_action: None,
        }
    }

    /// Build a not-in-lockfile failure (strict mode).
    pub fn not_in_lockfile(package: impl Into<String>) -> Self {
        ResolverFailure {
            kind: FailureKind::NotInLockfile,
            package: Some(package.into()),
            version_constraint: None,
            available_versions: None,
            dependencies: None,
            conflicting_packages: None,
            suggested_action: None,
        }
    }

    /// Build a has-dependents failure (remove refused).
    pub fn has_dependents(
        package: impl Into<String>,
        dependents: Vec<String>,
    ) -> Self {
        ResolverFailure {
            kind: FailureKind::HasDependents,
            package: Some(package.into()),
            version_constraint: None,
            available_versions: None,
            dependencies: None,
            conflicting_packages: Some(Box::new(dependents)),
            suggested_action: None,
        }
    }

    /// Build an other/catch-all failure.
    pub fn other(package: Option<impl Into<String>>, detail: Option<impl Into<String>>) -> Self {
        ResolverFailure {
            kind: FailureKind::Other,
            package: package.map(Into::into),
            version_constraint: None,
            available_versions: None,
            dependencies: detail.map(|d| Box::new(vec![d.into()])),
            conflicting_packages: None,
            suggested_action: None,
        }
    }
}

impl fmt::Display for ResolverFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", format_human(self))
    }
}

/// Human-readable format: summary line, optional details, suggested action.
/// Format is kept stable; scripts should use JSON for parsing.
pub fn format_human(f: &ResolverFailure) -> String {
    let (summary, middle, suggested) = format_human_parts(f);
    if middle.is_empty() {
        format!("{}\n{}\n", summary, suggested)
    } else {
        format!("{}\n{}\n{}\n", summary, middle, suggested)
    }
}

/// Parts of human format for optional coloring: (summary_line, middle_lines, suggested_line with newline).
fn format_human_parts(f: &ResolverFailure) -> (String, String, String) {
    let kind_str = match f.kind {
        FailureKind::Unsatisfiable => "Unsatisfiable",
        FailureKind::Conflict => "Conflict",
        FailureKind::PlatformMismatch => "Platform mismatch",
        FailureKind::Cycle => "Cycle",
        FailureKind::NotInLockfile => "Not in lockfile",
        FailureKind::HasDependents => "Has dependents",
        FailureKind::Other => "Other",
    };
    let mut summary = kind_str.to_string();
    if let Some(ref pkg) = f.package {
        summary.push_str(": ");
        summary.push_str(pkg);
        if let Some(ref c) = f.version_constraint {
            summary.push_str(" requires ");
            summary.push_str(c);
        }
        if let Some(ref av) = f.available_versions {
            if !av.is_empty() {
                let vers: Vec<&str> = av.iter().map(Version::as_str).collect();
                summary.push_str("; available: ");
                summary.push_str(&vers.join(", "));
            }
        }
    }
    let mut middle = String::new();
    if let Some(ref deps) = f.dependencies {
        if !deps.is_empty() {
            middle.push_str("dependencies: ");
            middle.push_str(&deps.join(", "));
            middle.push('\n');
        }
    }
    if let Some(ref conf) = f.conflicting_packages {
        if !conf.is_empty() {
            middle.push_str("conflicting packages: ");
            middle.push_str(&conf.join(", "));
            middle.push('\n');
        }
    }
    let action = f.suggested_action();
    let suggested = format!("Suggested: {}\n", action);
    (summary, middle, suggested)
}

/// Serialize failure to TOML string (single `[failure]` section).
pub fn to_toml(f: &ResolverFailure) -> Result<String, FailureSerializeError> {
    let root = FailureRoot {
        failure: f.clone(),
    };
    toml::to_string_pretty(&root).map_err(|e| FailureSerializeError::Toml(e.to_string()))
}

/// Serialize failure to JSON string (one object, same structure as TOML).
pub fn to_json(f: &ResolverFailure) -> Result<String, FailureSerializeError> {
    serde_json::to_string_pretty(f).map_err(|e| FailureSerializeError::Json(e.to_string()))
}

/// Error when serializing or emitting a failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FailureSerializeError {
    Toml(String),
    Json(String),
    Io(String),
}

impl fmt::Display for FailureSerializeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FailureSerializeError::Toml(s) => write!(f, "TOML serialization failed: {}", s),
            FailureSerializeError::Json(s) => write!(f, "JSON serialization failed: {}", s),
            FailureSerializeError::Io(s) => write!(f, "write failed: {}", s),
        }
    }
}

impl std::error::Error for FailureSerializeError {}

/// Emit failure in the requested format to the given writer.
/// CLI will call this with format from env or flag (Phase 11).
/// When format is Human and use_color is true, summary line is red and "Suggested:" line is yellow.
pub fn emit_failure(
    f: &ResolverFailure,
    format: OutputFormat,
    destination: &mut impl Write,
    use_color: bool,
) -> Result<(), FailureSerializeError> {
    match format {
        OutputFormat::Human => {
            if use_color {
                let (summary, middle, suggested) = format_human_parts(f);
                let summary_styled = format!("{}", summary.red());
                let suggested_styled = format!("{}", suggested.yellow());
                let out = if middle.is_empty() {
                    format!("{}\n{}", summary_styled, suggested_styled)
                } else {
                    format!("{}\n{}\n{}", summary_styled, middle, suggested_styled)
                };
                destination
                    .write_all(out.as_bytes())
                    .map_err(|e| FailureSerializeError::Io(e.to_string()))?;
            } else {
                destination
                    .write_all(format_human(f).as_bytes())
                    .map_err(|e| FailureSerializeError::Io(e.to_string()))?;
            }
        }
        OutputFormat::Toml => {
            let s = to_toml(f)?;
            destination
                .write_all(s.as_bytes())
                .map_err(|e| FailureSerializeError::Io(e.to_string()))?;
        }
        OutputFormat::Json => {
            let s = to_json(f)?;
            destination
                .write_all(s.as_bytes())
                .map_err(|e| FailureSerializeError::Io(e.to_string()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::new(s).unwrap()
    }

    #[test]
    fn build_unsatisfiable_roundtrip_toml() {
        let f = ResolverFailure::unsatisfiable(
            "helix",
            Some(">= 25"),
            Some(vec![v("24.03"), v("24.02")]),
            Some(vec!["ripgrep >= 13".to_string()]),
        );
        let toml_str = to_toml(&f).unwrap();
        assert!(toml_str.contains("[failure]"));
        assert!(toml_str.contains("type = \"unsatisfiable\""));
        assert!(toml_str.contains("package = \"helix\""));
        assert!(toml_str.contains("version_constraint = \">= 25\""));
        let root: FailureRoot = toml::from_str(&toml_str).unwrap();
        assert_eq!(root.failure.kind, FailureKind::Unsatisfiable);
        assert_eq!(root.failure.package.as_deref(), Some("helix"));
        assert_eq!(root.failure.version_constraint.as_deref(), Some(">= 25"));
        assert_eq!(
            root.failure.available_versions.as_ref().map(|av| av.len()),
            Some(2)
        );
    }

    #[test]
    fn build_conflict_roundtrip_json() {
        let f = ResolverFailure::conflict(
            "a",
            Some(vec!["b".to_string()]),
        );
        let json_str = to_json(&f).unwrap();
        assert!(json_str.contains("conflict"));
        assert!(json_str.contains("\"package\""));
        assert!(json_str.contains("\"conflicting_packages\""));
        let parsed: ResolverFailure = serde_json::from_str(&json_str).unwrap();
        assert_eq!(parsed.kind, FailureKind::Conflict);
        assert_eq!(parsed.package.as_deref(), Some("a"));
    }

    #[test]
    fn build_cycle_and_other() {
        let cycle = ResolverFailure::cycle("foo");
        assert_eq!(cycle.kind, FailureKind::Cycle);
        assert_eq!(cycle.package.as_deref(), Some("foo"));
        assert_eq!(
            cycle.suggested_action(),
            "circular dependency; remove or relax one dependency"
        );

        let other = ResolverFailure::other(Some("pkg"), Some("missing"));
        assert_eq!(other.kind, FailureKind::Other);
        assert_eq!(other.suggested_action(), "check input and try again");
    }

    #[test]
    fn human_format_contains_summary_and_suggested_action() {
        let f = ResolverFailure::unsatisfiable(
            "helix",
            Some("ripgrep >= 13"),
            Some(vec![v("12.0")]),
            None,
        );
        let human = format_human(&f);
        assert!(human.contains("Unsatisfiable"));
        assert!(human.contains("helix"));
        assert!(human.contains("ripgrep >= 13"));
        assert!(human.contains("12.0"));
        assert!(human.contains("Suggested:"));
        assert!(human.contains("downgrade request or wait for newer release"));
    }

    #[test]
    fn emit_failure_all_formats_no_panic() {
        let f = ResolverFailure::conflict("a", Some(vec!["b".to_string()]));
        let mut human_out = Vec::new();
        emit_failure(&f, OutputFormat::Human, &mut human_out, false).unwrap();
        assert!(!human_out.is_empty());
        let human_s = String::from_utf8(human_out).unwrap();
        assert!(human_s.contains("Conflict"));
        assert!(human_s.contains("Suggested:"));

        let mut toml_out = Vec::new();
        emit_failure(&f, OutputFormat::Toml, &mut toml_out, false).unwrap();
        let toml_s = String::from_utf8(toml_out).unwrap();
        assert!(toml_s.contains("[failure]"));

        let mut json_out = Vec::new();
        emit_failure(&f, OutputFormat::Json, &mut json_out, false).unwrap();
        let json_s = String::from_utf8(json_out).unwrap();
        let _: serde_json::Value = serde_json::from_str(&json_s).unwrap();
    }

    #[test]
    fn empty_suggested_action_uses_default() {
        let mut f = ResolverFailure::unsatisfiable("pkg", None::<&str>, None, None);
        assert!(f.suggested_action().contains("downgrade"));
        f.suggested_action = Some(String::new());
        assert_eq!(f.suggested_action(), "downgrade request or wait for newer release");
        f.suggested_action = Some("  ".to_string());
        assert_eq!(f.suggested_action(), "downgrade request or wait for newer release");
    }

    #[test]
    fn default_suggested_actions_per_kind() {
        assert_eq!(
            default_suggested_action(FailureKind::Unsatisfiable),
            "downgrade request or wait for newer release"
        );
        assert_eq!(
            default_suggested_action(FailureKind::Conflict),
            "remove or change one of the conflicting packages"
        );
        assert_eq!(
            default_suggested_action(FailureKind::PlatformMismatch),
            "package not available for this platform"
        );
        assert_eq!(
            default_suggested_action(FailureKind::Cycle),
            "circular dependency; remove or relax one dependency"
        );
        assert_eq!(
            default_suggested_action(FailureKind::NotInLockfile),
            "add package to lockfile or run without strict mode"
        );
        assert_eq!(
            default_suggested_action(FailureKind::HasDependents),
            "remove dependents first or use --force"
        );
        assert_eq!(
            default_suggested_action(FailureKind::Other),
            "check input and try again"
        );
    }

    #[test]
    fn build_has_dependents() {
        let f = ResolverFailure::has_dependents("ripgrep", vec!["helix".to_string()]);
        assert_eq!(f.kind, FailureKind::HasDependents);
        assert_eq!(f.package.as_deref(), Some("ripgrep"));
        assert_eq!(
            f.conflicting_packages.as_deref().map(|v| v.to_vec()),
            Some(vec!["helix".to_string()])
        );
        assert_eq!(f.suggested_action(), "remove dependents first or use --force");
    }

    #[test]
    fn build_not_in_lockfile() {
        let f = ResolverFailure::not_in_lockfile("foo");
        assert_eq!(f.kind, FailureKind::NotInLockfile);
        assert_eq!(f.package.as_deref(), Some("foo"));
        assert_eq!(
            f.suggested_action(),
            "add package to lockfile or run without strict mode"
        );
        let human = format_human(&f);
        assert!(human.contains("Not in lockfile"));
        assert!(human.contains("foo"));
    }
}
