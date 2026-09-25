//! Evaluation (design §2.4, §3.2): applicability, then requirements with
//! facet cardinality, then spec cardinality. Semantics follow IfcTester
//! 0.8.5 (`ifctester/ids.py:282-328`, `ifctester/facet.py`) except where
//! the IDS text is explicit and IfcTester diverges (docs/ids/ambiguities.md
//! "follow the text" rows).
//!
//! Property, classification and material facets follow
//! `docs/ids/facet-semantics-slice2.md` and the slice-2 decisions in the
//! ambiguity register (A14, A16, A26, D8).
//!
//! Serial for slices 1–2. // rayon: slice 5 — shard (spec × candidate chunk)
//! over `table.order()`; the row order contract (spec_index, step_id,
//! requirement_index) is restored by the sort the candidates already get.

use std::collections::HashMap;

use super::attrs::{py_repr_str, read_attr};
use super::attrs::{AttrValue, Record};
use super::candidates::seed;
use super::compile::{
    CAttribute, CClassification, CEntity, CFacet, CMaterial, CProperty, CompiledSpec, Needs, Plan,
    SpecState,
};
use super::datatypes::datatype_base;
use super::graph::{Graph, GraphNeeds, PropData, PropSrc, PropView};
use super::ir::XsdBase;
use super::ir::{FacetCardinality, Schema, SpecCardinality};
use super::report::{card_str, reason, spec_card_str, IdsReport};
use super::restriction::py_float_repr;
use super::restriction::Actual;
use super::schema_tables::{tables, AttrKind, SchemaTables};
use super::IdsError;
use super::OnUnsupported;
use crate::entity_table::EntityTable;
use crate::extractors::property_graph::{PropClass, RawValue, Source, TypedValue};

/// Shared evaluation context: one per `validate` call.
pub struct Ctx<'t, 'a> {
    pub table: &'t EntityTable<'a>,
    pub schema: Schema,
    pub t: &'static SchemaTables,
    /// occurrence → its type object (first `IfcRelDefinesByType` in file
    /// order; ifcopenshell `get_type` takes `IsTypedBy[0]` in IFC4 and
    /// the first `IfcRelDefinesByType` of `IsDefinedBy` in IFC2X3,
    /// `ifcopenshell/util/element.py:629-641`).
    pub type_of: HashMap<u64, u64>,
    /// type object → its occurrences, in file order.
    pub occurrences: HashMap<u64, Vec<u64>>,
    /// Property / classification / material data, built only for the
    /// facet families the plans use.
    pub graph: Graph<'t>,
}

impl<'t, 'a> Ctx<'t, 'a> {
    pub fn new(table: &'t EntityTable<'a>, schema: Schema, needs: Needs) -> Ctx<'t, 'a> {
        let mut ctx = Ctx {
            table,
            schema,
            t: tables(schema),
            type_of: HashMap::new(),
            occurrences: HashMap::new(),
            graph: Graph::build(
                table,
                GraphNeeds {
                    properties: needs.properties,
                    classifications: needs.classifications,
                    materials: needs.materials,
                },
            ),
        };
        if needs.type_map {
            ctx.build_type_map();
        }
        ctx
    }

    /// One pass over the type tokens; only `IFCRELDEFINESBYTYPE` records
    /// are split. Positions from `doc::rel_rules` (RelatedObjects 4,
    /// RelatingType 5), not re-derived.
    fn build_type_map(&mut self) {
        let Some(rule) = crate::doc::rule_for(b"IFCRELDEFINESBYTYPE") else {
            return;
        };
        for (_, ty, args) in self.table.iter() {
            if !ty.eq_ignore_ascii_case(b"IFCRELDEFINESBYTYPE") {
                continue;
            }
            let fields = crate::lexer::split_top_level_args(args);
            let related = crate::doc::field_refs(&fields, rule.anchor);
            let relating = crate::doc::field_refs(&fields, rule.pull);
            let Some(&ty_id) = relating.first() else {
                continue;
            };
            for occ in related {
                self.type_of.entry(occ).or_insert(ty_id);
                self.occurrences.entry(ty_id).or_default().push(occ);
            }
        }
    }

    /// Canonical schema class of record `id`; `None` for a missing record
    /// or a token the schema does not know.
    pub fn class_of(&self, id: u64) -> Option<&'static str> {
        let ty = self.table.type_of(id)?;
        let s = std::str::from_utf8(ty).ok()?;
        self.t.canonical(s)
    }

    pub(crate) fn load(&self, id: u64) -> Result<Option<Record<'t>>, IdsError> {
        match self.class_of(id) {
            Some(c) => Record::load(self.table, id, c).map(Some),
            None => Ok(None),
        }
    }

    /// Value of the attribute called `name` on `rec`, `None` when the
    /// class has no such attribute (Python `getattr(x, name, None)`).
    fn attr(&self, rec: &Record, name: &str) -> Result<Option<AttrValue>, IdsError> {
        match self.t.attrs(rec.class).iter().find(|a| a.name == name) {
            Some(d) => rec.attr(d.pos, d.kind).map(Some),
            None => Ok(None),
        }
    }

    fn attr_str(&self, rec: &Record, name: &str) -> Result<Option<String>, IdsError> {
        Ok(self
            .attr(rec, name)?
            .and_then(|v| v.as_str().map(str::to_string)))
    }

    /// ifcopenshell `get_type`: a type object is its own type.
    fn type_record(&self, rec: &Record<'t>) -> Result<Option<Record<'t>>, IdsError> {
        if self.t.is_type_object(rec.class) {
            return Record::load(self.table, rec.id, rec.class).map(Some);
        }
        match self.type_of.get(&rec.id) {
            Some(&tid) => self.load(tid),
            None => Ok(None),
        }
    }

    /// ifcopenshell `get_predefined_type` (`util/element.py:547-576`), as
    /// IfcTester uses it: the TYPE first (its PredefinedType; USERDEFINED
    /// or empty → ElementType, else ProcessType), used unless empty or
    /// NOTDEFINED; then the occurrence (PredefinedType; USERDEFINED or
    /// empty → ObjectType). Returns the value and where it came from.
    pub(crate) fn predefined_type(
        &self,
        rec: &Record<'t>,
    ) -> Result<(Option<String>, Option<&'static str>), IdsError> {
        if let Some(ty) = self.type_record(rec)? {
            let mut p = self.attr_str(&ty, "PredefinedType")?;
            if p.as_deref() == Some("USERDEFINED") || p.as_deref().is_none_or(str::is_empty) {
                p = self.custom_type(&ty)?;
            }
            if let Some(v) = p.filter(|v| !v.is_empty() && v != "NOTDEFINED") {
                let src = if ty.id == rec.id { "instance" } else { "type" };
                return Ok((Some(v), Some(src)));
            }
        }
        let mut p = self.attr_str(rec, "PredefinedType")?;
        if p.as_deref() == Some("USERDEFINED") || p.as_deref().is_none_or(str::is_empty) {
            p = self.attr_str(rec, "ObjectType")?;
        }
        let src = p.as_ref().map(|_| "instance");
        Ok((p, src))
    }

    /// `getattr(t, "ElementType", ...)`, falling back to `ProcessType`
    /// only when the class has no ElementType attribute at all.
    fn custom_type(&self, ty: &Record) -> Result<Option<String>, IdsError> {
        match self.attr(ty, "ElementType")? {
            Some(v) => Ok(v.as_str().map(str::to_string)),
            None => self.attr_str(ty, "ProcessType"),
        }
    }

    /// ifcopenshell `is_userdefined_type` (`util/element.py:580-611`).
    fn is_userdefined(&self, rec: &Record<'t>) -> Result<bool, IdsError> {
        if let Some(ty) = self.type_record(rec)? {
            let mut p = self.attr_str(&ty, "PredefinedType")?;
            if p.as_deref() == Some("USERDEFINED") {
                return Ok(true);
            }
            if p.as_deref().is_none_or(str::is_empty) {
                p = self.custom_type(&ty)?;
                if p.as_deref().is_some_and(|s| !s.is_empty()) {
                    return Ok(true);
                }
            }
            if p.as_deref()
                .is_some_and(|s| !s.is_empty() && s != "NOTDEFINED")
            {
                return Ok(false);
            }
        }
        let p = self.attr_str(rec, "PredefinedType")?;
        match p.as_deref() {
            Some("USERDEFINED") => Ok(true),
            None | Some("") => Ok(self
                .attr_str(rec, "ObjectType")?
                .is_some_and(|s| !s.is_empty())),
            _ => Ok(false),
        }
    }
}

/// Result of one facet on one element.
#[derive(Debug, Clone)]
pub(crate) struct Outcome {
    pub pass: bool,
    pub reason: Option<&'static str>,
    pub actual: Option<String>,
    pub source: Option<&'static str>,
}

impl Outcome {
    fn pass() -> Outcome {
        Outcome {
            pass: true,
            reason: None,
            actual: None,
            source: None,
        }
    }

    fn fail(reason: &'static str, actual: Option<String>, source: Option<&'static str>) -> Outcome {
        Outcome {
            pass: false,
            reason: Some(reason),
            actual,
            source,
        }
    }
}

/// IfcTester `Entity.__call__` (`facet.py:230-258`).
pub(crate) fn eval_entity(ctx: &Ctx, e: &CEntity, rec: &Record) -> Result<Outcome, IdsError> {
    if !e.name.matches_class(rec.class) {
        let mapped = match e.name.plain() {
            Some(n) if ctx.schema == Schema::Ifc2x3 && !n.ends_with("TYPE") => {
                ctx.type_record(rec)?
            }
            _ => None,
        };
        match (mapped, e.name.plain()) {
            (Some(ty), Some(n)) => {
                if ty.class.len() != n.len() + 4
                    || !ty.class.starts_with(n)
                    || !ty.class.ends_with("TYPE")
                {
                    let actual = ty
                        .class
                        .strip_suffix("TYPE")
                        .unwrap_or(ty.class)
                        .to_string();
                    return Ok(Outcome::fail(
                        reason::ENTITY_MISMATCH,
                        Some(actual),
                        Some("type"),
                    ));
                }
            }
            _ => {
                return Ok(Outcome::fail(
                    reason::ENTITY_MISMATCH,
                    Some(rec.class.to_string()),
                    Some("instance"),
                ))
            }
        }
    }
    let Some(p) = &e.predefined else {
        return Ok(Outcome::pass());
    };
    if p.userdefined_query {
        if ctx.is_userdefined(rec)? {
            return Ok(Outcome::pass());
        }
        let (pt, src) = ctx.predefined_type(rec)?;
        return Ok(Outcome::fail(reason::PREDEFINED_MISMATCH, pt, src));
    }
    let (pt, src) = ctx.predefined_type(rec)?;
    match &pt {
        Some(v) if p.val.matches(&Actual::Str(v.clone())) => Ok(Outcome::pass()),
        _ => Ok(Outcome::fail(reason::PREDEFINED_MISMATCH, pt, src)),
    }
}

/// IfcTester `Attribute.__call__` (`facet.py:305-388`), with one
/// deliberate divergence (ambiguity register D3): an optional facet
/// passes when every matched attribute is null (`$`/`*`), as IDS 1.0
/// says ("if the attribute has a value, it must match") and
/// `attribute/pass-an_optional_attribute_passes_if_null` pins. An empty
/// string, empty list or UNKNOWN is a written value and still fails
/// (`attribute/fail-an_optional_attribute_fails_if_empty`).
pub(crate) fn eval_attribute(
    ctx: &Ctx,
    a: &CAttribute,
    card: FacetCardinality,
    rec: &Record,
) -> Result<Outcome, IdsError> {
    let defs = ctx
        .t
        .attrs(rec.class)
        .iter()
        .filter(|d| a.name.matches(d.name));
    let mut values = Vec::new();
    for d in defs {
        values.push(rec.attr(d.pos, d.kind)?);
    }
    let base = if values.is_empty() {
        if card == FacetCardinality::Optional {
            return Ok(Outcome::pass());
        }
        Outcome::fail(reason::ATTR_MISSING, None, None)
    } else {
        let present: Vec<&AttrValue> = values.iter().filter(|v| !v.is_empty()).collect();
        if present.is_empty() {
            if card == FacetCardinality::Optional && values.iter().all(AttrValue::is_null) {
                return Ok(Outcome::pass());
            }
            let actual = if values.len() == 1 {
                values[0].py_str()
            } else {
                AttrValue::List(values.clone()).py_str()
            };
            Outcome::fail(reason::ATTR_MISSING, Some(actual), Some("instance"))
        } else {
            let mut out = Outcome::pass();
            if let Some(val) = &a.value {
                for v in &present {
                    let ok = match v.to_actual() {
                        Some(act) => val.matches(&act),
                        // entity instances, typed selects, lists
                        None => false,
                    };
                    if !ok {
                        out = Outcome::fail(
                            reason::ATTR_VALUE_MISMATCH,
                            Some(v.py_str()),
                            Some("instance"),
                        );
                        break;
                    }
                }
            }
            if out.pass {
                out.actual = Some(present[0].py_str());
                out.source = Some("instance");
            }
            out
        }
    };
    if card == FacetCardinality::Prohibited {
        return Ok(if base.pass {
            Outcome::fail(reason::PROHIBITED_PRESENT, base.actual, base.source)
        } else {
            Outcome::pass()
        });
    }
    Ok(base)
}

fn eval_facet(
    ctx: &Ctx,
    f: &CFacet,
    card: FacetCardinality,
    rec: &Record,
) -> Result<Outcome, IdsError> {
    match f {
        CFacet::Entity(e) => eval_entity(ctx, e, rec),
        CFacet::Attribute(a) => eval_attribute(ctx, a, card, rec),
        CFacet::Property(p) => eval_property(ctx, p, card, rec),
        CFacet::Classification(c) => eval_classification(ctx, c, card, rec),
        CFacet::Material(m) => eval_material(ctx, m, card, rec),
    }
}

/// Applicable step-ids of an active spec, ascending.
fn applicable(ctx: &Ctx, app: &[CFacet]) -> Result<Vec<u64>, IdsError> {
    let Some(first) = app.first() else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    'cand: for id in seed(ctx, first) {
        let Some(rec) = ctx.load(id)? else { continue };
        for (i, f) in app.iter().enumerate() {
            // The first entity facet's name was the seed; IfcTester re-checks
            // it only when it carries a predefinedType (facet.py:226-227)
            // and skips entity facets in the per-element loop (ids.py:296).
            if i == 0 {
                if let CFacet::Entity(e) = f {
                    if e.predefined.is_none() {
                        continue;
                    }
                }
            }
            if !eval_facet(ctx, f, FacetCardinality::Required, &rec)?.pass {
                continue 'cand;
            }
        }
        out.push(id);
    }
    Ok(out)
}

/// Evaluate every plan against `table`.
pub fn run(plans: &[Plan], table: &EntityTable, schema: Schema) -> Result<IdsReport, IdsError> {
    let mut needs = Needs::default();
    for p in plans {
        needs.type_map |= p.needs.type_map;
        needs.properties |= p.needs.properties;
        needs.classifications |= p.needs.classifications;
        needs.materials |= p.needs.materials;
    }
    let ctx = Ctx::new(table, schema, needs);
    let mut rep = IdsReport::default();
    let mut gidx: i32 = 0;
    for (doc_i, plan) in plans.iter().enumerate() {
        for sp in &plan.specs {
            let (n_el, n_fail) = (rep.elements.len(), rep.failures.len());
            match eval_spec(&ctx, doc_i as i32, gidx, sp, &mut rep) {
                Ok(()) => {}
                // A26: a unit a comparison needs is not declared. `mark`
                // reports the spec as unsupported (`unit:<TYPE>`) with no
                // element rows; never a fabricated pass or fail.
                Err(IdsError::UnresolvedUnit { unit_type })
                    if plan.on_unsupported == OnUnsupported::Mark =>
                {
                    rep.elements.truncate(n_el);
                    rep.failures.truncate(n_fail);
                    finish_spec(
                        &mut rep,
                        "unsupported",
                        None,
                        Some(format!("unit:{unit_type}")),
                        0,
                        0,
                        0,
                    );
                }
                Err(e) => return Err(e),
            }
            gidx += 1;
        }
    }
    Ok(rep)
}

fn eval_spec(
    ctx: &Ctx,
    doc_i: i32,
    gidx: i32,
    sp: &CompiledSpec,
    rep: &mut IdsReport,
) -> Result<(), IdsError> {
    let s = &mut rep.specs;
    s.ids_index.push(doc_i);
    s.spec_index.push(gidx);
    s.name.push(sp.name.clone());
    s.identifier.push(sp.identifier.clone());
    s.description.push(sp.description.clone());
    s.instructions.push(sp.instructions.clone());
    s.ifc_versions.push(
        sp.ifc_versions
            .iter()
            .map(|v| v.ids_token())
            .collect::<Vec<_>>()
            .join(" "),
    );
    s.cardinality.push(spec_card_str(sp.cardinality));
    s.applicability_label.push(sp.applicability_label.clone());
    s.requirement_labels.push(sp.requirement_labels.clone());

    let (applicability, requirements) = match &sp.state {
        SpecState::SkippedIfcVersion => {
            finish_spec(rep, "skipped_ifc_version", None, None, 0, 0, 0);
            return Ok(());
        }
        SpecState::Unsupported { feature } => {
            finish_spec(rep, "unsupported", None, Some(feature.clone()), 0, 0, 0);
            return Ok(());
        }
        SpecState::Active {
            applicability,
            requirements,
        } => (applicability, requirements),
    };

    let ids = applicable(ctx, applicability)?;
    let (mut passed, mut failed) = (0i64, 0i64);
    for id in &ids {
        let Some(rec) = ctx.load(*id)? else { continue };
        let guid = if ctx.t.is_root(rec.class) {
            rec.attr(0, AttrKind::String)?.as_str().map(str::to_string)
        } else {
            None
        };
        let mut n_failed: i16 = 0;
        // Prohibited specs skip requirements (ids.py:304); every
        // applicable element is the violation.
        if sp.cardinality != SpecCardinality::Prohibited {
            for (ri, r) in requirements.iter().enumerate() {
                let o = eval_facet(ctx, &r.facet, r.cardinality, &rec)?;
                if o.pass {
                    continue;
                }
                n_failed = n_failed.saturating_add(1);
                let f = &mut rep.failures;
                f.spec_index.push(gidx);
                f.step_id.push(*id as i64);
                f.guid.push(guid.clone());
                f.requirement_index.push(ri as i16);
                f.facet_type.push(r.facet.kind());
                f.facet_cardinality.push(card_str(r.cardinality));
                f.reason_code
                    .push(o.reason.unwrap_or(reason::ENTITY_MISMATCH));
                f.expected.push(r.label.clone());
                f.actual.push(o.actual);
                f.value_source.push(o.source);
            }
        }
        let el_fail = n_failed > 0 || sp.cardinality == SpecCardinality::Prohibited;
        if el_fail {
            failed += 1;
        } else {
            passed += 1;
        }
        let (pt, _) = ctx.predefined_type(&rec)?;
        let type_step_id = if ctx.t.is_type_object(rec.class) {
            None
        } else {
            ctx.type_of.get(id).map(|t| *t as i64)
        };
        let name = ctx.attr_str(&rec, "Name")?;
        let description = ctx.attr_str(&rec, "Description")?;
        let tag = ctx.attr_str(&rec, "Tag")?;
        let e = &mut rep.elements;
        e.spec_index.push(gidx);
        e.step_id.push(*id as i64);
        e.guid.push(guid);
        e.entity.push(rec.class.to_string());
        e.predefined_type.push(pt);
        e.name.push(name);
        e.description.push(description);
        e.tag.push(tag);
        e.type_step_id.push(type_step_id);
        e.status.push(if el_fail { "fail" } else { "pass" });
        e.n_failed.push(n_failed);
    }
    let n = ids.len() as i64;
    let (status, why) = match sp.cardinality {
        SpecCardinality::Required if n == 0 => ("fail", Some(reason::SPEC_NO_APPLICABLE)),
        SpecCardinality::Prohibited if n > 0 => ("fail", Some(reason::SPEC_PROHIBITED_APPLICABLE)),
        _ if failed > 0 => ("fail", None),
        _ => ("pass", None),
    };
    finish_spec(rep, status, why, None, n, passed, failed);
    Ok(())
}

fn finish_spec(
    rep: &mut IdsReport,
    status: &'static str,
    why: Option<&'static str>,
    feature: Option<String>,
    applicable: i64,
    passed: i64,
    failed: i64,
) {
    let s = &mut rep.specs;
    s.status.push(status);
    s.reason_code.push(why);
    s.unsupported_feature.push(feature);
    s.applicable.push(applicable);
    s.passed.push(passed);
    s.failed.push(failed);
}

// --------------------------------------------------------------------------
// Property facet (docs/ids/facet-semantics-slice2.md §1)
// --------------------------------------------------------------------------

fn source_str(s: Source) -> &'static str {
    match s {
        Source::Instance => "instance",
        Source::Type => "type",
    }
}

fn missing_graph(kind: &str) -> IdsError {
    IdsError::IfcInput {
        msg: format!("internal: the {kind} data layer was not built for a plan that uses it"),
    }
}

/// One comparable value of a property, before unit conversion.
#[derive(Debug, Clone)]
struct Item {
    /// The IfcValue wrapper / quantity measure / declared attribute type,
    /// UPPERCASE; `None` when the value carries none.
    wrapper: Option<String>,
    /// `None`: present but never comparable (a reference, a nested list).
    actual: Option<Actual>,
    /// The unit that applies before the project unit (the property's own).
    unit: Option<u64>,
}

/// What a property holds, as the facet reads it.
#[derive(Debug)]
enum PropValues {
    /// Complex properties / quantities, reference values and classes with
    /// no reader: never satisfy a requirement, count as absent (A16).
    Unsupported(String),
    /// Null, `''`, LOGICAL `.U.`, an empty list, a bounded value with no
    /// bound (P6, D10). Carries the Python `str` of what is there.
    Empty(String),
    /// Single value, quantity, predefined-set attribute.
    One(Item),
    /// Enumerated, list and bounded values: every present element.
    Many(Vec<Item>),
    /// Table: DefiningValues then DefinedValues.
    Table([Vec<Item>; 2]),
}

/// `TypedValue` → [`Item`]; `Err(py_str)` when the value counts as empty.
fn typed_item(tv: &TypedValue, unit: Option<u64>) -> Result<Item, String> {
    let raw = tv.raw();
    let wrapper = tv.ifc_type.map(|w| w.to_ascii_uppercase());
    let double = wrapper
        .as_deref()
        .and_then(datatype_base)
        .flatten()
        .is_some_and(|b| b == XsdBase::Double);
    let actual = match raw {
        RawValue::Null => return Err("None".into()),
        RawValue::Str(s) if s.is_empty() => return Err(String::new()),
        RawValue::Logical(None) => return Err("UNKNOWN".into()),
        RawValue::Str(s) | RawValue::Enum(s) => Some(Actual::Str(s)),
        RawValue::Real(x) => Some(Actual::Num(x)),
        RawValue::Int(i) if double => Some(Actual::Num(i as f64)),
        RawValue::Int(i) => Some(Actual::Int(i)),
        RawValue::Bool(b) | RawValue::Logical(Some(b)) => Some(Actual::Bool(b)),
        RawValue::Ref(_) | RawValue::Other(_) => None,
    };
    Ok(Item {
        wrapper,
        actual,
        unit,
    })
}

/// The present members of a list-like property.
fn items_of(values: &[TypedValue], unit: Option<u64>) -> Vec<Item> {
    values
        .iter()
        .filter_map(|tv| typed_item(tv, unit).ok())
        .collect()
}

fn extract(ctx: &Ctx, pd: &PropData, pv: &PropView) -> Result<PropValues, IdsError> {
    let d = match pv.src {
        PropSrc::Def(d) => d,
        PropSrc::Predefined {
            set,
            attr,
            declared,
        } => {
            let v = read_attr(ctx.table, set, attr.pos, attr.kind)?;
            if v.is_empty() {
                return Ok(PropValues::Empty(v.py_str()));
            }
            return Ok(match v {
                AttrValue::Typed { type_name, value } => PropValues::One(Item {
                    wrapper: Some(type_name),
                    actual: value.to_actual(),
                    unit: None,
                }),
                AttrValue::List(_) | AttrValue::Ref(_) => {
                    PropValues::Unsupported(format!("{}.{}", attr.name, "list"))
                }
                other => PropValues::One(Item {
                    wrapper: declared.map(str::to_string),
                    actual: other.to_actual(),
                    unit: None,
                }),
            });
        }
    };
    let entity = || String::from_utf8_lossy(d.entity).into_owned();
    Ok(match d.class {
        PropClass::SingleValue
        | PropClass::QuantityLength
        | PropClass::QuantityArea
        | PropClass::QuantityVolume
        | PropClass::QuantityCount
        | PropClass::QuantityWeight
        | PropClass::QuantityTime => match d.values.first() {
            None => PropValues::Empty("None".into()),
            Some(tv) => match typed_item(tv, d.unit_step) {
                Ok(i) => PropValues::One(i),
                Err(py) => PropValues::Empty(py),
            },
        },
        PropClass::EnumeratedValue | PropClass::ListValue | PropClass::BoundedValue => {
            let unit = match (d.class, d.enumeration_ref) {
                (PropClass::EnumeratedValue, Some(e)) => pd.enumeration_unit(ctx, e)?,
                (PropClass::EnumeratedValue, None) => None,
                _ => d.unit_step,
            };
            let items = items_of(&d.values, unit);
            if items.is_empty() {
                PropValues::Empty("None".into())
            } else {
                PropValues::Many(items)
            }
        }
        PropClass::TableValue => {
            let cols = [
                items_of(&d.defining_values, d.defining_unit_step),
                items_of(&d.values, d.unit_step),
            ];
            if cols.iter().all(Vec::is_empty) {
                PropValues::Empty("None".into())
            } else {
                PropValues::Table(cols)
            }
        }
        PropClass::ReferenceValue
        | PropClass::Complex
        | PropClass::ComplexQuantity
        | PropClass::UnhandledProperty
        | PropClass::UnhandledQuantity => PropValues::Unsupported(entity()),
    })
}

/// A numeric value in SI (units.md): the property's own unit when it has
/// one, else the project unit of the measure's unit type. Values whose
/// measure carries no unit type are returned as they are. A unit that
/// cannot be resolved is [`IdsError::UnresolvedUnit`] (never SI-assumed).
fn to_si(ctx: &Ctx, pd: &PropData, it: &Item) -> Result<Option<Actual>, IdsError> {
    let Some(a) = &it.actual else {
        return Ok(None);
    };
    let x = match a {
        Actual::Num(x) => *x,
        Actual::Int(i) => *i as f64,
        other => return Ok(Some(other.clone())),
    };
    let unit_type = match it
        .wrapper
        .as_deref()
        .map(|w| ctx.t.unit_type_for_measure(w))
    {
        Some(Some(Some(ut))) => ut,
        _ => return Ok(Some(a.clone())),
    };
    let resolved = match it.unit {
        Some(u) => pd.units.resolve_unit_step(u),
        None => pd.units.resolve_unit_type(unit_type),
    };
    match resolved {
        Ok(r) => Ok(Some(Actual::Num(x * r.scale + r.offset))),
        Err(_) => Err(IdsError::UnresolvedUnit {
            unit_type: unit_type.to_string(),
        }),
    }
}

fn dt_ok(it: &Item, dt: &str) -> bool {
    it.wrapper
        .as_deref()
        .is_some_and(|w| w.eq_ignore_ascii_case(dt))
}

/// dataType, then value, of one present property. `None` = it satisfies.
fn check_prop(
    ctx: &Ctx,
    pd: &PropData,
    p: &CProperty,
    pv: &PropView,
    v: &PropValues,
) -> Result<Option<Outcome>, IdsError> {
    let src = Some(source_str(pv.source));
    let (items, many): (Vec<&Item>, bool) = match v {
        PropValues::One(i) => (vec![i], false),
        PropValues::Many(v) => (v.iter().collect(), true),
        PropValues::Table(cols) => {
            // P11a: only the columns whose values carry the dataType;
            // without a dataType every column is a candidate (D9).
            let picked: Vec<&Item> = match &p.data_type {
                Some(dt) => cols
                    .iter()
                    .filter(|c| !c.is_empty() && c.iter().all(|i| dt_ok(i, dt)))
                    .flatten()
                    .collect(),
                None => cols.iter().flatten().collect(),
            };
            if picked.is_empty() {
                let found = cols
                    .iter()
                    .flatten()
                    .next()
                    .and_then(|i| i.wrapper.clone())
                    .unwrap_or_else(|| "None".into());
                return Ok(Some(Outcome::fail(
                    reason::PROP_DATATYPE_MISMATCH,
                    Some(found),
                    src,
                )));
            }
            (picked, true)
        }
        PropValues::Unsupported(_) | PropValues::Empty(_) => return Ok(None),
    };
    // P7 / A17: every present element must carry the dataType.
    if let (Some(dt), false) = (&p.data_type, matches!(v, PropValues::Table(_))) {
        if let Some(bad) = items.iter().find(|i| !dt_ok(i, dt)) {
            return Ok(Some(Outcome::fail(
                reason::PROP_DATATYPE_MISMATCH,
                Some(bad.wrapper.clone().unwrap_or_else(|| "None".into())),
                src,
            )));
        }
    }
    let Some(val) = &p.value else {
        return Ok(None);
    };
    let mut acts: Vec<Option<Actual>> = Vec::with_capacity(items.len());
    for it in &items {
        acts.push(to_si(ctx, pd, it)?);
    }
    let hit = |a: &Option<Actual>| a.as_ref().is_some_and(|a| val.matches(a));
    // D8: a restriction with bounds must hold for every value; simple
    // values, enumerations and patterns need any one (P9–P11).
    let ok = if val.has_bounds() {
        acts.iter().all(hit)
    } else {
        acts.iter().any(hit)
    };
    if ok {
        return Ok(None);
    }
    let actual = if many {
        format!(
            "[{}]",
            acts.iter()
                .map(|a| a.as_ref().map_or("None".into(), actual_py_repr))
                .collect::<Vec<_>>()
                .join(", ")
        )
    } else {
        acts[0].as_ref().map_or("None".into(), actual_py_str)
    };
    Ok(Some(Outcome::fail(
        reason::PROP_VALUE_MISMATCH,
        Some(actual),
        src,
    )))
}

/// Python `str()` of a decoded value.
fn actual_py_str(a: &Actual) -> String {
    match a {
        Actual::Str(s) => s.clone(),
        Actual::Num(x) => py_float_repr(*x),
        Actual::Int(i) => i.to_string(),
        Actual::Bool(b) => if *b { "True" } else { "False" }.into(),
        Actual::List(v) => format!(
            "[{}]",
            v.iter().map(actual_py_repr).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// Python `repr()` of a decoded value (strings quoted).
fn actual_py_repr(a: &Actual) -> String {
    match a {
        Actual::Str(s) => py_repr_str(s),
        other => actual_py_str(other),
    }
}

fn py_list(items: &[Option<&str>]) -> String {
    format!(
        "[{}]",
        items
            .iter()
            .map(|v| v.map_or("None".into(), py_repr_str))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// IfcTester `Property.__call__` (`facet.py:681-904`) with the slice-2
/// decisions: every matched set and property must satisfy (P4, P5, A15),
/// complex / reference properties are absent (A16), dataType is checked on
/// the property that supplied the value (A14), bounds hold for all values
/// (D8), values are converted to SI (P13, D5, D6).
pub(crate) fn eval_property(
    ctx: &Ctx,
    p: &CProperty,
    card: FacetCardinality,
    rec: &Record,
) -> Result<Outcome, IdsError> {
    let pd = ctx
        .graph
        .props
        .as_ref()
        .ok_or_else(|| missing_graph("property"))?;
    let psets = pd.psets_for(ctx, rec)?;
    let optional = card == FacetCardinality::Optional;
    let mut base = Outcome::pass();
    let mut matched = psets.iter().filter(|s| p.pset.matches(&s.name)).peekable();
    if matched.peek().is_none() {
        if optional {
            return Ok(Outcome::pass());
        }
        base = Outcome::fail(reason::PSET_MISSING, None, None);
    }
    'sets: for s in matched {
        let named: Vec<&PropView> = s
            .props
            .iter()
            .filter(|pv| p.base_name.matches(pv.name))
            .collect();
        let mut present: Vec<(&PropView, PropValues)> = Vec::new();
        let mut unsupported: Option<(String, Source)> = None;
        let mut empty: Option<(String, Source)> = None;
        for pv in &named {
            match extract(ctx, pd, pv)? {
                PropValues::Unsupported(e) => {
                    unsupported.get_or_insert((e, pv.source));
                }
                PropValues::Empty(py) => {
                    empty.get_or_insert((py, pv.source));
                }
                v => present.push((pv, v)),
            }
        }
        if present.is_empty() {
            if optional {
                continue;
            }
            base = match (unsupported, empty) {
                (Some((e, src)), _) => {
                    Outcome::fail(reason::PROP_UNSUPPORTED, Some(e), Some(source_str(src)))
                }
                (None, Some((py, src))) => {
                    Outcome::fail(reason::PROP_NULL, Some(py), Some(source_str(src)))
                }
                (None, None) => Outcome::fail(reason::PROP_MISSING, None, None),
            };
            break;
        }
        for (pv, v) in &present {
            if let Some(o) = check_prop(ctx, pd, p, pv, v)? {
                base = o;
                break 'sets;
            }
            if base.source.is_none() {
                base.source = Some(source_str(pv.source));
            }
        }
    }
    if card == FacetCardinality::Prohibited {
        return Ok(if base.pass {
            Outcome::fail(reason::PROHIBITED_PRESENT, base.actual, base.source)
        } else {
            Outcome::pass()
        });
    }
    Ok(base)
}

// --------------------------------------------------------------------------
// Classification facet (§2)
// --------------------------------------------------------------------------

/// IfcTester `Classification.__call__` (`facet.py:419-448`): presence,
/// then value against every reference and its ancestors (C4, C5), then
/// system against every reference's root (C2, C3, A19).
pub(crate) fn eval_classification(
    ctx: &Ctx,
    c: &CClassification,
    card: FacetCardinality,
    rec: &Record,
) -> Result<Outcome, IdsError> {
    let cd = ctx
        .graph
        .classes
        .as_ref()
        .ok_or_else(|| missing_graph("classification"))?;
    let refs = cd.refs_for(ctx, rec);
    let src = if refs.is_empty() {
        None
    } else if refs.iter().all(|r| r.source == Source::Type) {
        Some("type")
    } else {
        Some("instance")
    };
    let mut base = Outcome::pass();
    base.source = src;
    if refs.is_empty() {
        if card == FacetCardinality::Optional {
            return Ok(Outcome::pass());
        }
        base = Outcome::fail(reason::CLASS_MISSING, None, None);
    } else {
        if let Some(v) = &c.value {
            let hit = refs.iter().any(|r| {
                r.value
                    .is_some_and(|x| v.matches(&Actual::Str(x.to_string())))
            });
            if !hit {
                let vals: Vec<Option<&str>> = refs.iter().map(|r| r.value).collect();
                base = Outcome::fail(reason::CLASS_VALUE_MISMATCH, Some(py_list(&vals)), src);
            }
        }
        if base.pass {
            if let Some(sys) = &c.system {
                let systems: Vec<Option<&str>> =
                    refs.iter().filter_map(|r| r.system_known()).collect();
                let hit = systems
                    .iter()
                    .any(|s| s.is_some_and(|x| sys.matches(&Actual::Str(x.to_string()))));
                if !hit {
                    base =
                        Outcome::fail(reason::CLASS_SYSTEM_MISMATCH, Some(py_list(&systems)), src);
                }
            }
        }
    }
    if card == FacetCardinality::Prohibited {
        return Ok(if base.pass {
            Outcome::fail(reason::PROHIBITED_PRESENT, base.actual, base.source)
        } else {
            Outcome::pass()
        });
    }
    Ok(base)
}

// --------------------------------------------------------------------------
// Material facet (§3)
// --------------------------------------------------------------------------

/// IfcTester `Material.__call__` (`facet.py:946-998`): presence (M1),
/// then any candidate string (M2). The `actual` of a value failure is the
/// sorted, deduplicated set of candidates (A28).
pub(crate) fn eval_material(
    ctx: &Ctx,
    m: &CMaterial,
    card: FacetCardinality,
    rec: &Record,
) -> Result<Outcome, IdsError> {
    let md = ctx
        .graph
        .materials
        .as_ref()
        .ok_or_else(|| missing_graph("material"))?;
    let base = match md.material_of(ctx, rec) {
        None => {
            if card == FacetCardinality::Optional {
                return Ok(Outcome::pass());
            }
            Outcome::fail(reason::MATERIAL_MISSING, None, None)
        }
        Some((mat, s)) => {
            let src = Some(source_str(s));
            match &m.value {
                None => Outcome {
                    pass: true,
                    reason: None,
                    actual: None,
                    source: src,
                },
                Some(v) => {
                    let strings = md.strings(mat);
                    if strings
                        .iter()
                        .any(|x| v.matches(&Actual::Str((*x).to_string())))
                    {
                        Outcome {
                            pass: true,
                            reason: None,
                            actual: None,
                            source: src,
                        }
                    } else {
                        let set = if strings.is_empty() {
                            "set()".to_string()
                        } else {
                            format!(
                                "{{{}}}",
                                strings
                                    .iter()
                                    .map(|x| py_repr_str(x))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            )
                        };
                        Outcome::fail(reason::MATERIAL_VALUE_MISMATCH, Some(set), src)
                    }
                }
            }
        }
    };
    if card == FacetCardinality::Prohibited {
        return Ok(if base.pass {
            Outcome::fail(reason::PROHIBITED_PRESENT, base.actual, base.source)
        } else {
            Outcome::pass()
        });
    }
    Ok(base)
}
