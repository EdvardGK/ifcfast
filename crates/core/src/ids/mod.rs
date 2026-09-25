//! Native IDS 1.0 validation (buildingSMART Information Delivery
//! Specification). Design: `docs/plans/2026-09-24_ids-validation-design.md`.
//!
//! Pipeline: [`parse_ids`] (strict XML → schema-independent IR, plus the
//! schema-free audit) → [`compile::compile`] (names resolved against the
//! generated per-schema tables, values compiled, unsupported facets
//! refused) → [`eval`] (candidates from EntityTable type tokens,
//! applicability, requirements, spec cardinality) → [`report::IdsReport`]
//! (column-major specs / elements / failures). [`validate`] is the single
//! entry point.
//!
//! Slices 1–2 implement the Entity, Attribute, Property, Classification
//! and Material facets (the last three read the lazy data layer in
//! [`graph`]). PartOf raises [`IdsError::Unsupported`] (`facet:part_of`)
//! until GH #192 slice 3. A value comparison that needs an undeclared
//! unit raises [`IdsError::UnresolvedUnit`], or under
//! [`OnUnsupported::Mark`] marks that spec `unsupported` (`unit:<TYPE>`).
//!
//! IfcTester is the reference implementation and a test-time oracle
//! only; it is never a runtime dependency.

pub mod attrs;
pub mod audit;
pub mod candidates;
pub mod compile;
pub mod datatypes;
pub mod eval;
pub mod graph;
pub mod ir;
pub mod report;
pub mod restriction;
pub mod schema_tables;
pub mod xml;
pub mod xsd_regex;

pub use ir::{IdsDocument, Schema};
pub use report::IdsReport;
pub use restriction::{matches, Actual, CompiledVal};
pub use xml::parse_ids;
pub use xsd_regex::compile_xsd_pattern;

use std::fmt;

use crate::entity_table::EntityTable;

/// What to do with a specification that uses a facet or construct this
/// engine does not implement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnUnsupported {
    /// Fail the whole call with [`IdsError::Unsupported`].
    Raise,
    /// Report the spec with `status = "unsupported"` and the feature
    /// named in `unsupported_feature`; emit no element rows for it. Never
    /// a fabricated pass or fail.
    Mark,
}

impl OnUnsupported {
    /// `"raise"` / `"mark"` (the Python / CLI spelling).
    pub fn from_name(s: &str) -> Option<OnUnsupported> {
        match s {
            "raise" => Some(OnUnsupported::Raise),
            "mark" => Some(OnUnsupported::Mark),
            _ => None,
        }
    }
}

/// Options for [`validate_with`].
#[derive(Debug, Clone, Copy)]
pub struct ValidateOptions {
    pub on_unsupported: OnUnsupported,
    /// Skip specifications whose `ifcVersion` list does not contain the
    /// file's schema (status `skipped_ifc_version`). Off by default:
    /// IfcTester validates every specification regardless
    /// (`Ids.validate(..., should_filter_version=False)`,
    /// `ifctester/ids.py:167-180`, `:282-284`) and the conformance suite
    /// pins that (`ids/pass-specification_version_is_purely_metadata_and_does_not_impact_pass_or_fail_result`:
    /// an IFC4 file checked against an `ifcVersion="IFC2X3"` spec).
    pub filter_ifc_version: bool,
}

impl Default for ValidateOptions {
    fn default() -> Self {
        ValidateOptions {
            on_unsupported: OnUnsupported::Raise,
            filter_ifc_version: false,
        }
    }
}

/// Validate one IFC (already indexed into `table`, schema `schema`)
/// against one IDS document. The single entry point; see
/// [`validate_with`] for several IDS files over one table.
pub fn validate(
    ids_xml: &[u8],
    table: &EntityTable,
    schema: Schema,
    on_unsupported: OnUnsupported,
) -> Result<IdsReport, IdsError> {
    validate_with(
        &[ids_xml],
        table,
        schema,
        ValidateOptions {
            on_unsupported,
            ..ValidateOptions::default()
        },
    )
}

/// Validate one IFC against several IDS documents over ONE EntityTable.
/// Spec indices run across the documents in order (`specs.ids_index`
/// names the document). Any error in any document fails the call.
pub fn validate_with(
    ids_docs: &[&[u8]],
    table: &EntityTable,
    schema: Schema,
    opts: ValidateOptions,
) -> Result<IdsReport, IdsError> {
    if let Some(err) = table.scan_error() {
        return Err(IdsError::IfcInput {
            msg: format!("refusing a truncated IFC: {err}"),
        });
    }
    let mut plans = Vec::with_capacity(ids_docs.len());
    for xml in ids_docs {
        let doc = parse_ids(xml)?;
        plans.push(compile::compile_with(&doc, schema, opts)?);
    }
    eval::run(&plans, table, schema)
}

/// Map a header `FILE_SCHEMA` identifier onto the IDS schema set.
/// `IFC4X3*` identifiers (IFC4X3, IFC4X3_ADD1, IFC4X3_ADD2, IFC4X3_TC1)
/// map to the IFC4X3_ADD2 tables (the generator pins ifcopenshell 0.8.5,
/// whose `IFC4X3` is ADD2). Anything else is an error, never a guess.
pub fn schema_from_identifier(ident: &str) -> Result<Schema, IdsError> {
    let up = ident.trim().to_ascii_uppercase();
    match up.as_str() {
        "IFC2X3" => Ok(Schema::Ifc2x3),
        "IFC4" => Ok(Schema::Ifc4),
        s if s.starts_with("IFC4X3") => Ok(Schema::Ifc4x3),
        "" => Err(IdsError::IfcInput {
            msg: "the IFC declares no FILE_SCHEMA; the IDS engine needs the schema to resolve entity and attribute names".into(),
        }),
        other => Err(IdsError::IfcInput {
            msg: format!(
                "FILE_SCHEMA '{other}' is not supported by the IDS engine (IFC2X3, IFC4, IFC4X3 only)"
            ),
        }),
    }
}

/// The schema of an IFC buffer, read from its HEADER exactly as the
/// indexer reads it (`indexer::extract_header`).
pub fn schema_from_header(buf: &[u8]) -> Result<Schema, IdsError> {
    let (schema, _) = crate::indexer::extract_header(buf);
    schema_from_identifier(&schema)
}

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
    /// A value comparison needs a unit the model does not declare (or
    /// declares in a form that does not resolve). We never assume SI (same
    /// policy as GH #149). Routed through [`OnUnsupported`] at evaluation:
    /// `Raise` returns it, `Mark` reports the spec `unsupported` with
    /// `unsupported_feature = "unit:<unit_type>"` (ambiguity register A26).
    UnresolvedUnit { unit_type: String },
    /// The IFC side cannot be validated: truncated file, unknown or
    /// missing FILE_SCHEMA, or a record whose arguments do not fit the
    /// schema. Surfaces as the base `IfcfastError` in Python.
    IfcInput { msg: String },
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

    /// Attach a spec index to an `Unsupported` that has none.
    pub(crate) fn in_spec(self, idx: u32) -> Self {
        match self {
            IdsError::Unsupported {
                feature,
                spec_index: None,
            } => IdsError::Unsupported {
                feature,
                spec_index: Some(idx),
            },
            other => other,
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
            IdsError::IfcInput { msg } => write!(f, "IFC cannot be validated: {msg}"),
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
        assert!(
            s.contains("xsd-regex:block:IsThai")
                && s.contains("specification 2")
                && s.contains("IfcTester")
        );
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
