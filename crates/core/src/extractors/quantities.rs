//! Element-quantity extraction.
//!
//! Walks `IfcRelDefinesByProperties` → `IfcElementQuantity` → physical
//! quantity records (Area / Length / Volume / Count / Weight / Time).
//!
//! Author-supplied quantities are the gold-standard QTO source — more
//! accurate than geometry-derived numbers because authors use the
//! intended measurement convention (e.g. "GrossArea" vs "NetArea" for
//! a wall, with openings subtracted on the latter).
//!
//! Long-format output:
//!     (guid, qto_name, quantity_name, value, quantity_type, unit, source)
//!
//! Where:
//!   `qto_name`       = e.g. "Qto_WallBaseQuantities"
//!   `quantity_name`  = e.g. "NetArea", "Length", "GrossVolume"
//!   `value`          = numeric value as string (downstream parses to f64)
//!   `quantity_type`  = "Area" | "Length" | "Volume" | "Count" |
//!                      "Weight" | "Time"
//!   `unit`           = step_id of the IfcUnit (often null; the project's
//!                      default unit assignment applies)
//!   `source`         = "instance" (declared directly on the product via
//!                      IfcRelDefinesByProperties) or "type" (inherited
//!                      from IfcRelDefinesByType → RelatingType's
//!                      HasPropertySets). Mirrors `psets.source`. The
//!                      schema's HasPropertySets slot accepts ANY
//!                      IfcPropertySetDefinition — IfcElementQuantity
//!                      is one such subtype — so types can carry
//!                      authored quantities exactly like psets.

use std::collections::HashMap;

use crate::entity_table::EntityTable;
use crate::lexer::{parse_field, split_top_level_args, Field};

#[derive(Debug, Default)]
pub struct QuantityTable {
    pub guid: Vec<String>,
    pub qto_name: Vec<String>,
    pub quantity_name: Vec<String>,
    pub value: Vec<Option<String>>,
    pub quantity_type: Vec<String>,
    pub unit_step_id: Vec<Option<u64>>,
    pub source: Vec<String>,
}

impl QuantityTable {
    pub fn len(&self) -> usize {
        self.guid.len()
    }

    pub fn is_empty(&self) -> bool {
        self.guid.is_empty()
    }
}

pub fn build(
    table: &EntityTable,
    product_step_to_guid: &HashMap<u64, String>,
) -> QuantityTable {
    // Pass 1: index IfcElementQuantity records and their physical quantity refs.
    //         Also index each physical-simple-quantity record.
    let mut qtos: HashMap<u64, (String, Vec<u64>)> = HashMap::with_capacity(1024);
    let mut quantities: HashMap<u64, Quantity> = HashMap::with_capacity(8192);
    let mut rel_pairs: Vec<(u64, u64)> = Vec::with_capacity(8192);
    // Project-default unit fallback (GH #43). Real-world Revit/ArchiCAD
    // exports almost always leave the per-quantity `Unit` slot as `$`
    // and rely on the IfcUnitAssignment to disambiguate. Without this
    // map every `unit_step_id` column on `m.quantities` is None, which
    // forces consumers either to re-parse the IFC for units or assume
    // a fixed interpretation (the canonical "are these volumes in m3
    // or mm3?" trap). Resolution path is intentionally narrow: walk
    // the file's IfcUnitAssignment refs, intersect with IfcSIUnit
    // records, and read their UnitType enum. IfcConversionBasedUnit
    // and IfcDerivedUnit are out of scope here — they're a separate
    // resolver because conversion factors have to be threaded too.
    let mut project_unit_refs: std::collections::HashSet<u64> =
        std::collections::HashSet::with_capacity(16);
    // `unit_type` is the raw enum body without dots, uppercased
    // (e.g. `LENGTHUNIT`, `AREAUNIT`). Matched against the
    // `quantity_kind → unit_type` table below.
    let mut si_unit_by_type: HashMap<String, u64> = HashMap::with_capacity(16);
    // Type inheritance (GH #45) — mirror of the GH #36 path in
    // extractors/psets.rs. Types can carry quantities the same way
    // they carry psets because `IfcTypeObject.HasPropertySets` is
    // typed `SET OF IfcPropertySetDefinition` and IfcElementQuantity
    // IS-A IfcPropertySetDefinition. Captured here so type-attached
    // quantities surface on every instance bound via
    // IfcRelDefinesByType.
    let mut product_to_type: HashMap<u64, u64> = HashMap::with_capacity(16_384);
    // (type_step_id → [qto_step_id]). We re-use the same arg-5 read
    // psets.rs does (HasPropertySets is at attribute 6 / index 5 on
    // IfcTypeObject and every subtype); the `qtos` map captures
    // step ids regardless of which `IfcPropertySetDefinition` subtype
    // a ref points at, so pset refs that slip in here just don't
    // resolve in pass 2 and get skipped silently.
    let mut type_qtos: HashMap<u64, Vec<u64>> = HashMap::with_capacity(256);

    for (step_id, type_name, args) in table.iter() {
        if type_name.eq_ignore_ascii_case(b"IFCELEMENTQUANTITY") {
            // (GlobalId, OwnerHistory, Name, Description, MethodOfMeasurement, Quantities)
            let fields = split_top_level_args(args);
            let name = string_at(&fields, 2).unwrap_or_default();
            let qty_ids = ref_list_at(&fields, 5);
            qtos.insert(step_id, (name, qty_ids));
        } else if let Some(kind) = quantity_kind(type_name) {
            // IfcQuantityArea/Length/Volume/Count/Weight/Time
            // (Name, Description, Unit, <kind>Value)
            let fields = split_top_level_args(args);
            let name = string_at(&fields, 0).unwrap_or_default();
            let unit = match fields.get(2).copied().map(parse_field) {
                Some(Field::Ref(id)) => Some(id),
                _ => None,
            };
            let value = number_at(&fields, 3).map(format_number);
            quantities.insert(
                step_id,
                Quantity { name, kind, value, unit_step_id: unit },
            );
        } else if type_name.eq_ignore_ascii_case(b"IFCRELDEFINESBYPROPERTIES") {
            let fields = split_top_level_args(args);
            let rel_obj = match fields.get(5).copied().map(parse_field) {
                Some(Field::Ref(id)) => id,
                _ => continue,
            };
            let relateds = match fields.get(4).copied().map(parse_field) {
                Some(Field::List(body)) => parse_ref_list(body),
                Some(Field::Ref(id)) => vec![id],
                _ => continue,
            };
            for obj_id in relateds {
                rel_pairs.push((obj_id, rel_obj));
            }
        } else if type_name.eq_ignore_ascii_case(b"IFCRELDEFINESBYTYPE") {
            // (GlobalId, OwnerHistory, Name, Description,
            //  RelatedObjects, RelatingType). Same shape as the
            // properties variant — fan out N products per relation.
            let fields = split_top_level_args(args);
            let type_id = match fields.get(5).copied().map(parse_field) {
                Some(Field::Ref(id)) => id,
                _ => continue,
            };
            let relateds = match fields.get(4).copied().map(parse_field) {
                Some(Field::List(body)) => parse_ref_list(body),
                Some(Field::Ref(id)) => vec![id],
                _ => continue,
            };
            for obj_id in relateds {
                product_to_type.insert(obj_id, type_id);
            }
        } else if is_type_object(type_name) {
            // IfcTypeObject + every IfcXxxType subclass. Attribute 6
            // (index 5) is HasPropertySets on IfcTypeObject and every
            // subtype — same positional read psets.rs uses. The list
            // may contain IfcPropertySet refs (pset side, handled by
            // extractors/psets.rs) AND IfcElementQuantity refs (qto
            // side, handled here). We capture every ref; pass 2's
            // `qtos.get(qto_id)` filters to the ones that resolve.
            let fields = split_top_level_args(args);
            let candidate_ids = ref_list_at(&fields, 5);
            if !candidate_ids.is_empty() {
                type_qtos.insert(step_id, candidate_ids);
            }
        } else if type_name.eq_ignore_ascii_case(b"IFCUNITASSIGNMENT") {
            // (Units : SET [1:?] OF IfcUnit). One arg, a list of refs.
            // Most files have exactly one IfcUnitAssignment; legal but
            // unusual files may have several. We union them all — any
            // SIUnit reachable from any assignment counts as a
            // project default candidate.
            let fields = split_top_level_args(args);
            for unit_id in ref_list_at(&fields, 0) {
                project_unit_refs.insert(unit_id);
            }
        } else if type_name.eq_ignore_ascii_case(b"IFCSIUNIT") {
            // (Dimensions, UnitType, Prefix, Name). Slot 1 is the
            // UnitType enum (`.LENGTHUNIT.`, `.AREAUNIT.`, …). Capture
            // every SIUnit's type here; project-membership filtering
            // happens after pass 1 so we don't have to revisit the
            // table once we know which units are in `project_unit_refs`.
            let fields = split_top_level_args(args);
            if let Some(raw) = fields.get(1).copied() {
                if let Some(unit_type) = parse_enum_uppercase(raw) {
                    // last-write-wins on collisions. A well-formed
                    // file has at most one SIUnit per UnitType.
                    si_unit_by_type.insert(unit_type, step_id);
                }
            }
        }
    }

    // Build the (UnitType → step_id) project-default map. Only SIUnits
    // referenced by an IfcUnitAssignment count — a stray SIUnit
    // declared inside an IfcConversionBasedUnit shouldn't masquerade
    // as a project default.
    let mut project_default_unit: HashMap<&'static str, u64> = HashMap::with_capacity(8);
    if !project_unit_refs.is_empty() {
        for (unit_type, step_id) in &si_unit_by_type {
            if !project_unit_refs.contains(step_id) {
                continue;
            }
            if let Some(canonical) = canonical_unit_type(unit_type) {
                project_default_unit.insert(canonical, *step_id);
            }
        }
    }

    let mut out = QuantityTable::default();

    // (guid → set of "qto_name\tquantity_name" keys already emitted
    // from the instance side). Used to suppress same-named type
    // quantities on collision so instance values win, matching the
    // ifcopenshell `should_inherit=True` semantics replicated in
    // extractors/psets.rs (GH #36 / #45).
    let mut seen_per_product: HashMap<&str, std::collections::HashSet<String>> =
        HashMap::with_capacity(product_step_to_guid.len());

    // Instance pass.
    for (obj_step_id, rel_obj_id) in &rel_pairs {
        // Only act on rels that point at IfcElementQuantity (the same
        // rel type also points at IfcPropertySet — we filter via the
        // qtos table).
        let (qto_name, qty_ids) = match qtos.get(rel_obj_id) {
            Some(x) => x,
            None => continue,
        };
        let guid = match product_step_to_guid.get(obj_step_id) {
            Some(g) => g.as_str(),
            None => continue,
        };
        for qid in qty_ids {
            if let Some(name) = emit_quantity(
                qid,
                guid,
                qto_name,
                "instance",
                &quantities,
                &project_default_unit,
                &mut out,
            ) {
                seen_per_product
                    .entry(guid)
                    .or_default()
                    .insert(format!("{qto_name}\t{name}"));
            }
        }
    }

    // Type-inheritance pass. Quantity inheritance is silent unless
    // BOTH there's a product↔type relation AND at least one type
    // carries quantity refs.
    if !type_qtos.is_empty() && !product_to_type.is_empty() {
        for (product_step_id, type_step_id) in &product_to_type {
            let guid = match product_step_to_guid.get(product_step_id) {
                Some(g) => g.as_str(),
                None => continue,
            };
            let candidate_qto_ids = match type_qtos.get(type_step_id) {
                Some(v) => v,
                None => continue,
            };
            let empty = std::collections::HashSet::new();
            let already_seen = seen_per_product.get(guid).unwrap_or(&empty);
            for qto_id in candidate_qto_ids {
                // Same filter as the instance side: a qto_id from
                // HasPropertySets that doesn't resolve in `qtos` is
                // an IfcPropertySet ref, handled by extractors/psets.rs.
                let (qto_name, qty_ids) = match qtos.get(qto_id) {
                    Some(x) => x,
                    None => continue,
                };
                for qid in qty_ids {
                    emit_quantity_dedup(
                        qid,
                        guid,
                        qto_name,
                        "type",
                        &quantities,
                        &project_default_unit,
                        &mut out,
                        already_seen,
                    );
                }
            }
        }
    }

    out
}

/// Append one quantity row to `out`. Returns the resolved quantity
/// name (the dedup-set key, sans qto_name prefix) when the row is
/// actually written so the caller can stamp `seen_per_product` for the
/// type-inheritance pass.
fn emit_quantity(
    qid: &u64,
    guid: &str,
    qto_name: &str,
    source: &str,
    quantities: &HashMap<u64, Quantity>,
    project_default_unit: &HashMap<&'static str, u64>,
    out: &mut QuantityTable,
) -> Option<String> {
    let q = quantities.get(qid)?;
    let unit = q.unit_step_id.or_else(|| {
        unit_type_for_quantity_kind(q.kind)
            .and_then(|ut| project_default_unit.get(ut).copied())
    });
    out.guid.push(guid.to_string());
    out.qto_name.push(qto_name.to_string());
    out.quantity_name.push(q.name.clone());
    out.value.push(q.value.clone());
    out.quantity_type.push(q.kind.to_string());
    out.unit_step_id.push(unit);
    out.source.push(source.to_string());
    Some(q.name.clone())
}

/// Dedup-aware variant. Skips emit when the instance side already
/// surfaced a row at `(qto_name, q.name)`. Same shape contract as the
/// pset-side `emit_property_dedup` — instance wins on collision.
#[allow(clippy::too_many_arguments)]
fn emit_quantity_dedup(
    qid: &u64,
    guid: &str,
    qto_name: &str,
    source: &str,
    quantities: &HashMap<u64, Quantity>,
    project_default_unit: &HashMap<&'static str, u64>,
    out: &mut QuantityTable,
    already_seen: &std::collections::HashSet<String>,
) {
    let q = match quantities.get(qid) {
        Some(q) => q,
        None => return,
    };
    let key = format!("{qto_name}\t{}", q.name);
    if already_seen.contains(&key) {
        return;
    }
    let unit = q.unit_step_id.or_else(|| {
        unit_type_for_quantity_kind(q.kind)
            .and_then(|ut| project_default_unit.get(ut).copied())
    });
    out.guid.push(guid.to_string());
    out.qto_name.push(qto_name.to_string());
    out.quantity_name.push(q.name.clone());
    out.value.push(q.value.clone());
    out.quantity_type.push(q.kind.to_string());
    out.unit_step_id.push(unit);
    out.source.push(source.to_string());
}

/// Detect an IfcTypeObject / IfcXxxType subclass by entity-name
/// suffix. Mirrors the identical rule in `extractors/psets.rs` and
/// `indexer::index` so the three loops agree on what counts as a
/// type. The IFC2x3 collapsed `IfcDoorStyle` / `IfcWindowStyle`
/// classes don't follow the `*Type` suffix but ARE valid
/// `IfcRelDefinesByType.RelatingType` targets on 2x3 files.
fn is_type_object(t: &[u8]) -> bool {
    let suffix_ok = t.len() > 7
        && t[..3].eq_ignore_ascii_case(b"IFC")
        && t[t.len() - 4..].eq_ignore_ascii_case(b"TYPE");
    let ifc2x3_style = t.eq_ignore_ascii_case(b"IFCDOORSTYLE")
        || t.eq_ignore_ascii_case(b"IFCWINDOWSTYLE");
    suffix_ok || ifc2x3_style
}

struct Quantity {
    name: String,
    kind: &'static str,
    value: Option<String>,
    unit_step_id: Option<u64>,
}

fn quantity_kind(type_name: &[u8]) -> Option<&'static str> {
    if type_name.eq_ignore_ascii_case(b"IFCQUANTITYAREA") {
        Some("Area")
    } else if type_name.eq_ignore_ascii_case(b"IFCQUANTITYLENGTH") {
        Some("Length")
    } else if type_name.eq_ignore_ascii_case(b"IFCQUANTITYVOLUME") {
        Some("Volume")
    } else if type_name.eq_ignore_ascii_case(b"IFCQUANTITYCOUNT") {
        Some("Count")
    } else if type_name.eq_ignore_ascii_case(b"IFCQUANTITYWEIGHT") {
        Some("Weight")
    } else if type_name.eq_ignore_ascii_case(b"IFCQUANTITYTIME") {
        Some("Time")
    } else {
        None
    }
}

/// Map a `quantity_type` string (the column written to parquet) back
/// to the canonical `IfcUnitEnum` literal used by `IfcSIUnit.UnitType`.
/// `Count` is dimensionless — it has no fallback target, so a null
/// `unit_step_id` is the correct, terminal answer.
fn unit_type_for_quantity_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "Length" => Some("LENGTHUNIT"),
        "Area" => Some("AREAUNIT"),
        "Volume" => Some("VOLUMEUNIT"),
        "Weight" => Some("MASSUNIT"),
        "Time" => Some("TIMEUNIT"),
        _ => None,
    }
}

/// Pin every `IfcUnitEnum` literal we accept as a fallback target to
/// a `&'static str` so the project-default map keys don't carry
/// allocations. Anything outside this set is irrelevant to quantity
/// resolution and is intentionally dropped.
fn canonical_unit_type(uppercase: &str) -> Option<&'static str> {
    match uppercase {
        "LENGTHUNIT" => Some("LENGTHUNIT"),
        "AREAUNIT" => Some("AREAUNIT"),
        "VOLUMEUNIT" => Some("VOLUMEUNIT"),
        "MASSUNIT" => Some("MASSUNIT"),
        "TIMEUNIT" => Some("TIMEUNIT"),
        _ => None,
    }
}

/// Parse a STEP enum field (`.LENGTHUNIT.` → `"LENGTHUNIT"`). Strips
/// surrounding dots, uppercases, returns None on shapes that don't
/// match the enum-literal pattern.
fn parse_enum_uppercase(raw: &[u8]) -> Option<String> {
    let trimmed = trim_bytes(raw);
    if trimmed.len() < 2 {
        return None;
    }
    if trimmed.first() != Some(&b'.') || trimmed.last() != Some(&b'.') {
        return None;
    }
    let inner = &trimmed[1..trimmed.len() - 1];
    let s = std::str::from_utf8(inner).ok()?;
    Some(s.to_ascii_uppercase())
}

fn trim_bytes(s: &[u8]) -> &[u8] {
    let mut start = 0;
    while start < s.len() && (s[start] as char).is_whitespace() {
        start += 1;
    }
    let mut end = s.len();
    while end > start && (s[end - 1] as char).is_whitespace() {
        end -= 1;
    }
    &s[start..end]
}

fn format_number(n: f64) -> String {
    if n.fract() == 0.0 && n.is_finite() && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{}", n)
    }
}

fn string_at(fields: &[&[u8]], idx: usize) -> Option<String> {
    match parse_field(fields.get(idx)?) {
        Field::String(s) => Some(s),
        _ => None,
    }
}

fn number_at(fields: &[&[u8]], idx: usize) -> Option<f64> {
    match parse_field(fields.get(idx)?) {
        Field::Number(n) => Some(n),
        _ => None,
    }
}

fn ref_list_at(fields: &[&[u8]], idx: usize) -> Vec<u64> {
    match fields.get(idx).copied().map(parse_field) {
        Some(Field::List(body)) => parse_ref_list(body),
        _ => Vec::new(),
    }
}

fn parse_ref_list(body: &[u8]) -> Vec<u64> {
    split_top_level_args(body)
        .into_iter()
        .filter_map(|f| match parse_field(f) {
            Field::Ref(id) => Some(id),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_buf(extra_data: &str) -> String {
        format!(
            r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('qto_test.ifc','2026-05-26T00:00:00',('test'),('test'),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCPROJECT('0Test000000000000000001',$,'p',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#3));
#3=IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.);
#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);
#6=IFCAXIS2PLACEMENT3D(#7,$,$);
#7=IFCCARTESIANPOINT((0.,0.,0.));
#10=IFCWALL('1Wall00000000000000001',$,'W',$,$,$,$,'t',.STANDARD.);
{extra_data}
ENDSEC;
END-ISO-10303-21;
"#
        )
    }

    fn run(buf: &str) -> QuantityTable {
        let table = crate::entity_table::EntityTable::build(buf.as_bytes());
        let mut step_to_guid: HashMap<u64, String> = HashMap::new();
        for (sid, _t, args) in table.iter() {
            let fields = split_top_level_args(args);
            if let Some(first) = fields.first() {
                if let Field::String(s) = parse_field(first) {
                    if s.len() == 22 {
                        step_to_guid.insert(sid, s);
                    }
                }
            }
        }
        build(&table, &step_to_guid)
    }

    #[test]
    fn area_volume_length_count_all_classify() {
        // One IfcElementQuantity bundling all four common quantity types.
        // Verifies the kind dispatch (`IFCQUANTITYAREA` → "Area", etc.)
        // and that each physical-quantity arg layout (Name, Desc, Unit,
        // Value at index 3) parses correctly.
        let buf = make_buf(
            r#"
#20=IFCQUANTITYAREA('NetArea',$,$,12.5);
#21=IFCQUANTITYVOLUME('NetVolume',$,$,2.5);
#22=IFCQUANTITYLENGTH('Length',$,$,5.0);
#23=IFCQUANTITYCOUNT('Count',$,$,1.);
#24=IFCELEMENTQUANTITY('2Qto000000000000000001',$,'Qto_WallBaseQuantities',$,$,(#20,#21,#22,#23));
#25=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#10),#24);
"#,
        );
        let t = run(&buf);
        assert_eq!(t.len(), 4, "expected 4 quantity rows, got {}", t.len());

        let by_name: std::collections::HashMap<&str, (&str, Option<&str>)> = t
            .quantity_name
            .iter()
            .enumerate()
            .map(|(i, n)| {
                (
                    n.as_str(),
                    (t.quantity_type[i].as_str(), t.value[i].as_deref()),
                )
            })
            .collect();
        // Whole-number scalars normalise to their integer string form
        // (`5.0` → `"5"`) — see `format_number`. Decimals keep their
        // fractional part.
        assert_eq!(by_name.get("NetArea"), Some(&("Area", Some("12.5"))));
        assert_eq!(by_name.get("NetVolume"), Some(&("Volume", Some("2.5"))));
        assert_eq!(by_name.get("Length"), Some(&("Length", Some("5"))));
        assert_eq!(by_name.get("Count"), Some(&("Count", Some("1"))));

        // All rows must point back to the same product and qto.
        for i in 0..t.len() {
            assert_eq!(t.guid[i], "1Wall00000000000000001");
            assert_eq!(t.qto_name[i], "Qto_WallBaseQuantities");
        }
    }

    #[test]
    fn unit_ref_threaded_through_to_unit_step_id() {
        // When IfcQuantity*.Unit is set, the resolved IfcUnit step_id
        // must surface on the row. Pre-fix any nuance about ratio /
        // derived units would have been lost; this test pins the
        // contract.
        let buf = make_buf(
            r#"
#30=IFCSIUNIT(*,.AREAUNIT.,$,.SQUARE_METRE.);
#20=IFCQUANTITYAREA('Area',$,#30,42.0);
#21=IFCELEMENTQUANTITY('2Qto000000000000000001',$,'Qto_X',$,$,(#20));
#22=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#10),#21);
"#,
        );
        let t = run(&buf);
        assert_eq!(t.len(), 1);
        assert!(
            t.unit_step_id[0].is_some(),
            "expected unit_step_id to resolve, got None"
        );
    }

    #[test]
    fn missing_value_produces_null_value() {
        // Quantity declared with `$` for its scalar; row should still
        // exist but value column is None.
        let buf = make_buf(
            r#"
#20=IFCQUANTITYAREA('Area',$,$,$);
#21=IFCELEMENTQUANTITY('2Qto000000000000000001',$,'Qto_X',$,$,(#20));
#22=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#10),#21);
"#,
        );
        let t = run(&buf);
        assert_eq!(t.len(), 1);
        assert_eq!(t.value[0], None);
        assert_eq!(t.quantity_type[0], "Area");
    }

    /// Build a synthetic fixture that declares an IfcUnitAssignment
    /// with the five physical SIUnits relevant to quantity fallback
    /// (length / area / volume / mass / time). `extra_data` adds the
    /// quantity records under test.
    fn make_buf_with_units(extra_data: &str) -> String {
        format!(
            r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('qto_test.ifc','2026-06-02T00:00:00',('test'),('test'),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCPROJECT('0Test000000000000000001',$,'p',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#3,#90,#91,#92,#93));
#3=IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.);
#90=IFCSIUNIT(*,.AREAUNIT.,$,.SQUARE_METRE.);
#91=IFCSIUNIT(*,.VOLUMEUNIT.,$,.CUBIC_METRE.);
#92=IFCSIUNIT(*,.MASSUNIT.,.KILO.,.GRAM.);
#93=IFCSIUNIT(*,.TIMEUNIT.,$,.SECOND.);
#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);
#6=IFCAXIS2PLACEMENT3D(#7,$,$);
#7=IFCCARTESIANPOINT((0.,0.,0.));
#10=IFCWALL('1Wall00000000000000001',$,'W',$,$,$,$,'t',.STANDARD.);
{extra_data}
ENDSEC;
END-ISO-10303-21;
"#
        )
    }

    #[test]
    fn null_quantity_unit_falls_back_to_project_default_per_kind() {
        // GH #43 baseline: every quantity declares `$` for Unit. The
        // project's IfcUnitAssignment defines one SIUnit per
        // UnitType; each quantity row's `unit_step_id` should
        // resolve to the matching SIUnit's step_id.
        //
        // Mapping under test:
        //   Length → #3   (LENGTHUNIT)
        //   Area   → #90  (AREAUNIT)
        //   Volume → #91  (VOLUMEUNIT)
        //   Weight → #92  (MASSUNIT)
        //   Time   → #93  (TIMEUNIT)
        //   Count  → None (dimensionless, no fallback)
        let buf = make_buf_with_units(
            r#"
#41=IFCQUANTITYLENGTH('Length',$,$,3.0);
#42=IFCQUANTITYAREA('NetArea',$,$,7.5);
#43=IFCQUANTITYVOLUME('Volume',$,$,1.275);
#44=IFCQUANTITYWEIGHT('Weight',$,$,3060.);
#45=IFCQUANTITYTIME('InstallSeconds',$,$,1800.);
#46=IFCQUANTITYCOUNT('Bolts',$,$,12.);
#47=IFCELEMENTQUANTITY('2Qto000000000000000001',$,'Qto_All',$,$,(#41,#42,#43,#44,#45,#46));
#48=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#10),#47);
"#,
        );
        let t = run(&buf);
        assert_eq!(t.len(), 6);
        let by_name: std::collections::HashMap<&str, (&str, Option<u64>)> = (0..t.len())
            .map(|i| {
                (
                    t.quantity_name[i].as_str(),
                    (t.quantity_type[i].as_str(), t.unit_step_id[i]),
                )
            })
            .collect();
        assert_eq!(by_name.get("Length"), Some(&("Length", Some(3))));
        assert_eq!(by_name.get("NetArea"), Some(&("Area", Some(90))));
        assert_eq!(by_name.get("Volume"), Some(&("Volume", Some(91))));
        assert_eq!(by_name.get("Weight"), Some(&("Weight", Some(92))));
        assert_eq!(by_name.get("InstallSeconds"), Some(&("Time", Some(93))));
        // Count is dimensionless — no SIUnit fallback target exists
        // even though the project declares LENGTHUNIT etc.
        assert_eq!(by_name.get("Bolts"), Some(&("Count", None)));
    }

    #[test]
    fn explicit_unit_overrides_project_default_fallback() {
        // When IfcQuantity*.Unit is explicitly set, the fallback
        // doesn't fire — the quantity's own ref wins, even if the
        // project's IfcUnitAssignment has a different SIUnit for
        // the same UnitType.
        let buf = make_buf_with_units(
            r#"
#80=IFCSIUNIT(*,.AREAUNIT.,$,.SQUARE_METRE.);
#41=IFCQUANTITYAREA('NetArea',$,#80,7.5);
#47=IFCELEMENTQUANTITY('2Qto000000000000000001',$,'Qto_X',$,$,(#41));
#48=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#10),#47);
"#,
        );
        let t = run(&buf);
        assert_eq!(t.len(), 1);
        // The quantity's own #80 ref must win, not the project's #90.
        assert_eq!(t.unit_step_id[0], Some(80));
    }

    #[test]
    fn fallback_is_silent_when_project_has_no_matching_unit() {
        // File declares LENGTHUNIT but no MASSUNIT in the
        // IfcUnitAssignment. A QuantityWeight with null Unit has no
        // legitimate fallback target — stay None instead of
        // misattributing.
        let buf = format!(
            r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('qto_test.ifc','2026-06-02T00:00:00',('test'),('test'),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCPROJECT('0Test000000000000000001',$,'p',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#3));
#3=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);
#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);
#6=IFCAXIS2PLACEMENT3D(#7,$,$);
#7=IFCCARTESIANPOINT((0.,0.,0.));
#10=IFCWALL('1Wall00000000000000001',$,'W',$,$,$,$,'t',.STANDARD.);
#41=IFCQUANTITYWEIGHT('Weight',$,$,42.);
#47=IFCELEMENTQUANTITY('2Qto000000000000000001',$,'Qto_X',$,$,(#41));
#48=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#10),#47);
ENDSEC;
END-ISO-10303-21;
"#
        );
        let t = run(&buf);
        assert_eq!(t.len(), 1);
        assert_eq!(t.unit_step_id[0], None);
    }

    #[test]
    fn instance_quantity_rows_are_marked_source_instance() {
        // Baseline: existing-path rows now carry source="instance".
        let buf = make_buf(
            r#"
#20=IFCQUANTITYAREA('NetArea',$,$,12.5);
#21=IFCELEMENTQUANTITY('2Qto000000000000000001',$,'Qto_X',$,$,(#20));
#22=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#10),#21);
"#,
        );
        let t = run(&buf);
        assert_eq!(t.len(), 1);
        assert_eq!(t.source[0], "instance");
    }

    #[test]
    fn type_attached_quantity_surfaces_on_instance_with_source_type() {
        // GH #45 — quantity-side equivalent of GH #36. IfcElementQuantity
        // sitting in IfcTypeObject.HasPropertySets must inherit to every
        // related instance, tagged source="type". Real-world payload:
        // component-library exports stamp Qto_xxxBaseQuantities on the
        // type so all 200 occurrences share one numeric record.
        let buf = make_buf(
            r#"
#30=IFCQUANTITYWEIGHT('GrossWeight',$,$,42.5);
#31=IFCELEMENTQUANTITY('4Qto0000000000000001',$,'Qto_TypeBase',$,$,(#30));
#32=IFCWALLTYPE('5Type0000000000000001',$,'200mm Concrete',$,$,(#31),$,$,$,.STANDARD.);
#33=IFCRELDEFINESBYTYPE('6RelType000000000000001',$,$,$,(#10),#32);
"#,
        );
        let t = run(&buf);
        assert_eq!(t.len(), 1, "expected one inherited quantity, got {}", t.len());
        assert_eq!(t.guid[0], "1Wall00000000000000001");
        assert_eq!(t.qto_name[0], "Qto_TypeBase");
        assert_eq!(t.quantity_name[0], "GrossWeight");
        assert_eq!(t.value[0].as_deref(), Some("42.5"));
        assert_eq!(t.source[0], "type");
    }

    #[test]
    fn instance_quantity_shadows_same_named_type_quantity() {
        // Same dedup contract as psets — instance wins on collision.
        // The (qto_name, quantity_name) tuple is the dedup key.
        let buf = make_buf(
            r#"
#20=IFCQUANTITYAREA('NetArea',$,$,12.5);
#21=IFCELEMENTQUANTITY('2Qto000000000000000001',$,'Qto_WallBaseQuantities',$,$,(#20));
#22=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#10),#21);
#30=IFCQUANTITYAREA('NetArea',$,$,9.9);
#31=IFCELEMENTQUANTITY('4Qto0000000000000001',$,'Qto_WallBaseQuantities',$,$,(#30));
#32=IFCWALLTYPE('5Type0000000000000001',$,'Concrete',$,$,(#31),$,$,$,.STANDARD.);
#33=IFCRELDEFINESBYTYPE('6RelType000000000000001',$,$,$,(#10),#32);
"#,
        );
        let t = run(&buf);
        assert_eq!(t.len(), 1);
        assert_eq!(t.value[0].as_deref(), Some("12.5"));
        assert_eq!(t.source[0], "instance");
    }

    #[test]
    fn type_qto_inherits_to_all_related_instances_with_unit_fallback() {
        // Combined inheritance + unit fallback. Type carries the qto,
        // qto's quantities leave Unit as `$`, and the project's
        // IfcUnitAssignment supplies the SIUnit. Each inherited row
        // should still benefit from the GH #43 fallback so consumers
        // see usable unit_step_id values on a type-only qto.
        let buf = format!(
            r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('qto_test.ifc','2026-06-02T00:00:00',('test'),('test'),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCPROJECT('0Test000000000000000001',$,'p',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#3,#90));
#3=IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.);
#90=IFCSIUNIT(*,.MASSUNIT.,.KILO.,.GRAM.);
#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);
#6=IFCAXIS2PLACEMENT3D(#7,$,$);
#7=IFCCARTESIANPOINT((0.,0.,0.));
#10=IFCWALL('1Wall00000000000000001',$,'W1',$,$,$,$,'t1',.STANDARD.);
#11=IFCWALL('1Wall00000000000000002',$,'W2',$,$,$,$,'t2',.STANDARD.);
#30=IFCQUANTITYWEIGHT('GrossWeight',$,$,42.5);
#31=IFCELEMENTQUANTITY('4Qto0000000000000001',$,'Qto_TypeBase',$,$,(#30));
#32=IFCWALLTYPE('5Type0000000000000001',$,'Concrete',$,$,(#31),$,$,$,.STANDARD.);
#33=IFCRELDEFINESBYTYPE('6RelType000000000000001',$,$,$,(#10,#11),#32);
ENDSEC;
END-ISO-10303-21;
"#
        );
        let t = run(&buf);
        assert_eq!(t.len(), 2);
        let guids: std::collections::HashSet<&str> =
            t.guid.iter().map(String::as_str).collect();
        assert!(guids.contains("1Wall00000000000000001"));
        assert!(guids.contains("1Wall00000000000000002"));
        for i in 0..t.len() {
            assert_eq!(t.source[i], "type");
            assert_eq!(t.quantity_type[i], "Weight");
            // GH #43 fallback firing on an inherited row — the
            // project's MASSUNIT step_id surfaces here.
            assert_eq!(t.unit_step_id[i], Some(90));
        }
    }

    #[test]
    fn type_object_with_only_pset_refs_emits_no_quantity_rows() {
        // IfcTypeObject.HasPropertySets accepts both IfcPropertySet
        // AND IfcElementQuantity refs. When a type only carries
        // psets (the common case — Pset_ManufacturerTypeInformation
        // etc.), the quantity extractor must NOT invent rows.
        // Verified by setting up a type whose HasPropertySets points
        // ONLY at an IfcPropertySet, no IfcElementQuantity.
        let buf = make_buf(
            r#"
#30=IFCPROPERTYSINGLEVALUE('Manufacturer',$,IFCLABEL('Wurth'),$);
#31=IFCPROPERTYSET('4PsetType00000000000001',$,'Pset_Manuf',$,(#30));
#32=IFCWALLTYPE('5Type0000000000000001',$,'X',$,$,(#31),$,$,$,.STANDARD.);
#33=IFCRELDEFINESBYTYPE('6RelType000000000000001',$,$,$,(#10),#32);
"#,
        );
        let t = run(&buf);
        assert_eq!(t.len(), 0);
    }

    #[test]
    fn unassigned_si_unit_does_not_leak_as_project_default() {
        // A file that declares an SIUnit but doesn't reference it from
        // any IfcUnitAssignment (e.g. a dangling SIUnit nested inside
        // a never-resolved IfcConversionBasedUnit). That SIUnit must
        // NOT silently become the project default — fallback stays
        // None.
        let buf = format!(
            r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('qto_test.ifc','2026-06-02T00:00:00',('test'),('test'),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCPROJECT('0Test000000000000000001',$,'p',$,$,$,$,(#5),$);
#3=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);
#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);
#6=IFCAXIS2PLACEMENT3D(#7,$,$);
#7=IFCCARTESIANPOINT((0.,0.,0.));
#10=IFCWALL('1Wall00000000000000001',$,'W',$,$,$,$,'t',.STANDARD.);
#41=IFCQUANTITYLENGTH('Length',$,$,3.0);
#47=IFCELEMENTQUANTITY('2Qto000000000000000001',$,'Qto_X',$,$,(#41));
#48=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#10),#47);
ENDSEC;
END-ISO-10303-21;
"#
        );
        let t = run(&buf);
        assert_eq!(t.len(), 1);
        // SIUnit #3 exists but isn't referenced by any
        // IfcUnitAssignment, so it doesn't qualify as a project
        // default for fallback.
        assert_eq!(t.unit_step_id[0], None);
    }
}
