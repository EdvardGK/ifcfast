//! Evaluation (design §2.4, §3.2): applicability, then requirements with
//! facet cardinality, then spec cardinality. Semantics follow IfcTester
//! 0.8.5 (`ifctester/ids.py:282-328`, `ifctester/facet.py`) except where
//! the IDS text is explicit and IfcTester diverges (docs/ids/ambiguities.md
//! "follow the text" rows).
//!
//! Serial for slice 1. // rayon: slice 5 — shard (spec × candidate chunk)
//! over `table.order()`; the row order contract (spec_index, step_id,
//! requirement_index) is restored by the sort the candidates already get.

use std::collections::HashMap;

use super::attrs::{AttrValue, Record};
use super::candidates::seed;
use super::compile::{CAttribute, CEntity, CFacet, CompiledSpec, Plan, SpecState};
use super::ir::{FacetCardinality, Schema, SpecCardinality};
use super::report::{card_str, reason, spec_card_str, IdsReport};
use super::restriction::Actual;
use super::schema_tables::{tables, AttrKind, SchemaTables};
use super::IdsError;
use crate::entity_table::EntityTable;

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
}

impl<'t, 'a> Ctx<'t, 'a> {
    pub fn new(table: &'t EntityTable<'a>, schema: Schema, type_map: bool) -> Ctx<'t, 'a> {
        let mut ctx = Ctx {
            table,
            schema,
            t: tables(schema),
            type_of: HashMap::new(),
            occurrences: HashMap::new(),
        };
        if type_map {
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

    fn load(&self, id: u64) -> Result<Option<Record<'t>>, IdsError> {
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
    let type_map = plans.iter().any(|p| p.needs.type_map);
    let ctx = Ctx::new(table, schema, type_map);
    let mut rep = IdsReport::default();
    let mut gidx: i32 = 0;
    for (doc_i, plan) in plans.iter().enumerate() {
        for sp in &plan.specs {
            eval_spec(&ctx, doc_i as i32, gidx, sp, &mut rep)?;
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
