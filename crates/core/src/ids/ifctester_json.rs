//! IfcTester-shaped JSON report (GH #192 slice 4).
//!
//! [`build`] turns an [`IdsReport`] plus the parsed IDS documents into the
//! exact structure IfcTester 0.8.5's JSON reporter emits
//! (`ifctester/reporter.py` `Json.report`, `:244-453`): top-level
//! `title` / `date` / `filepath` / `filename` / totals, `specifications`
//! with `applicability` labels and `requirements` carrying
//! `passed_entities` / `failed_entities`, every failed entity with
//! IfcTester's `reason` sentence. A tool that reads IfcTester JSON reads
//! this. One implementation serves the wheel (`IdsReport.to_ifctester_json`,
//! `ifcfast ids --json`) and the browser (`IfcModel.validateIds`).
//!
//! Deliberate differences, all additive or unavoidable:
//!
//! * `element` / `element_type` are `str(entity_instance)` in IfcTester —
//!   ifcopenshell's re-serialisation of the record. Here they are the
//!   source record, `#<id>=<IfcClass>(<args as written>)`: the same id and
//!   class, argument spelling as in the file (ifcopenshell may re-spell
//!   reals and strings).
//! * Keys prefixed `ifcfast_` are ours (`ifcfast_status` keeps the
//!   `unsupported` / `skipped_ifc_version` distinction IfcTester's
//!   `status: null` cannot, `ifcfast_reason_code` is the stable reason
//!   code of a failed entity). IfcTester readers ignore unknown keys.
//! * `date` is whatever the caller passes (the wheel: local time,
//!   IfcTester's `%Y-%m-%d %H:%M:%S`).
//!
//! Entity lists are in step-id order; IfcTester's `passed_entities` is a
//! Python set (no stable order).

use serde::ser::{Serialize, SerializeMap, SerializeSeq, Serializer};

use super::compile::CFacet;
use super::ir::{
    Facet, FacetCardinality, IdsDocument, Requirement, Restriction, Schema, Spec, SpecCardinality,
    Val,
};
use super::report::{facet_label, py_str_val, reason, spec_card_str, Clause, IdsReport};
use super::schema_tables::tables;
use crate::entity_table::EntityTable;

/// An ordered JSON value (IfcTester's key order survives serialisation;
/// `serde_json::Map` would sort it).
#[derive(Debug, Clone, PartialEq)]
pub enum J {
    Null,
    Bool(bool),
    Int(i64),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(&'static str, J)>),
}

impl Serialize for J {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            J::Null => s.serialize_unit(),
            J::Bool(b) => s.serialize_bool(*b),
            J::Int(i) => s.serialize_i64(*i),
            J::Str(v) => s.serialize_str(v),
            J::Arr(v) => {
                let mut seq = s.serialize_seq(Some(v.len()))?;
                for x in v {
                    seq.serialize_element(x)?;
                }
                seq.end()
            }
            J::Obj(v) => {
                let mut map = s.serialize_map(Some(v.len()))?;
                for (k, x) in v {
                    map.serialize_entry(k, x)?;
                }
                map.end()
            }
        }
    }
}

impl J {
    fn str(v: impl Into<String>) -> J {
        J::Str(v.into())
    }

    fn opt(v: Option<&str>) -> J {
        v.map_or(J::Null, J::str)
    }

    /// Compact JSON text.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("J serialises infallibly")
    }
}

/// Caller-supplied report metadata.
#[derive(Debug, Clone, Copy)]
pub struct JsonMeta<'m> {
    /// IfcTester `date` (`%Y-%m-%d %H:%M:%S`, local time).
    pub date: &'m str,
    /// The IFC path (`filepath`; `filename` is its basename). `None` for
    /// bytes input, as IfcTester reports a validation without `filepath`.
    pub filepath: Option<&'m str>,
    /// The header `FILE_SCHEMA` identifier as written (ifcopenshell
    /// `schema_identifier`), for `is_ifc_version`.
    pub schema_identifier: &'m str,
}

/// IfcTester's `math.floor(pass / total * 100)` or `"N/A"`.
fn percent(pass: i64, total: i64) -> J {
    if total > 0 {
        J::Int((pass * 100).div_euclid(total))
    } else {
        J::str("N/A")
    }
}

// --------------------------------------------------------------------------
// Reason sentences (facet.py:1100-1179)
// --------------------------------------------------------------------------

/// IfcTester's `FacetFailure.reason` for one failed requirement.
/// `it_kind` / `it_actual` override the reason type / printed actual where
/// the reason code alone does not decide them (see `eval::Outcome`).
pub(crate) fn reason_text(
    f: &CFacet,
    code: &str,
    actual: Option<&str>,
    it_kind: Option<&str>,
    it_actual: Option<&str>,
) -> String {
    let a = it_actual.or(actual).unwrap_or("None");
    match code {
        reason::ENTITY_MISMATCH => {
            format!("The entity class \"{a}\" does not meet the required IFC class")
        }
        reason::PREDEFINED_MISMATCH => {
            format!("The predefined type \"{a}\" does not meet the required type")
        }
        reason::ATTR_MISSING => match actual {
            None => "The required attribute did not exist".into(),
            Some(_) => format!("The attribute value \"{a}\" is empty"),
        },
        reason::ATTR_VALUE_MISMATCH => {
            format!("The attribute value \"{a}\" does not match the requirement")
        }
        reason::PSET_MISSING => "The required property set does not exist".into(),
        reason::PROP_MISSING | reason::PROP_NULL | reason::PROP_UNSUPPORTED => {
            "The property set does not contain the required property".into()
        }
        reason::PROP_DATATYPE_MISMATCH => {
            let dt = match f {
                CFacet::Property(p) => p.data_type.as_deref().unwrap_or("None"),
                _ => "None",
            };
            format!(
                "The property's data type \"{a}\" does not match the required data type of \"{dt}\""
            )
        }
        reason::PROP_VALUE_MISMATCH => {
            if it_kind == Some("VALUES") {
                format!("The property values \"{a}\" do not match the requirements")
            } else {
                format!("The property value \"{a}\" does not match the requirements")
            }
        }
        reason::CLASS_MISSING => "The entity has no classification".into(),
        reason::CLASS_VALUE_MISMATCH => {
            format!("The references \"{a}\" do not match the requirements")
        }
        reason::CLASS_SYSTEM_MISMATCH => {
            format!("The systems \"{a}\" do not match the requirements")
        }
        reason::MATERIAL_MISSING => "The entity has no material".into(),
        reason::MATERIAL_VALUE_MISMATCH => {
            format!("The material names and categories of \"{a}\" does not match the requirement")
        }
        reason::PARTOF_MISSING => "The entity has no relationship".into(),
        reason::PARTOF_ENTITY_MISMATCH => {
            if it_kind == Some("PREDEFINEDTYPE") {
                format!("The entity has a relationship with incorrect predefined type: \"{a}\"")
            } else {
                format!("The entity has a relationship with incorrect entities: \"{a}\"")
            }
        }
        reason::PROHIBITED_PRESENT => match f {
            CFacet::Attribute(_) => "The attribute value should not have met the requirement",
            CFacet::Classification(_) => "The classification should not have met the requirement",
            CFacet::PartOf(_) => "The relationship should not have met the requirement",
            CFacet::Property(_) => "The property should not have met the requirement",
            CFacet::Material(_) => "The material should not have met the requirement",
            // Entity facets carry no cardinality (never prohibited).
            CFacet::Entity(_) => {
                "The requirements were not met for some inexplicable reason. Good luck!"
            }
        }
        .into(),
        other => other.to_string(),
    }
}

// --------------------------------------------------------------------------
// Facet metadata (Facet.asdict("requirement"), facet.py:91-103)
// --------------------------------------------------------------------------

/// `Facet.to_ids_value` (facet.py:160-173).
fn ids_value(v: &Val) -> J {
    match v {
        Val::Simple(s) => J::Obj(vec![("simpleValue", J::str(s.clone()))]),
        Val::Restriction(r) => J::Obj(vec![("xs:restriction", J::Arr(vec![restriction_obj(r)]))]),
    }
}

/// `Restriction.asdict` (facet.py:1023-1045): length facets keep their
/// integer type, everything else is a string.
fn restriction_obj(r: &Restriction) -> J {
    let mut o: Vec<(&'static str, J)> =
        vec![("@base", J::str(format!("xs:{}", r.base.local_name())))];
    let vals = |v: &[String]| {
        J::Arr(
            v.iter()
                .map(|x| J::Obj(vec![("@value", J::str(x.clone()))]))
                .collect(),
        )
    };
    if let Some(en) = &r.enumeration {
        o.push(("xs:enumeration", vals(en)));
    }
    if let Some(ps) = &r.patterns {
        o.push(("xs:pattern", vals(ps)));
    }
    for (k, v) in [
        ("xs:minInclusive", &r.min_inclusive),
        ("xs:minExclusive", &r.min_exclusive),
        ("xs:maxInclusive", &r.max_inclusive),
        ("xs:maxExclusive", &r.max_exclusive),
    ] {
        if let Some(v) = v {
            o.push((k, vals(std::slice::from_ref(v))));
        }
    }
    for (k, v) in [
        ("xs:length", r.length),
        ("xs:minLength", r.min_length),
        ("xs:maxLength", r.max_length),
    ] {
        if let Some(v) = v {
            o.push((k, J::Arr(vec![J::Obj(vec![("@value", J::Int(v as i64))])])));
        }
    }
    J::Obj(o)
}

fn card_token(c: FacetCardinality) -> &'static str {
    super::report::card_str(c)
}

fn metadata(r: &Requirement) -> J {
    let mut o: Vec<(&'static str, J)> = Vec::new();
    let card = ("@cardinality", J::str(card_token(r.cardinality)));
    let instr = r
        .instructions
        .as_ref()
        .map(|i| ("@instructions", J::str(i.clone())));
    let push_opt = |o: &mut Vec<(&'static str, J)>, k: &'static str, v: Option<&Val>| {
        if let Some(v) = v {
            o.push((k, ids_value(v)));
        }
    };
    let uri = |u: &Option<String>| u.as_ref().map(|u| ("@uri", J::str(u.clone())));
    match &r.facet {
        Facet::Entity(e) => {
            o.push(("name", ids_value(&e.name)));
            push_opt(&mut o, "predefinedType", e.predefined_type.as_ref());
            o.extend(instr);
        }
        Facet::Attribute { name, value } => {
            o.push(("name", ids_value(name)));
            push_opt(&mut o, "value", value.as_ref());
            o.push(card);
            o.extend(instr);
        }
        Facet::Classification {
            system,
            value,
            uri: u,
        } => {
            push_opt(&mut o, "value", value.as_ref());
            o.push(("system", ids_value(system)));
            o.extend(uri(u));
            o.push(card);
            o.extend(instr);
        }
        Facet::PartOf { entity, relation } => {
            if let Some(rel) = relation {
                o.push(("@relation", J::str(rel.ids_token())));
            }
            o.push(card);
            o.extend(instr);
            let mut ent = vec![("name", ids_value(&entity.name))];
            push_opt(&mut ent, "predefinedType", entity.predefined_type.as_ref());
            o.push(("entity", J::Obj(ent)));
        }
        Facet::Property {
            property_set,
            base_name,
            value,
            data_type,
            uri: u,
        } => {
            o.push(("propertySet", ids_value(property_set)));
            o.push(("baseName", ids_value(base_name)));
            push_opt(&mut o, "value", value.as_ref());
            if let Some(dt) = data_type {
                o.push(("@dataType", J::str(dt.to_uppercase())));
            }
            o.extend(uri(u));
            o.push(card);
            o.extend(instr);
        }
        Facet::Material { value, uri: u } => {
            push_opt(&mut o, "value", value.as_ref());
            o.extend(uri(u));
            o.push(card);
            o.extend(instr);
        }
    }
    J::Obj(o)
}

/// Python truthiness of an optional parameter: an empty simple value is
/// falsy, a restriction object always truthy.
fn truthy(v: Option<&Val>) -> Option<String> {
    match v {
        Some(Val::Simple(s)) if s.is_empty() => None,
        other => py_str_val(other),
    }
}

/// `facet_type`, `label`, `value` (reporter.py:311-349).
fn label_value(f: &Facet) -> (&'static str, J, String) {
    let s = |v: &Val| py_str_val(Some(v)).unwrap_or_default();
    match f {
        Facet::Entity(e) => match truthy(e.predefined_type.as_ref()) {
            Some(pt) => (
                "Entity",
                J::str("IFC Class / Predefined Type"),
                format!("{}.{pt}", s(&e.name)),
            ),
            None => ("Entity", J::str("IFC Class"), s(&e.name)),
        },
        Facet::Attribute { name, value } => (
            "Attribute",
            J::str(s(name)),
            truthy(value.as_ref()).unwrap_or_default(),
        ),
        Facet::Classification { system, value, .. } => {
            match (truthy(Some(system)), truthy(value.as_ref())) {
                (Some(sy), Some(v)) => (
                    "Classification",
                    J::str("System / Reference"),
                    format!("{sy} / {v}"),
                ),
                (Some(sy), None) => ("Classification", J::str("System"), sy),
                (None, Some(v)) => ("Classification", J::str("Reference"), v),
                // IfcTester raises UnboundLocalError here; report no label.
                (None, None) => ("Classification", J::Null, String::new()),
            }
        }
        Facet::PartOf { entity, relation } => {
            let value = match truthy(entity.predefined_type.as_ref()) {
                Some(pt) => format!("{}.{pt}", s(&entity.name)),
                None => s(&entity.name),
            };
            (
                "PartOf",
                relation.map_or(J::Null, |r| J::str(r.ids_token())),
                value,
            )
        }
        Facet::Property {
            property_set,
            base_name,
            value,
            ..
        } => (
            "Property",
            J::str(format!("{}.{}", s(property_set), s(base_name))),
            truthy(value.as_ref()).unwrap_or_default(),
        ),
        Facet::Material { value, .. } => (
            "Material",
            J::str("Name / Category"),
            truthy(value.as_ref()).unwrap_or_default(),
        ),
    }
}

// --------------------------------------------------------------------------
// Entities
// --------------------------------------------------------------------------

/// `#<id>=<IfcClass>(<args>)` from the source record.
fn record_text(table: &EntityTable, id: i64) -> J {
    match table.get(id as u64) {
        Some((ty, args)) => J::Str(format!(
            "#{id}={}({})",
            crate::extractors::type_names::camel_type_name(ty),
            String::from_utf8_lossy(args)
        )),
        None => J::Null,
    }
}

/// One `ResultsEntity` (reporter.py:400-450) for elements-table row `i`.
fn entity_obj(
    rep: &IdsReport,
    table: &EntityTable,
    schema: Schema,
    i: usize,
    failure: Option<usize>,
) -> J {
    let e = &rep.elements;
    let id = e.step_id[i];
    let class = &e.entity[i];
    let element = record_text(table, id);
    let element_type = if tables(schema).is_type_object(class) {
        element.clone()
    } else {
        e.type_step_id[i].map_or(J::Null, |t| record_text(table, t))
    };
    let mut o: Vec<(&'static str, J)> = Vec::with_capacity(12);
    if let Some(f) = failure {
        o.push(("reason", J::str(rep.failures.ifctester_reason[f].clone())));
    }
    o.push(("element", element));
    o.push(("element_type", element_type));
    o.push((
        "class",
        J::Str(crate::extractors::type_names::camel_type_name(
            class.as_bytes(),
        )),
    ));
    o.push(("predefined_type", J::opt(e.predefined_type[i].as_deref())));
    o.push(("name", J::opt(e.name[i].as_deref())));
    o.push(("description", J::opt(e.description[i].as_deref())));
    o.push(("id", J::Int(id)));
    o.push(("global_id", J::opt(e.guid[i].as_deref())));
    o.push(("tag", J::opt(e.tag[i].as_deref())));
    if let Some(f) = failure {
        o.push(("ifcfast_reason_code", J::str(rep.failures.reason_code[f])));
    }
    J::Obj(o)
}

// --------------------------------------------------------------------------
// Report
// --------------------------------------------------------------------------

/// Row ranges of a column sorted by spec index.
fn ranges(spec_index: &[i32], n_specs: usize) -> Vec<std::ops::Range<usize>> {
    let mut out = vec![0..0; n_specs];
    let mut i = 0;
    while i < spec_index.len() {
        let s = spec_index[i];
        let start = i;
        while i < spec_index.len() && spec_index[i] == s {
            i += 1;
        }
        if let Some(r) = out.get_mut(s as usize) {
            *r = start..i;
        }
    }
    out
}

fn spec_obj(
    sp: &Spec,
    row: usize,
    rep: &IdsReport,
    table: &EntityTable,
    schema: Schema,
    meta: &JsonMeta,
    el: std::ops::Range<usize>,
    fl: std::ops::Range<usize>,
) -> J {
    let s = &rep.specs;
    let status = s.status[row];
    let evaluated = status == "pass" || status == "fail";
    let n = el.len() as i64;
    let n_req = sp.requirements.len();

    // Failures of this spec, keyed (step_id, requirement_index) -> row.
    let mut fail_at: std::collections::HashMap<(i64, i16), usize> =
        std::collections::HashMap::with_capacity(fl.len());
    let mut failed_el: std::collections::HashSet<i64> = std::collections::HashSet::new();
    for f in fl.clone() {
        fail_at.insert(
            (rep.failures.step_id[f], rep.failures.requirement_index[f]),
            f,
        );
        failed_el.insert(rep.failures.step_id[f]);
    }
    let prohibited = sp.cardinality == SpecCardinality::Prohibited;

    let mut total_checks = 0i64;
    let mut total_checks_pass = 0i64;
    let mut requirements = Vec::with_capacity(n_req);
    for (ri, r) in sp.requirements.iter().enumerate() {
        let mut passed = Vec::new();
        let mut failed = Vec::new();
        for i in el.clone() {
            match fail_at.get(&(rep.elements.step_id[i], ri as i16)) {
                Some(&f) => failed.push(entity_obj(rep, table, schema, i, Some(f))),
                // Prohibited specs never evaluate requirements (ids.py:304).
                None if !prohibited => passed.push(entity_obj(rep, table, schema, i, None)),
                None => {}
            }
        }
        let total_fail = failed.len() as i64;
        let total_pass = n - total_fail;
        total_checks += n;
        total_checks_pass += total_pass;
        let req_status = if !evaluated {
            J::Null
        } else {
            J::Bool(total_fail == 0 && !(sp.cardinality == SpecCardinality::Required && n == 0))
        };
        let (facet_type, label, value) = label_value(&r.facet);
        requirements.push(J::Obj(vec![
            ("facet_type", J::str(facet_type)),
            ("metadata", metadata(r)),
            ("label", label),
            ("value", J::Str(value)),
            (
                "description",
                J::Str(facet_label(
                    &r.facet,
                    Clause::Requirement,
                    r.cardinality,
                    sp.cardinality,
                )),
            ),
            ("status", req_status),
            ("passed_entities", J::Arr(passed)),
            ("failed_entities", J::Arr(failed)),
            ("total_applicable", J::Int(n)),
            ("total_pass", J::Int(total_pass)),
            ("total_fail", J::Int(total_fail)),
            ("percent_pass", percent(total_pass, n)),
        ]));
    }
    let n_failed_el = el
        .clone()
        .filter(|&i| failed_el.contains(&rep.elements.step_id[i]))
        .count() as i64;
    let total_applicable_pass = n - n_failed_el;
    let applicable_entities: Vec<J> = el
        .clone()
        .map(|i| entity_obj(rep, table, schema, i, None))
        .collect();
    let spec_status = match status {
        "pass" => J::Bool(true),
        "fail" => J::Bool(false),
        _ => J::Null,
    };
    let card = spec_card_str(sp.cardinality);
    let is_ifc_version = sp
        .ifc_versions
        .iter()
        .any(|v| v.ids_token() == meta.schema_identifier);
    J::Obj(vec![
        ("name", J::str(sp.name.clone())),
        (
            "description",
            J::str(sp.description.clone().unwrap_or_default()),
        ),
        (
            "instructions",
            J::str(sp.instructions.clone().unwrap_or_default()),
        ),
        ("status", spec_status),
        (
            "is_skipped",
            J::Bool(card == "optional" && total_checks == 0),
        ),
        ("is_ifc_version", J::Bool(is_ifc_version)),
        ("total_applicable", J::Int(n)),
        ("total_applicable_pass", J::Int(total_applicable_pass)),
        ("total_applicable_fail", J::Int(n - total_applicable_pass)),
        ("applicable_entities", J::Arr(applicable_entities)),
        ("percent_applicable_pass", percent(total_applicable_pass, n)),
        ("total_checks", J::Int(total_checks)),
        ("total_checks_pass", J::Int(total_checks_pass)),
        (
            "total_checks_fail",
            J::Int(total_checks - total_checks_pass),
        ),
        (
            "percent_checks_pass",
            percent(total_checks_pass, total_checks),
        ),
        ("cardinality", J::str(card)),
        (
            "applicability",
            J::Arr(
                sp.applicability
                    .iter()
                    .map(|f| {
                        J::Str(facet_label(
                            f,
                            Clause::Applicability,
                            FacetCardinality::Required,
                            sp.cardinality,
                        ))
                    })
                    .collect(),
            ),
        ),
        ("requirements", J::Arr(requirements)),
        ("ifcfast_spec_index", J::Int(s.spec_index[row] as i64)),
        ("ifcfast_status", J::str(status)),
        (
            "ifcfast_unsupported_feature",
            J::opt(s.unsupported_feature[row].as_deref()),
        ),
    ])
}

/// One IfcTester `Json.report()` object per IDS document of `rep`
/// (`docs[k]` is document `ids_index == k`).
pub fn build(
    docs: &[IdsDocument],
    rep: &IdsReport,
    table: &EntityTable,
    schema: Schema,
    meta: &JsonMeta,
) -> Vec<J> {
    let n_specs = rep.specs.spec_index.len();
    let el = ranges(&rep.elements.spec_index, n_specs);
    let fl = ranges(&rep.failures.spec_index, n_specs);
    let filename = meta.filepath.map(|p| {
        std::path::Path::new(p)
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    let mut row = 0usize;
    let mut out = Vec::with_capacity(docs.len());
    for (k, doc) in docs.iter().enumerate() {
        let mut specs = Vec::with_capacity(doc.specs.len());
        let (mut n_pass, mut n_req, mut n_req_pass, mut checks, mut checks_pass) =
            (0i64, 0i64, 0i64, 0i64, 0i64);
        let mut status = true;
        for sp in &doc.specs {
            debug_assert_eq!(rep.specs.ids_index[row] as usize, k);
            let obj = spec_obj(
                sp,
                row,
                rep,
                table,
                schema,
                meta,
                el[row].clone(),
                fl[row].clone(),
            );
            if let J::Obj(fields) = &obj {
                let get = |key: &str| fields.iter().find(|(k, _)| *k == key).map(|(_, v)| v);
                let ok = matches!(get("status"), Some(J::Bool(true)));
                if ok {
                    n_pass += 1;
                } else {
                    status = false;
                }
                if let Some(J::Arr(reqs)) = get("requirements") {
                    n_req += reqs.len() as i64;
                    n_req_pass += reqs
                        .iter()
                        .filter(|r| {
                            matches!(r, J::Obj(f) if f.iter().any(|(k, v)| *k == "status" && *v == J::Bool(true)))
                        })
                        .count() as i64;
                }
                if let Some(J::Int(c)) = get("total_checks") {
                    checks += c;
                }
                if let Some(J::Int(c)) = get("total_checks_pass") {
                    checks_pass += c;
                }
            }
            specs.push(obj);
            row += 1;
        }
        let n_specs_doc = doc.specs.len() as i64;
        out.push(J::Obj(vec![
            ("hide_skipped", J::Bool(false)),
            ("title", J::str(doc.info.title.clone())),
            ("date", J::str(meta.date)),
            ("filepath", J::opt(meta.filepath)),
            ("filename", J::opt(filename.as_deref())),
            ("specifications", J::Arr(specs)),
            ("status", J::Bool(status)),
            ("total_specifications", J::Int(n_specs_doc)),
            ("total_specifications_pass", J::Int(n_pass)),
            ("total_specifications_fail", J::Int(n_specs_doc - n_pass)),
            ("percent_specifications_pass", percent(n_pass, n_specs_doc)),
            ("total_requirements", J::Int(n_req)),
            ("total_requirements_pass", J::Int(n_req_pass)),
            ("total_requirements_fail", J::Int(n_req - n_req_pass)),
            ("percent_requirements_pass", percent(n_req_pass, n_req)),
            ("total_checks", J::Int(checks)),
            ("total_checks_pass", J::Int(checks_pass)),
            ("total_checks_fail", J::Int(checks - checks_pass)),
            ("percent_checks_pass", percent(checks_pass, checks)),
            ("ifcfast_ids_index", J::Int(k as i64)),
            ("ifcfast_schema", J::str(schema.ids_token())),
        ]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_json_percent_floors_and_na() {
        assert_eq!(percent(2, 3), J::Int(66));
        assert_eq!(percent(0, 0), J::str("N/A"));
        assert_eq!(percent(3, 3), J::Int(100));
    }

    #[test]
    fn ids_json_keeps_key_order() {
        let j = J::Obj(vec![
            ("b", J::Int(1)),
            ("a", J::Null),
            ("c", J::Arr(vec![J::Bool(true)])),
        ]);
        assert_eq!(j.to_json(), r#"{"b":1,"a":null,"c":[true]}"#);
    }

    #[test]
    fn ids_json_restriction_metadata_types() {
        let r = Restriction {
            enumeration: Some(vec!["A".into(), "B".into()]),
            min_length: Some(2),
            ..Restriction::default()
        };
        assert_eq!(
            ids_value(&Val::Restriction(r)).to_json(),
            r#"{"xs:restriction":[{"@base":"xs:string","xs:enumeration":[{"@value":"A"},{"@value":"B"}],"xs:minLength":[{"@value":2}]}]}"#
        );
    }
}
