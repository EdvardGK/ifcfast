//! Native IDS 1.0 validation (buildingSMART Information Delivery
//! Specification). Design: `docs/plans/2026-09-24_ids-validation-design.md`.
//!
//! Slice 1A (this layer) is the front half: a strict XML parse into a
//! schema-independent IR, the XSD-regex → Rust `regex` translation, and
//! the value/restriction matcher. Nothing here knows the IFC schema;
//! names (entities, attributes, dataTypes) are resolved and validated
//! by `compile` against the generated schema tables.
//!
//! IfcTester is the reference implementation and a test-time oracle
//! only; it is never a runtime dependency.

pub mod audit;
pub mod datatypes;
pub mod ir;
pub mod restriction;
pub mod schema_tables;
pub mod xml;
pub mod xsd_regex;

// slice-1B/1D modules land here: `compile`, `candidates`, `attrs`,
// `eval`, `report`, `schema_tables` (generated).

pub use ir::IdsDocument;
pub use restriction::{matches, Actual, CompiledVal};
pub use xml::parse_ids;
pub use xsd_regex::compile_xsd_pattern;

use std::fmt;

/// Every failure mode of the IDS layer. No variant is ever swallowed:
/// a malformed IDS, a construct we do not implement, or a unit we
/// cannot resolve all surface to the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdsError {
    /// The IDS document is malformed or violates IDS 1.0 (XSD structure,
    /// cardinality combinations, regex syntax, restriction facets …).
    /// `path` is an XPath-like location (`/ids/specifications/specification[2]/…`),
    /// `line` the 1-based source line when known.
    InvalidIds {
        path: String,
        line: Option<u32>,
        msg: String,
    },
    /// A valid IDS construct this engine does not implement. `feature`
    /// is a stable machine-readable tag (e.g. `xsd-regex:block:IsThai`).
    /// IfcTester is the reference implementation for these cases.
    Unsupported {
        feature: String,
        spec_index: Option<u32>,
    },
    /// A requirement needs a unit the model does not declare. We never
    /// assume SI (same policy as GH #149).
    UnresolvedUnit { unit_type: String },
}

impl IdsError {
    /// Shorthand for an `InvalidIds` without location (callers that know
    /// the location use [`IdsError::at`]).
    pub(crate) fn invalid(msg: impl Into<String>) -> Self {
        IdsError::InvalidIds {
            path: String::new(),
            line: None,
            msg: msg.into(),
        }
    }

    pub(crate) fn unsupported(feature: impl Into<String>) -> Self {
        IdsError::Unsupported {
            feature: feature.into(),
            spec_index: None,
        }
    }

    /// Attach a location to an `InvalidIds` that does not have one yet.
    /// Other variants pass through unchanged.
    pub(crate) fn at(self, at_path: &str, at_line: Option<u32>) -> Self {
        match self {
            IdsError::InvalidIds { path, line, msg } if path.is_empty() => IdsError::InvalidIds {
                path: at_path.to_string(),
                line: line.or(at_line),
                msg,
            },
            other => other,
        }
    }
}

impl fmt::Display for IdsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IdsError::InvalidIds { path, line, msg } => {
                write!(f, "invalid IDS")?;
                if !path.is_empty() {
                    write!(f, " at {path}")?;
                }
                if let Some(l) = line {
                    write!(f, " (line {l})")?;
                }
                write!(f, ": {msg}")
            }
            IdsError::Unsupported {
                feature,
                spec_index,
            } => {
                write!(f, "unsupported IDS feature '{feature}'")?;
                if let Some(i) = spec_index {
                    write!(f, " in specification {i}")?;
                }
                write!(
                    f,
                    "; ifcfast does not implement it — use IfcTester (the reference implementation) for this IDS"
                )
            }
            IdsError::UnresolvedUnit { unit_type } => write!(
                f,
                "IDS requirement needs unit type {unit_type}, which the model does not declare (SI is never assumed)"
            ),
        }
    }
}

impl std::error::Error for IdsError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_error_display_names_location_and_feature() {
        let e = IdsError::invalid("bad").at("/ids/info", Some(3));
        assert_eq!(e.to_string(), "invalid IDS at /ids/info (line 3): bad");
        let u = IdsError::Unsupported {
            feature: "xsd-regex:block:IsThai".into(),
            spec_index: Some(2),
        };
        let s = u.to_string();
        assert!(s.contains("xsd-regex:block:IsThai") && s.contains("specification 2") && s.contains("IfcTester"));
        let w = IdsError::UnresolvedUnit {
            unit_type: "LENGTHUNIT".into(),
        };
        assert!(w.to_string().contains("LENGTHUNIT"));
    }

    #[test]
    fn ids_error_at_keeps_existing_location() {
        let e = IdsError::InvalidIds {
            path: "/a".into(),
            line: Some(1),
            msg: "m".into(),
        }
        .at("/b", Some(9));
        assert_eq!(
            e,
            IdsError::InvalidIds {
                path: "/a".into(),
                line: Some(1),
                msg: "m".into()
            }
        );
    }
}
