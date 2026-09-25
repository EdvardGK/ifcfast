//! Column-major IDS report (design §3.2) and the IfcTester-style facet
//! labels that fill its `applicability_label`, `requirement_labels` and
//! `expected` columns.
//!
//! Labels are ports of IfcTester 0.8.5 `Facet.to_string`
//! (`ifctester/facet.py:123-158`) with the Entity templates
//! (`facet.py:182-197`) and Attribute templates (`facet.py:260-275`).
//! Parameter values print as Python `str()`: a simple value verbatim, a
//! restriction as `str(Restriction.options)` — a dict repr in IfcTester's
//! parse order (`facet.py:1007-1022`); ours uses a fixed key order
//! (enumeration, pattern, bounds, lengths), which equals document order
//! for the usual authoring order. Word-for-word parity is gated in
//! slice 4 (`to_ifctester_json`).

use super::attrs::py_repr_str;
use super::ir::{Facet, FacetCardinality, Restriction, SpecCardinality, Val};

/// Reason codes (design §3.2). Emitted today: entity, attribute,
/// property, classification, material, prohibited and spec-level ones
/// (PartOf arrives in slice 3). Mapping from IfcTester's reasons:
/// `docs/ids/facet-semantics-slice2.md` §5.
pub mod reason {
    pub const ENTITY_MISMATCH: &str = "ENTITY_MISMATCH";
    pub const PREDEFINED_MISMATCH: &str = "PREDEFINED_MISMATCH";
    pub const ATTR_MISSING: &str = "ATTR_MISSING";
    pub const ATTR_VALUE_MISMATCH: &str = "ATTR_VALUE_MISMATCH";
    /// No property set / quantity set of that name (IfcTester NOPSET).
    pub const PSET_MISSING: &str = "PSET_MISSING";
    /// The set exists, the property does not (NOVALUE).
    pub const PROP_MISSING: &str = "PROP_MISSING";
    /// The property exists with a null / `''` / `.U.` / empty value (NOVALUE).
    pub const PROP_NULL: &str = "PROP_NULL";
    /// A complex property / quantity or a reference value: not checkable
    /// by IDS 1.0, counted as absent (ambiguity register A16).
    pub const PROP_UNSUPPORTED: &str = "PROP_UNSUPPORTED";
    /// Value wrapper / quantity measure ≠ dataType (DATATYPE).
    pub const PROP_DATATYPE_MISMATCH: &str = "PROP_DATATYPE_MISMATCH";
    pub const PROP_VALUE_MISMATCH: &str = "PROP_VALUE_MISMATCH";
    pub const CLASS_MISSING: &str = "CLASS_MISSING";
    pub const CLASS_SYSTEM_MISMATCH: &str = "CLASS_SYSTEM_MISMATCH";
    pub const CLASS_VALUE_MISMATCH: &str = "CLASS_VALUE_MISMATCH";
    pub const MATERIAL_MISSING: &str = "MATERIAL_MISSING";
    pub const MATERIAL_VALUE_MISMATCH: &str = "MATERIAL_VALUE_MISMATCH";
    pub const PROHIBITED_PRESENT: &str = "PROHIBITED_PRESENT";
    pub const SPEC_NO_APPLICABLE: &str = "SPEC_NO_APPLICABLE";
    pub const SPEC_PROHIBITED_APPLICABLE: &str = "SPEC_PROHIBITED_APPLICABLE";
}

/// One row per specification.
#[derive(Debug, Default, Clone)]
pub struct SpecsTable {
    /// Which IDS document of the call (0-based).
    pub ids_index: Vec<i32>,
    /// Running index across every document of the call; the join key of
    /// `elements` and `failures`.
    pub spec_index: Vec<i32>,
    pub name: Vec<String>,
    pub identifier: Vec<Option<String>>,
    pub description: Vec<Option<String>>,
    pub instructions: Vec<Option<String>>,
    /// Space-separated IDS tokens (`IFC2X3 IFC4`).
    pub ifc_versions: Vec<String>,
    /// `required` / `optional` / `prohibited`.
    pub cardinality: Vec<&'static str>,
    /// `pass` / `fail` / `skipped_ifc_version` / `unsupported`.
    pub status: Vec<&'static str>,
    pub reason_code: Vec<Option<&'static str>>,
    pub unsupported_feature: Vec<Option<String>>,
    pub applicable: Vec<i64>,
    pub passed: Vec<i64>,
    pub failed: Vec<i64>,
    pub applicability_label: Vec<String>,
    pub requirement_labels: Vec<Vec<String>>,
}

impl ElementsTable {
    pub fn len(&self) -> usize {
        self.step_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.step_id.is_empty()
    }

    /// Drop every row from `n` on (a spec that turned out unsupported).
    pub(crate) fn truncate(&mut self, n: usize) {
        self.spec_index.truncate(n);
        self.step_id.truncate(n);
        self.guid.truncate(n);
        self.entity.truncate(n);
        self.predefined_type.truncate(n);
        self.name.truncate(n);
        self.description.truncate(n);
        self.tag.truncate(n);
        self.type_step_id.truncate(n);
        self.status.truncate(n);
        self.n_failed.truncate(n);
    }
}

impl FailuresTable {
    pub fn len(&self) -> usize {
        self.step_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.step_id.is_empty()
    }

    pub(crate) fn truncate(&mut self, n: usize) {
        self.spec_index.truncate(n);
        self.step_id.truncate(n);
        self.guid.truncate(n);
        self.requirement_index.truncate(n);
        self.facet_type.truncate(n);
        self.facet_cardinality.truncate(n);
        self.reason_code.truncate(n);
        self.expected.truncate(n);
        self.actual.truncate(n);
        self.value_source.truncate(n);
    }
}

/// One row per spec × applicable element.
#[derive(Debug, Default, Clone)]
pub struct ElementsTable {
    pub spec_index: Vec<i32>,
    pub step_id: Vec<i64>,
    /// `None` when the class is not an IfcRoot.
    pub guid: Vec<Option<String>>,
    /// Canonical UPPERCASE class token; the Python layer title-cases it.
    pub entity: Vec<String>,
    /// IfcTester `get_predefined_type` (`ifcopenshell/util/element.py:547-576`).
    pub predefined_type: Vec<Option<String>>,
    pub name: Vec<Option<String>>,
    pub description: Vec<Option<String>>,
    pub tag: Vec<Option<String>>,
    /// The element's type object (`IfcRelDefinesByType`), `None` when
    /// untyped or when the element is itself a type object.
    pub type_step_id: Vec<Option<i64>>,
    /// `pass` / `fail`.
    pub status: Vec<&'static str>,
    pub n_failed: Vec<i16>,
}

/// One row per spec × element × failing requirement.
#[derive(Debug, Default, Clone)]
pub struct FailuresTable {
    pub spec_index: Vec<i32>,
    pub step_id: Vec<i64>,
    pub guid: Vec<Option<String>>,
    pub requirement_index: Vec<i16>,
    /// `entity` / `attribute` / `property` / `classification` /
    /// `material` (`part_of` in slice 3).
    pub facet_type: Vec<&'static str>,
    pub facet_cardinality: Vec<&'static str>,
    pub reason_code: Vec<&'static str>,
    /// The requirement's IfcTester label.
    pub expected: Vec<String>,
    pub actual: Vec<Option<String>>,
    /// `instance` / `type`; `None` when there was no value.
    pub value_source: Vec<Option<&'static str>>,
}

/// The three tables. Rows are ordered by (spec_index, step_id,
/// requirement_index).
#[derive(Debug, Default, Clone)]
pub struct IdsReport {
    pub specs: SpecsTable,
    pub elements: ElementsTable,
    pub failures: FailuresTable,
}

impl IdsReport {
    /// Every spec passed or was skipped for its ifcVersion. An
    /// `unsupported` spec is NOT ok: nothing was checked.
    pub fn ok(&self) -> bool {
        self.specs
            .status
            .iter()
            .all(|s| *s == "pass" || *s == "skipped_ifc_version")
    }
}

pub(crate) fn card_str(c: FacetCardinality) -> &'static str {
    match c {
        FacetCardinality::Required => "required",
        FacetCardinality::Optional => "optional",
        FacetCardinality::Prohibited => "prohibited",
    }
}

pub(crate) fn spec_card_str(c: SpecCardinality) -> &'static str {
    match c {
        SpecCardinality::Required => "required",
        SpecCardinality::Optional => "optional",
        SpecCardinality::Prohibited => "prohibited",
    }
}

// --------------------------------------------------------------------------
// Labels
// --------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clause {
    Applicability,
    Requirement,
}

/// IfcTester `facet.to_string(clause, specification, requirement)`.
pub fn facet_label(
    f: &Facet,
    clause: Clause,
    card: FacetCardinality,
    spec_card: SpecCardinality,
) -> String {
    let (app, req, prohib, params): (&[&str], &[&str], &[&str], Vec<(&str, Option<String>)>) =
        match f {
            // facet.py:182-197
            Facet::Entity(e) => (
                &[
                    "All {name} data of type {predefinedType}",
                    "All {name} data",
                ],
                &[
                    "Shall be {name} data of type {predefinedType}",
                    "Shall be {name} data",
                ],
                &[
                    "Shall not be {name} data of type {predefinedType}",
                    "Shall not be {name} data",
                ],
                vec![
                    ("name", py_str_val(Some(&e.name))),
                    ("predefinedType", py_str_val(e.predefined_type.as_ref())),
                ],
            ),
            // facet.py:260-275
            Facet::Attribute { name, value } => (
                &[
                    "Data where the {name} is {value}",
                    "Data where the {name} is provided",
                ],
                &[
                    "The {name} shall be {value}",
                    "The {name} shall be provided",
                ],
                &[
                    "The {name} shall not be {value}",
                    "The {name} shall not be provided",
                ],
                vec![
                    ("name", py_str_val(Some(name))),
                    ("value", py_str_val(value.as_ref())),
                ],
            ),
            // facet.py:654-665; dataType never appears in a template.
            Facet::Property {
                property_set,
                base_name,
                value,
                ..
            } => (
                &[
                    "Elements with {baseName} data of {value} in the dataset {propertySet}",
                    "Elements with {baseName} data in the dataset {propertySet}",
                ],
                &[
                    "{baseName} data shall be {value} and in the dataset {propertySet}",
                    "{baseName} data shall be provided in the dataset {propertySet}",
                ],
                &[
                    "{baseName} data shall not be {value} and in the dataset {propertySet}",
                    "{baseName} data shall not be provided in the dataset {propertySet}",
                ],
                vec![
                    ("propertySet", py_str_val(Some(property_set))),
                    ("baseName", py_str_val(Some(base_name))),
                    ("value", py_str_val(nonempty(value.as_ref()))),
                ],
            ),
            // facet.py:394-408; parameter order value, system.
            Facet::Classification { system, value, .. } => (
                &[
                    "Data having a {system} reference of {value}",
                    "Data classified using {system}",
                    "Data classified as {value}",
                ],
                &[
                    "Shall have a {system} reference of {value}",
                    "Shall be classified using {system}",
                    "Shall be classified as {value}",
                ],
                &[
                    "Shall not have a {system} reference of {value}",
                    "Shall not be classified using {system}",
                    "Shall not be classified as {value}",
                ],
                vec![
                    ("value", py_str_val(nonempty(value.as_ref()))),
                    ("system", py_str_val(nonempty(Some(system)))),
                ],
            ),
            // facet.py:925-936
            Facet::Material { value, .. } => (
                &[
                    "All data with a {value} material",
                    "All data with a material",
                ],
                &["Shall have a material of {value}", "Shall have a material"],
                &[
                    "Shall not have a material of {value}",
                    "Shall not have a material",
                ],
                vec![("value", py_str_val(nonempty(value.as_ref())))],
            ),
            // Slice 3 ports the PartOf templates.
            other => return format!("{} facet", other.kind()),
        };
    // facet.py:128-145
    let templates: Vec<String> = match clause {
        // The reporter calls `to_string("applicability")` without the
        // specification (reporter.py:296), so the prohibited-spec
        // templates of facet.py:129-131 never apply to these labels.
        Clause::Applicability => app.iter().map(|s| s.to_string()).collect(),
        Clause::Requirement => {
            if spec_card == SpecCardinality::Prohibited {
                return "The requirement is not applicable".into();
            }
            let is_entity = matches!(f, Facet::Entity(_));
            if is_entity || card == FacetCardinality::Required {
                req.iter().map(|s| s.to_string()).collect()
            } else if card == FacetCardinality::Prohibited {
                prohib.iter().map(|s| s.to_string()).collect()
            } else {
                req.iter()
                    .map(|t| {
                        t.replace("shall", "may")
                            .replace("Shall", "May")
                            .replace("must", "may")
                    })
                    .collect()
            }
        }
    };
    fill_template(&templates, &params)
}

/// facet.py:146-158: the first template whose every `{var}` has a
/// non-None parameter.
fn fill_template(templates: &[String], params: &[(&str, Option<String>)]) -> String {
    for t in templates {
        let total = t.matches('{').count();
        let mut out = t.clone();
        let mut done = 0;
        for (k, v) in params {
            let var = format!("{{{k}}}");
            if let Some(v) = v {
                if out.contains(&var) {
                    out = out.replace(&var, v);
                    done += 1;
                }
            }
            if done == total {
                return out;
            }
        }
    }
    "This facet cannot be interpreted".into()
}

/// IfcTester drops falsy parameters: an empty simple value is no value.
fn nonempty(v: Option<&Val>) -> Option<&Val> {
    match v {
        Some(Val::Simple(s)) if s.is_empty() => None,
        other => other,
    }
}

/// Python `str()` of a parsed IfcTester parameter: a simple value is the
/// string itself, a restriction is `str(options)`. `None` for absent.
fn py_str_val(v: Option<&Val>) -> Option<String> {
    match v? {
        Val::Simple(s) => Some(s.clone()),
        Val::Restriction(r) => Some(restriction_repr(r)),
    }
}

/// `str(Restriction.options)` (facet.py:1007-1022, 1076-1077): single
/// values unwrap, repeated facets are lists, `enumeration` is always a
/// list, length facets are ints (the XSD types them
/// `xs:nonNegativeInteger`), everything else is a string.
fn restriction_repr(r: &Restriction) -> String {
    let mut parts: Vec<String> = Vec::new();
    let list = |v: &[String]| {
        format!(
            "[{}]",
            v.iter()
                .map(|s| py_repr_str(s))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    if let Some(en) = &r.enumeration {
        parts.push(format!("'enumeration': {}", list(en)));
    }
    if let Some(ps) = &r.patterns {
        let v = if ps.len() == 1 {
            py_repr_str(&ps[0])
        } else {
            list(ps)
        };
        parts.push(format!("'pattern': {v}"));
    }
    for (k, v) in [
        ("minInclusive", &r.min_inclusive),
        ("minExclusive", &r.min_exclusive),
        ("maxInclusive", &r.max_inclusive),
        ("maxExclusive", &r.max_exclusive),
    ] {
        if let Some(v) = v {
            parts.push(format!("'{k}': {}", py_repr_str(v)));
        }
    }
    for (k, v) in [
        ("length", r.length),
        ("minLength", r.min_length),
        ("maxLength", r.max_length),
    ] {
        if let Some(v) = v {
            parts.push(format!("'{k}': {v}"));
        }
    }
    format!("{{{}}}", parts.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ir::{EntityFacet, XsdBase};

    fn ent(n: &str, p: Option<Val>) -> Facet {
        Facet::Entity(EntityFacet {
            name: Val::Simple(n.into()),
            predefined_type: p,
        })
    }

    fn attr(n: &str, v: Option<Val>) -> Facet {
        Facet::Attribute {
            name: Val::Simple(n.into()),
            value: v,
        }
    }

    const R: FacetCardinality = FacetCardinality::Required;
    const SR: SpecCardinality = SpecCardinality::Required;

    #[test]
    fn ids_labels_entity_templates() {
        assert_eq!(
            facet_label(&ent("IFCWALL", None), Clause::Applicability, R, SR),
            "All IFCWALL data"
        );
        assert_eq!(
            facet_label(
                &ent("IFCWALL", Some(Val::Simple("SOLIDWALL".into()))),
                Clause::Requirement,
                R,
                SR
            ),
            "Shall be IFCWALL data of type SOLIDWALL"
        );
        // Entity requirements ignore cardinality.
        assert_eq!(
            facet_label(
                &ent("IFCWALL", None),
                Clause::Requirement,
                FacetCardinality::Optional,
                SR
            ),
            "Shall be IFCWALL data"
        );
    }

    #[test]
    fn ids_labels_attribute_templates_and_cardinality() {
        let a = attr("Name", Some(Val::Simple("Foobar".into())));
        assert_eq!(
            facet_label(&a, Clause::Applicability, R, SR),
            "Data where the Name is Foobar"
        );
        assert_eq!(
            facet_label(&a, Clause::Requirement, R, SR),
            "The Name shall be Foobar"
        );
        assert_eq!(
            facet_label(&a, Clause::Requirement, FacetCardinality::Optional, SR),
            "The Name may be Foobar"
        );
        assert_eq!(
            facet_label(
                &attr("Name", None),
                Clause::Requirement,
                FacetCardinality::Prohibited,
                SR
            ),
            "The Name shall not be provided"
        );
        assert_eq!(
            facet_label(&a, Clause::Requirement, R, SpecCardinality::Prohibited),
            "The requirement is not applicable"
        );
    }

    #[test]
    fn ids_labels_restriction_repr() {
        let r = Restriction {
            base: XsdBase::String,
            enumeration: Some(vec!["Foo".into(), "Bar".into()]),
            ..Restriction::default()
        };
        assert_eq!(
            facet_label(
                &attr("Name", Some(Val::Restriction(r))),
                Clause::Requirement,
                R,
                SR
            ),
            "The Name shall be {'enumeration': ['Foo', 'Bar']}"
        );
        let p = Restriction {
            patterns: Some(vec!["FOO.*".into()]),
            min_length: Some(2),
            ..Restriction::default()
        };
        assert_eq!(restriction_repr(&p), "{'pattern': 'FOO.*', 'minLength': 2}");
    }
}
