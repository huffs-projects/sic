//! Dependency constraint: parse strings like "ripgrep >= 13", "helix = 24.03".

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use crate::package_name::PackageName;
use crate::version::Version;

/// A single dependency constraint: package name, operator, version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DepConstraint {
    /// Package name.
    pub name: PackageName,
    /// Comparison operator.
    pub op: DepOp,
    /// Version to compare against.
    pub version: Version,
}

/// Comparison operator for dependency constraints.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DepOp {
    /// Exact match: `= 24.03`
    Eq,
    /// Greater or equal: `>= 13`
    Ge,
    /// Less or equal: `<= 2.0`
    Le,
    /// Strictly greater: `> 1`
    Gt,
    /// Strictly less: `< 2`
    Lt,
}

impl DepConstraint {
    /// Returns true if the given version satisfies this constraint.
    pub fn satisfies(&self, version: &Version) -> bool {
        let ord = version.cmp(&self.version);
        match self.op {
            DepOp::Eq => ord == Ordering::Equal,
            DepOp::Ge => ord != Ordering::Less,
            DepOp::Le => ord != Ordering::Greater,
            DepOp::Gt => ord == Ordering::Greater,
            DepOp::Lt => ord == Ordering::Less,
        }
    }

    /// Parses a constraint string like `"ripgrep >= 13"` or `"helix = 24.03"`.
    pub fn parse(s: &str) -> Result<Self, InvalidDepConstraint> {
        let s = s.trim();
        if s.is_empty() {
            return Err(InvalidDepConstraint::Empty);
        }
        // Find operator: first occurrence of " >= ", " <= ", " = ", " > ", " < " (with spaces).
        let (name_part, op, version_part) = parse_op(s)?;
        let name = PackageName::new(name_part.trim())
            .map_err(|e| InvalidDepConstraint::InvalidName(e.value))?;
        let version =
            Version::new(version_part.trim()).map_err(|_| InvalidDepConstraint::InvalidVersion)?;
        Ok(DepConstraint { name, op, version })
    }
}

fn parse_op(s: &str) -> Result<(String, DepOp, String), InvalidDepConstraint> {
    const OPS: &[(&str, DepOp)] = &[
        (" >= ", DepOp::Ge),
        (" <= ", DepOp::Le),
        (" = ", DepOp::Eq),
        (" > ", DepOp::Gt),
        (" < ", DepOp::Lt),
    ];
    for (pat, op) in OPS {
        if let Some(pos) = s.find(pat) {
            let (name_part, rest) = s.split_at(pos);
            let version_part = rest.strip_prefix(pat).unwrap_or(rest);
            return Ok((name_part.to_string(), *op, version_part.to_string()));
        }
    }
    Err(InvalidDepConstraint::MissingOperator {
        value: s.to_string(),
    })
}

impl FromStr for DepConstraint {
    type Err = InvalidDepConstraint;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl fmt::Display for DepConstraint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let op_str = match self.op {
            DepOp::Eq => "=",
            DepOp::Ge => ">=",
            DepOp::Le => "<=",
            DepOp::Gt => ">",
            DepOp::Lt => "<",
        };
        write!(f, "{} {} {}", self.name, op_str, self.version)
    }
}

/// Error when dependency constraint string is invalid.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InvalidDepConstraint {
    Empty,
    MissingOperator { value: String },
    InvalidName(String),
    InvalidVersion,
}

impl fmt::Display for InvalidDepConstraint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InvalidDepConstraint::Empty => write!(f, "empty dependency constraint"),
            InvalidDepConstraint::MissingOperator { value } => {
                write!(f, "dependency constraint missing operator: {:?}", value)
            }
            InvalidDepConstraint::InvalidName(name) => {
                write!(f, "invalid package name in constraint: {:?}", name)
            }
            InvalidDepConstraint::InvalidVersion => {
                write!(f, "invalid version in dependency constraint")
            }
        }
    }
}

impl std::error::Error for InvalidDepConstraint {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ge() {
        let c = DepConstraint::parse("ripgrep >= 13").unwrap();
        assert_eq!(c.name.as_str(), "ripgrep");
        assert_eq!(c.op, DepOp::Ge);
        assert_eq!(c.version.as_str(), "13");
    }

    #[test]
    fn parse_eq() {
        let c = DepConstraint::parse("helix = 24.03").unwrap();
        assert_eq!(c.name.as_str(), "helix");
        assert_eq!(c.op, DepOp::Eq);
        assert_eq!(c.version.as_str(), "24.03");
    }

    #[test]
    fn parse_lt_gt() {
        let c = DepConstraint::parse("foo > 1").unwrap();
        assert_eq!(c.op, DepOp::Gt);
        let c = DepConstraint::parse("foo < 2").unwrap();
        assert_eq!(c.op, DepOp::Lt);
    }

    #[test]
    fn reject_empty() {
        assert!(DepConstraint::parse("").is_err());
    }

    #[test]
    fn reject_no_operator() {
        assert!(DepConstraint::parse("ripgrep").is_err());
    }

    #[test]
    fn satisfies_ge() {
        let c = DepConstraint::parse("pkg >= 13").unwrap();
        assert!(c.satisfies(&Version::new("13").unwrap()));
        assert!(c.satisfies(&Version::new("24.03").unwrap()));
        assert!(!c.satisfies(&Version::new("12").unwrap()));
    }

    #[test]
    fn satisfies_eq() {
        let c = DepConstraint::parse("pkg = 24.03").unwrap();
        assert!(c.satisfies(&Version::new("24.03").unwrap()));
        assert!(!c.satisfies(&Version::new("24.02").unwrap()));
        assert!(!c.satisfies(&Version::new("25").unwrap()));
    }

    #[test]
    fn satisfies_lt_gt() {
        let gt = DepConstraint::parse("pkg > 1").unwrap();
        assert!(gt.satisfies(&Version::new("2").unwrap()));
        assert!(!gt.satisfies(&Version::new("1").unwrap()));
        let lt = DepConstraint::parse("pkg < 2").unwrap();
        assert!(lt.satisfies(&Version::new("1").unwrap()));
        assert!(!lt.satisfies(&Version::new("2").unwrap()));
    }
}
