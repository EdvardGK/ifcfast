//! IR → [`Plan`]: resolve names against the generated schema tables of
//! the FILE's schema, compile values, refuse what slice 1 does not
//! implement (design §2.2, §2.4).
//!
//! Schema-dependent audit rules live here (the schema-free ones are in
//! `audit.rs`, applied by `parse_ids`). Each raises
//! `IdsError::InvalidIds`; `invalid-` conformance cases accept either
//! that or an overall `fail` (design §4):
//!
//! | Rule | Pinned by |
//! |---|---|
//! | entity name must exist in the schema (IFC2X3: or `<NAME>TYPE` must, the type-mapping table) | `entity/invalid-invalid_entities_always_fail` (also caught by the audit), `entity/pass-in_ifc2x3_*` |
//! | attribute name must be an explicit, non-derived attribute of the applicability entity (of some entity when there is none) | `attribute/invalid-invalid_attribute_names_always_fail`, `…inverse_attributes…`, `…derived_attributes…` |
//! | a value on an object / list / select attribute can never match | `attribute/invalid-value_checks_always_fail_for_{objects,lists,selects}` |
//! | a simple value must be a literal of the attribute's type (integer, double, boolean) | `attribute/invalid-integers_cannot_be_expressed_as_floating_point_numbers_2_2`, `…specifying_a_float_when_the_value_is_an_integer_is_invalid`, `…only_specifically_formatted_numbers_are_allowed_{1,2}_4`, `…booleans_must_be_specified_as_lowercase_strings_2_3` |
//! | `xs:pattern` on a numeric / boolean attribute can never match | `restriction/invalid-patterns_always_fail_on_any_number`, `…patterns_only_work_on_strings_and_nothing_else` |
//!
//! NOT a rule: a `predefinedType` literal outside the entity's enumeration.
//! It is a user-defined type (ObjectType / ElementType) and legal:
//! `entity/pass-a_predefined_type_may_specify_a_user_defined_object_type`
//! (`WALDO` on IFCWALL), `partof/pass-a_group_predefined_type_must_match_exactly_2_2`.
//! Lowercase literals only fail (`entity/fail-a_predefined_type_from_an_enumeration_must_be_uppercase`).

use super::audit::lexical_ok;
use super::datatypes::datatype_base;
use super::ir::{
    EntityFacet, Facet, FacetCardinality, IdsDocument, Schema, Spec, SpecCardinality, Val, XsdBase,
};
use super::report::{facet_label, Clause};
use super::restriction::{Actual, CompiledVal};
use super::schema_tables::{tables, AttrDef, AttrKind, SchemaTables};
use super::{IdsError, OnUnsupported, ValidateOptions};

/// Which lazy passes the evaluation needs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Needs {
    /// The occurrence → type map from `IfcRelDefinesByType` (predefinedType
    /// resolution, the IFC2X3 type-mapping table, the `type_step_id` and
    /// `predefined_type` columns of the elements table, and the type
    /// inheritance of properties, classifications and materials — so every
    /// active spec needs it).
    pub type_map: bool,
    /// Property records + units ([`super::graph::PropData`]).
    pub properties: bool,
    /// Classification chains ([`super::graph::ClassData`]).
    pub classifications: bool,
    /// Material strings ([`super::graph::MaterialData`]).
    pub materials: bool,
}

/// A compiled IDS document.
#[derive(Debug)]
pub struct Plan {
    pub schema: Schema,
    pub specs: Vec<CompiledSpec>,
    pub needs: Needs,
    /// What evaluation does with an [`IdsError::UnresolvedUnit`] (A26):
    /// raise it, or mark the spec `unsupported` (`unit:<UNITTYPE>`).
    pub on_unsupported: OnUnsupported,
}

/// A specification ready for evaluation, or the reason it will not be.
#[derive(Debug)]
pub struct CompiledSpec {
    /// Position in its document.
    pub index: u32,
    pub name: String,
    pub identifier: Option<String>,
    pub description: Option<String>,
    pub instructions: Option<String>,
    pub ifc_versions: Vec<Schema>,
    pub cardinality: SpecCardinality,
    /// IfcTester `to_string("applicability")` labels, joined with `", "`
    /// (the reporter's join, `ifctester/reporter.py:754`).
    pub applicability_label: String,
    pub requirement_labels: Vec<String>,
    pub state: SpecState,
}

#[derive(Debug)]
pub enum SpecState {
    Active {
        applicability: Vec<CFacet>,
        requirements: Vec<CRequirement>,
    },
    SkippedIfcVersion,
    Unsupported {
        feature: String,
    },
}

#[derive(Debug)]
pub enum CFacet {
    Entity(CEntity),
    Attribute(CAttribute),
    Property(CProperty),
    Classification(CClassification),
    Material(CMaterial),
}

impl CFacet {
    pub fn kind(&self) -> &'static str {
        match self {
            CFacet::Entity(_) => "entity",
            CFacet::Attribute(_) => "attribute",
            CFacet::Property(_) => "property",
            CFacet::Classification(_) => "classification",
            CFacet::Material(_) => "material",
        }
    }
}

/// A property facet (`docs/ids/facet-semantics-slice2.md` §1).
#[derive(Debug)]
pub struct CProperty {
    /// `propertySet`: exact, case-sensitive, or a restriction (P1).
    pub pset: AttrName,
    /// `baseName`, same matching.
    pub base_name: AttrName,
    pub value: Option<CompiledVal>,
    /// Canonical UPPERCASE dataType (`IFCLENGTHMEASURE`), compared
    /// case-insensitively against the value's wrapper (P7).
    pub data_type: Option<String>,
}

/// A classification facet (§2). `system` is required by the XSD; an
/// empty simple value is "no constraint".
#[derive(Debug)]
pub struct CClassification {
    pub system: Option<CompiledVal>,
    pub value: Option<CompiledVal>,
}

/// A material facet (§3).
#[derive(Debug)]
pub struct CMaterial {
    pub value: Option<CompiledVal>,
}

#[derive(Debug)]
pub struct CRequirement {
    pub facet: CFacet,
    /// Always `Required` for an entity facet (IfcTester ignores the
    /// attribute on entity requirements, `facet.py:230-258`).
    pub cardinality: FacetCardinality,
    /// IfcTester `to_string("requirement", spec, requirement)`.
    pub label: String,
}

#[derive(Debug)]
pub struct CEntity {
    pub name: EntityName,
    pub predefined: Option<CPredef>,
}

/// A resolved entity-facet name.
#[derive(Debug)]
pub enum EntityName {
    /// A class of the schema (canonical uppercase).
    Exact(&'static str),
    /// IFC2X3 only: `name` is not a class of the schema but `type_name`
    /// (`<name>TYPE`) is. IfcTester collects the occurrences of that type
    /// (`ifctester/facet.py:208-216`) and matches an occurrence whose type
    /// object is `<name>TYPE` (`facet.py:234-243`).
    Mapped2x3 {
        name: String,
        type_name: &'static str,
    },
    /// A pattern / enumeration, resolved to the schema classes it matches
    /// (sorted). Matching stays exact-class (no subtypes).
    Set(Vec<&'static str>),
}

impl EntityName {
    /// Does a record of class `class` (canonical uppercase) match by name?
    pub fn matches_class(&self, class: &str) -> bool {
        match self {
            EntityName::Exact(n) => *n == class,
            EntityName::Mapped2x3 { name, .. } => name == class,
            EntityName::Set(v) => v.binary_search(&class).is_ok(),
        }
    }

    /// The name as a plain string, for the IFC2X3 `<name>TYPE` rule.
    /// `None` for a restriction (IfcTester calls `str.endswith` on it and
    /// raises; we skip the rule — ambiguity register A6).
    pub fn plain(&self) -> Option<&str> {
        match self {
            EntityName::Exact(n) => Some(n),
            EntityName::Mapped2x3 { name, .. } => Some(name),
            EntityName::Set(_) => None,
        }
    }
}

#[derive(Debug)]
pub struct CPredef {
    pub val: CompiledVal,
    /// IfcTester `self.predefinedType == "USERDEFINED"` (`facet.py:246`):
    /// a value that accepts the literal `USERDEFINED` asks "is the type
    /// user-defined?" instead of comparing the resolved type.
    pub userdefined_query: bool,
}

#[derive(Debug)]
pub struct CAttribute {
    pub name: AttrName,
    pub value: Option<CompiledVal>,
}

#[derive(Debug)]
pub enum AttrName {
    Exact(String),
    Pattern(CompiledVal),
}

impl AttrName {
    pub fn matches(&self, attr: &str) -> bool {
        match self {
            AttrName::Exact(n) => n == attr,
            AttrName::Pattern(cv) => cv.matches(&Actual::Str(attr.to_string())),
        }
    }
}

/// Compile with the default options (unsupported → error, no version
/// filtering).
pub fn compile(doc: &IdsDocument, schema: Schema) -> Result<Plan, IdsError> {
    compile_with(doc, schema, ValidateOptions::default())
}

pub fn compile_with(
    doc: &IdsDocument,
    schema: Schema,
    opts: ValidateOptions,
) -> Result<Plan, IdsError> {
    let t = tables(schema);
    let mut specs = Vec::with_capacity(doc.specs.len());
    let mut needs = Needs::default();
    for sp in &doc.specs {
        let applicability_label = sp
            .applicability
            .iter()
            .map(|f| {
                facet_label(
                    f,
                    Clause::Applicability,
                    FacetCardinality::Required,
                    sp.cardinality,
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let requirement_labels: Vec<String> = sp
            .requirements
            .iter()
            .map(|r| facet_label(&r.facet, Clause::Requirement, r.cardinality, sp.cardinality))
            .collect();
        let state = if opts.filter_ifc_version && !sp.ifc_versions.contains(&schema) {
            SpecState::SkippedIfcVersion
        } else if let Some(feature) = unsupported_feature(sp) {
            match opts.on_unsupported {
                OnUnsupported::Raise => {
                    return Err(IdsError::Unsupported {
                        feature,
                        spec_index: Some(sp.idx),
                    })
                }
                OnUnsupported::Mark => SpecState::Unsupported { feature },
            }
        } else {
            match compile_spec(sp, schema, t) {
                Ok((applicability, requirements)) => {
                    needs.type_map = true;
                    for f in applicability
                        .iter()
                        .chain(requirements.iter().map(|r| &r.facet))
                    {
                        match f {
                            CFacet::Property(_) => needs.properties = true,
                            CFacet::Classification(_) => needs.classifications = true,
                            CFacet::Material(_) => needs.materials = true,
                            CFacet::Entity(_) | CFacet::Attribute(_) => {}
                        }
                    }
                    SpecState::Active {
                        applicability,
                        requirements,
                    }
                }
                Err(IdsError::Unsupported { feature, .. })
                    if opts.on_unsupported == OnUnsupported::Mark =>
                {
                    SpecState::Unsupported { feature }
                }
                Err(e) => return Err(e.in_spec(sp.idx)),
            }
        };
        specs.push(CompiledSpec {
            index: sp.idx,
            name: sp.name.clone(),
            identifier: sp.identifier.clone(),
            description: sp.description.clone(),
            instructions: sp.instructions.clone(),
            ifc_versions: sp.ifc_versions.clone(),
            cardinality: sp.cardinality,
            applicability_label,
            requirement_labels,
            state,
        });
    }
    Ok(Plan {
        schema,
        specs,
        needs,
        on_unsupported: opts.on_unsupported,
    })
}

/// The first facet this engine does not implement (PartOf until slice 3),
/// as `facet:<kind>`.
fn unsupported_feature(sp: &Spec) -> Option<String> {
    sp.applicability
        .iter()
        .chain(sp.requirements.iter().map(|r| &r.facet))
        .find(|f| matches!(f, Facet::PartOf { .. }))
        .map(|f| format!("facet:{}", f.kind()))
}

fn spec_path(sp: &Spec) -> String {
    format!("/ids/specifications/specification[{}]", sp.idx + 1)
}

fn invalid(sp: &Spec, clause: &str, i: usize, msg: String) -> IdsError {
    IdsError::InvalidIds {
        path: format!("{}/{clause}/*[{}]", spec_path(sp), i + 1),
        line: None,
        msg,
    }
}

type Compiled = (Vec<CFacet>, Vec<CRequirement>);

fn compile_spec(sp: &Spec, schema: Schema, t: &'static SchemaTables) -> Result<Compiled, IdsError> {
    // Applicability entity first: it is the context attribute names are
    // checked against.
    let mut applicability = Vec::with_capacity(sp.applicability.len());
    let mut context: Option<Vec<&'static str>> = None;
    for (i, f) in sp.applicability.iter().enumerate() {
        if let Facet::Entity(e) = f {
            let ce =
                compile_entity(e, schema, t).map_err(|m| invalid(sp, "applicability", i, m))?;
            context = match &ce.name {
                EntityName::Exact(n) => Some(vec![n]),
                EntityName::Set(v) => Some(v.clone()),
                EntityName::Mapped2x3 { .. } => None,
            };
            applicability.push((i, CFacet::Entity(ce)));
        }
    }
    for (i, f) in sp.applicability.iter().enumerate() {
        let at = |e: IdsError| {
            e.at(
                &format!("{}/applicability/*[{}]", spec_path(sp), i + 1),
                None,
            )
        };
        match f {
            Facet::Attribute { name, value } => {
                let ca =
                    compile_attribute(name, value.as_ref(), context.as_deref(), t).map_err(at)?;
                applicability.push((i, CFacet::Attribute(ca)));
            }
            Facet::Entity(_) | Facet::PartOf { .. } => {}
            other => applicability.push((i, compile_data_facet(other, t).map_err(at)?)),
        }
    }
    applicability.sort_by_key(|(i, _)| *i);
    let applicability = applicability.into_iter().map(|(_, f)| f).collect();

    let mut requirements = Vec::with_capacity(sp.requirements.len());
    for (i, r) in sp.requirements.iter().enumerate() {
        let label = facet_label(&r.facet, Clause::Requirement, r.cardinality, sp.cardinality);
        let (facet, cardinality) = match &r.facet {
            Facet::Entity(e) => (
                CFacet::Entity(
                    compile_entity(e, schema, t).map_err(|m| invalid(sp, "requirements", i, m))?,
                ),
                FacetCardinality::Required,
            ),
            Facet::Attribute { name, value } => (
                CFacet::Attribute(
                    compile_attribute(name, value.as_ref(), context.as_deref(), t).map_err(
                        |e| {
                            e.at(
                                &format!("{}/requirements/*[{}]", spec_path(sp), i + 1),
                                None,
                            )
                        },
                    )?,
                ),
                r.cardinality,
            ),
            f
            @ (Facet::Property { .. } | Facet::Classification { .. } | Facet::Material { .. }) => (
                compile_data_facet(f, t).map_err(|e| {
                    e.at(
                        &format!("{}/requirements/*[{}]", spec_path(sp), i + 1),
                        None,
                    )
                })?,
                r.cardinality,
            ),
            // unsupported_feature() ran first.
            other => {
                return Err(IdsError::Unsupported {
                    feature: format!("facet:{}", other.kind()),
                    spec_index: Some(sp.idx),
                })
            }
        };
        requirements.push(CRequirement {
            facet,
            cardinality,
            label,
        });
    }
    Ok((applicability, requirements))
}

/// `Val::Simple("")` is "no constraint" (IfcTester drops falsy values;
/// the canonical IR maps it to null).
fn nonempty(v: Option<&Val>) -> Option<&Val> {
    match v {
        Some(Val::Simple(s)) if s.is_empty() => None,
        other => other,
    }
}

/// A plain name or a restriction, for `propertySet` / `baseName`.
fn compile_name(v: &Val) -> Result<AttrName, IdsError> {
    Ok(match v {
        Val::Simple(s) => AttrName::Exact(s.clone()),
        r @ Val::Restriction(_) => AttrName::Pattern(CompiledVal::new(r)?),
    })
}

/// Property, classification and material facets.
fn compile_data_facet(f: &Facet, t: &'static SchemaTables) -> Result<CFacet, IdsError> {
    let cv = |v: Option<&Val>| nonempty(v).map(CompiledVal::new).transpose();
    Ok(match f {
        Facet::Property {
            property_set,
            base_name,
            value,
            data_type,
            ..
        } => {
            let data_type = match data_type.as_deref().filter(|d| !d.is_empty()) {
                None => None,
                Some(d) => Some(resolve_data_type(d, t)?),
            };
            CFacet::Property(CProperty {
                pset: compile_name(property_set)?,
                base_name: compile_name(base_name)?,
                value: cv(value.as_ref())?,
                data_type,
            })
        }
        Facet::Classification { system, value, .. } => CFacet::Classification(CClassification {
            system: cv(Some(system))?,
            value: cv(value.as_ref())?,
        }),
        Facet::Material { value, .. } => CFacet::Material(CMaterial {
            value: cv(value.as_ref())?,
        }),
        other => {
            return Err(IdsError::unsupported(format!("facet:{}", other.kind())));
        }
    })
}

/// A property `dataType` must be an IDS dataType (DataTypes.md), and one
/// the file's schema has when it is a measure or an IfcValue member
/// (design §2.5). Enumeration and other defined types are only checked
/// against DataTypes.md (the schema tables carry no full type list).
fn resolve_data_type(d: &str, t: &'static SchemaTables) -> Result<String, IdsError> {
    let up = d.to_ascii_uppercase();
    if datatype_base(&up).is_none() {
        return Err(IdsError::invalid(format!(
            "dataType '{d}' is not an IDS 1.0 dataType (DataTypes.md)"
        )));
    }
    let in_any_schema_vocab = [Schema::Ifc2x3, Schema::Ifc4, Schema::Ifc4x3]
        .iter()
        .any(|s| tables(*s).unit_type_for_measure(&up).is_some());
    if in_any_schema_vocab && t.unit_type_for_measure(&up).is_none() {
        return Err(IdsError::invalid(format!(
            "dataType '{d}' does not exist in the file's schema"
        )));
    }
    Ok(up)
}

fn compile_entity(
    e: &EntityFacet,
    schema: Schema,
    t: &'static SchemaTables,
) -> Result<CEntity, String> {
    let name = match &e.name {
        Val::Simple(n) => match t.canonical(n) {
            // Exact spelling only: the audit already refused lowercase.
            Some(c) if c == n => EntityName::Exact(c),
            Some(_) | None => {
                let type_name = format!("{n}TYPE");
                match (schema, n.ends_with("TYPE"), t.canonical(&type_name)) {
                    (Schema::Ifc2x3, false, Some(tn))
                        if tn == type_name && t.is_type_object(tn) =>
                    {
                        EntityName::Mapped2x3 {
                            name: n.clone(),
                            type_name: tn,
                        }
                    }
                    _ => {
                        return Err(format!(
                            "entity '{n}' does not exist in {}",
                            schema.ids_token()
                        ))
                    }
                }
            }
        },
        v @ Val::Restriction(_) => {
            let cv = CompiledVal::new(v).map_err(|e| e.to_string())?;
            let set: Vec<&'static str> = t
                .entities
                .iter()
                .copied()
                .filter(|c| cv.matches(&Actual::Str((*c).to_string())))
                .collect();
            EntityName::Set(set)
        }
    };
    let predefined = match nonempty(e.predefined_type.as_ref()) {
        None => None,
        Some(v) => {
            let val = CompiledVal::new(v).map_err(|e| e.to_string())?;
            let userdefined_query = val.matches(&Actual::Str("USERDEFINED".into()));
            Some(CPredef {
                val,
                userdefined_query,
            })
        }
    };
    Ok(CEntity { name, predefined })
}

fn compile_attribute(
    name: &Val,
    value: Option<&Val>,
    context: Option<&[&'static str]>,
    t: &'static SchemaTables,
) -> Result<CAttribute, IdsError> {
    let value = nonempty(value);
    let (cname, matched): (AttrName, Vec<&'static AttrDef>) = match name {
        Val::Simple(n) => {
            let classes: Box<dyn Iterator<Item = &&'static str>> = match context {
                Some(c) => Box::new(c.iter()),
                None => Box::new(t.entities.iter()),
            };
            let defs: Vec<&'static AttrDef> = classes
                .flat_map(|c| t.attrs(c).iter().filter(|a| a.name == n.as_str()))
                .collect();
            let where_ = match context {
                Some([one]) => format!("of {one}"),
                Some(_) => "of any applicable entity".to_string(),
                None => "of any entity in the schema".to_string(),
            };
            if defs.is_empty() {
                return Err(IdsError::invalid(format!(
                    "'{n}' is not an explicit attribute {where_}; inverse and derived attributes \
                     cannot be checked by an attribute facet"
                )));
            }
            let live: Vec<&'static AttrDef> = defs.into_iter().filter(|a| !a.derived).collect();
            if live.is_empty() {
                return Err(IdsError::invalid(format!(
                    "'{n}' is a derived attribute {where_}; derived attributes cannot be checked"
                )));
            }
            (AttrName::Exact(n.clone()), live)
        }
        v @ Val::Restriction(_) => {
            let cv = CompiledVal::new(v)?;
            let defs = match context {
                Some(c) => c
                    .iter()
                    .flat_map(|cls| t.attrs(cls).iter())
                    .filter(|a| !a.derived && cv.matches(&Actual::Str(a.name.to_string())))
                    .collect(),
                None => Vec::new(),
            };
            (AttrName::Pattern(cv), defs)
        }
    };
    if let (Some(v), false) = (value, matched.is_empty()) {
        check_value_against_kinds(v, &matched)?;
    }
    let value = value.map(CompiledVal::new).transpose()?;
    Ok(CAttribute { name: cname, value })
}

/// Rules that make a value impossible to satisfy for every attribute the
/// name can resolve to.
fn check_value_against_kinds(v: &Val, defs: &[&'static AttrDef]) -> Result<(), IdsError> {
    use AttrKind::*;
    let all = |p: &dyn Fn(AttrKind) -> bool| defs.iter().all(|d| p(d.kind));
    let names = {
        let mut n: Vec<&str> = defs.iter().map(|d| d.name).collect();
        n.dedup();
        n.join("/")
    };
    if all(&|k| matches!(k, Ref | List | Select | Other)) {
        return Err(IdsError::invalid(format!(
            "attribute '{names}' holds an object, list or select value; a value check on it always fails \
             (IDS compares simple values only)"
        )));
    }
    let numeric_or_bool = all(&|k| matches!(k, Int | Real | Bool | Logical));
    match v {
        Val::Restriction(r) if numeric_or_bool && r.patterns.is_some() => Err(IdsError::invalid(
            format!("xs:pattern only matches strings; attribute '{names}' is numeric or boolean"),
        )),
        Val::Simple(s) => {
            let base = if all(&|k| k == Int) {
                Some(XsdBase::Integer)
            } else if all(&|k| matches!(k, Int | Real)) {
                Some(XsdBase::Double)
            } else if all(&|k| matches!(k, Bool | Logical)) {
                Some(XsdBase::Boolean)
            } else {
                None
            };
            match base {
                Some(b) if !lexical_ok(b, s) => Err(IdsError::invalid(format!(
                    "value '{s}' is not a valid xs:{} literal, which attribute '{names}' requires",
                    b.local_name()
                ))),
                _ => Ok(()),
            }
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::parse_ids;

    fn doc(ver: &str, app: &str, req: &str) -> IdsDocument {
        let x = format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<ids xmlns="http://standards.buildingsmart.org/IDS" xmlns:xs="http://www.w3.org/2001/XMLSchema">
<info><title>T</title></info><specifications>
<specification name="S" ifcVersion="{ver}"><applicability minOccurs="1" maxOccurs="unbounded">{app}</applicability>
<requirements>{req}</requirements></specification></specifications></ids>"#
        );
        parse_ids(x.as_bytes()).unwrap_or_else(|e| panic!("{e}"))
    }

    fn ent(n: &str) -> String {
        format!("<entity><name><simpleValue>{n}</simpleValue></name></entity>")
    }

    fn attr(n: &str, v: &str) -> String {
        format!("<attribute><name><simpleValue>{n}</simpleValue></name><value><simpleValue>{v}</simpleValue></value></attribute>")
    }

    fn err(d: &IdsDocument, s: Schema) -> String {
        match compile(d, s) {
            Err(IdsError::InvalidIds { msg, .. }) => msg,
            other => panic!("expected InvalidIds, got {other:?}"),
        }
    }

    #[test]
    fn ids_compile_resolves_and_rejects_names() {
        let d = doc("IFC4", &ent("IFCWALL"), &attr("Name", "x"));
        assert!(compile(&d, Schema::Ifc4).is_ok());
        let d = doc("IFC4", &ent("IFCWALL"), &attr("ActingRole", "x"));
        assert!(err(&d, Schema::Ifc4).contains("ActingRole"));
        let d = doc("IFC4", &ent("IFCPERSON"), &attr("EngagedIn", "x"));
        assert!(err(&d, Schema::Ifc4).contains("inverse"));
        let d = doc("IFC4", &ent("IFCRABBITS"), "");
        assert!(err(&d, Schema::Ifc4).contains("IFCRABBITS"));
        // IFC2X3 type-mapping table: IFCAIRTERMINAL is not an IFC2X3 class.
        let d = doc("IFC2X3", &ent("IFCAIRTERMINAL"), &attr("Name", "x"));
        let p = compile(&d, Schema::Ifc2x3).unwrap_or_else(|e| panic!("{e}"));
        match &p.specs[0].state {
            SpecState::Active { applicability, .. } => match &applicability[0] {
                CFacet::Entity(CEntity {
                    name: EntityName::Mapped2x3 { type_name, .. },
                    ..
                }) => assert_eq!(*type_name, "IFCAIRTERMINALTYPE"),
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn ids_compile_value_kind_rules() {
        let d = doc(
            "IFC4",
            &ent("IFCSTAIRFLIGHT"),
            &attr("NumberOfRisers", "42.0"),
        );
        assert!(err(&d, Schema::Ifc4).contains("integer"));
        let d = doc(
            "IFC4",
            &ent("IFCSTAIRFLIGHT"),
            &attr("NumberOfRisers", "42"),
        );
        assert!(compile(&d, Schema::Ifc4).is_ok());
        let d = doc("IFC4", &ent("IFCTASK"), &attr("IsMilestone", "FALSE"));
        assert!(err(&d, Schema::Ifc4).contains("boolean"));
        let d = doc("IFC4", &ent("IFCCARTESIANPOINT"), &attr("Coordinates", "x"));
        assert!(err(&d, Schema::Ifc4).contains("list"));
        let d = doc(
            "IFC4",
            &ent("IFCSURFACESTYLEREFRACTION"),
            &attr("RefractionIndex", "42,3"),
        );
        assert!(err(&d, Schema::Ifc4).contains("double"));
        let d = doc(
            "IFC4",
            &ent("IFCCARTESIANPOINT"),
            "<attribute><name><simpleValue>Dim</simpleValue></name></attribute>",
        );
        assert!(err(&d, Schema::Ifc4).contains("Dim"));
    }

    #[test]
    fn ids_compile_unsupported_facets_raise_or_mark() {
        let part_of = r#"<partOf><entity><name><simpleValue>IFCBUILDINGSTOREY</simpleValue></name></entity></partOf>"#;
        let d = doc("IFC4", &ent("IFCWALL"), part_of);
        match compile(&d, Schema::Ifc4) {
            Err(IdsError::Unsupported {
                feature,
                spec_index,
            }) => {
                assert_eq!(feature, "facet:part_of");
                assert_eq!(spec_index, Some(0));
            }
            other => panic!("{other:?}"),
        }
        let p = compile_with(
            &d,
            Schema::Ifc4,
            ValidateOptions {
                on_unsupported: OnUnsupported::Mark,
                filter_ifc_version: false,
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(
            matches!(&p.specs[0].state, SpecState::Unsupported { feature } if feature == "facet:part_of")
        );
    }

    fn prop(dt: &str) -> String {
        format!(
            r#"<property dataType="{dt}"><propertySet><simpleValue>P</simpleValue></propertySet><baseName><simpleValue>B</simpleValue></baseName></property>"#
        )
    }

    #[test]
    fn ids_compile_data_facets_and_datatypes() {
        let d = doc("IFC4", &ent("IFCWALL"), &prop("IFCLENGTHMEASURE"));
        let p = compile(&d, Schema::Ifc4).unwrap_or_else(|e| panic!("{e}"));
        assert!(p.needs.properties && !p.needs.classifications && !p.needs.materials);
        match &p.specs[0].state {
            SpecState::Active { requirements, .. } => match &requirements[0].facet {
                CFacet::Property(cp) => {
                    assert_eq!(cp.data_type.as_deref(), Some("IFCLENGTHMEASURE"))
                }
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
        // Not an IDS dataType at all.
        let d = doc("IFC4", &ent("IFCWALL"), &prop("IFCRABBITMEASURE"));
        assert!(err(&d, Schema::Ifc4).contains("IFCRABBITMEASURE"));
        // An IFC4 IfcValue member that IFC2X3 does not have.
        let d = doc("IFC2X3", &ent("IFCWALL"), &prop("IFCDATE"));
        assert!(err(&d, Schema::Ifc2x3).contains("schema"));
        // Classification + material facets compile and set their needs.
        let c = r#"<classification><system><simpleValue>S</simpleValue></system></classification><material/>"#;
        let d = doc("IFC4", &ent("IFCWALL"), c);
        let p = compile(&d, Schema::Ifc4).unwrap_or_else(|e| panic!("{e}"));
        assert!(!p.needs.properties && p.needs.classifications && p.needs.materials);
    }

    #[test]
    fn ids_compile_version_filter_is_opt_in() {
        let d = doc("IFC2X3", &ent("IFCWALL"), "");
        let p = compile(&d, Schema::Ifc4).unwrap_or_else(|e| panic!("{e}"));
        assert!(matches!(p.specs[0].state, SpecState::Active { .. }));
        let p = compile_with(
            &d,
            Schema::Ifc4,
            ValidateOptions {
                on_unsupported: OnUnsupported::Raise,
                filter_ifc_version: true,
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(matches!(p.specs[0].state, SpecState::SkippedIfcVersion));
    }
}
