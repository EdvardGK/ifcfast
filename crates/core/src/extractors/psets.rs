//! Property-set extraction.
//!
//! Walks the entity table for IfcPropertySet, IfcPropertySingleValue, and
//! IfcRelDefinesByProperties records and emits long-format rows:
//!
//! ```text
//! (product_guid, pset_name, prop_name, value_str, value_type)
//! ```
//!
//! For the "all external load-bearing walls" class of query. Stays in
//! column-major form so the PyO3 bridge marshalling stays cheap.
//!
//! Phase 1 scope: `IfcPropertySet` + `IfcPropertySingleValue` only. Covers
//! 90%+ of psets seen on Revit/Archicad/Tekla/MagiCAD exports. Bounded,
//! enumerated, list, and complex property variants are future work
//! (Phase 2 if any query actually needs them).

use std::collections::HashMap;

use crate::entity_table::EntityTable;
use crate::lexer::{parse_field, split_top_level_args, Field};

/// Long-format pset rows in column-major layout.
#[derive(Debug, Default)]
pub struct PsetTable {
    pub guid: Vec<String>,
    pub pset_name: Vec<String>,
    pub prop_name: Vec<String>,
    pub value: Vec<Option<String>>,
    pub value_type: Vec<Option<String>>,
}

impl PsetTable {
    pub fn len(&self) -> usize {
        self.guid.len()
    }

    pub fn is_empty(&self) -> bool {
        self.guid.is_empty()
    }
}

/// Build the property table given an entity-table and a step_id → guid
/// resolver for the products you care about (typically the products
/// the indexer already extracted).
pub fn build(
    table: &EntityTable,
    product_step_to_guid: &HashMap<u64, String>,
) -> PsetTable {
    // Pass 1: collect IfcPropertySet records (id → (name, prop_ids))
    //         and IfcPropertySingleValue records (id → Prop).
    let mut psets: HashMap<u64, (String, Vec<u64>)> = HashMap::with_capacity(2048);
    let mut props: HashMap<u64, Prop> = HashMap::with_capacity(8192);
    // IfcComplexProperty groups inner properties under a named "complex"
    // wrapper. We flatten in pass 2 via dot-joined names: a complex
    // "ProfileGeometry" containing "Width" + "Height" produces rows
    // named "ProfileGeometry.Width" + "ProfileGeometry.Height".
    let mut complex_props: HashMap<u64, (String, Vec<u64>)> = HashMap::with_capacity(256);
    // Pass 2 input: (related_object_step_ids, pset_step_id) — many rels share
    // a single pset, and many objects share a single rel.
    let mut rel_pairs: Vec<(u64, u64)> = Vec::with_capacity(16_384);

    for (step_id, type_name, args) in table.iter() {
        if type_name.eq_ignore_ascii_case(b"IFCPROPERTYSET") {
            // (GlobalId, OwnerHistory, Name, Description, HasProperties)
            let fields = split_top_level_args(args);
            let name = string_at(&fields, 2).unwrap_or_default();
            let prop_ids = ref_list_at(&fields, 4);
            psets.insert(step_id, (name, prop_ids));
        } else if type_name.eq_ignore_ascii_case(b"IFCPROPERTYSINGLEVALUE") {
            // (Name, Description, NominalValue, Unit)
            let fields = split_top_level_args(args);
            let name = string_at(&fields, 0).unwrap_or_default();
            let (val_str, val_type) = parse_nominal_value(fields.get(2).copied());
            props.insert(step_id, Prop { name, value: val_str, value_type: val_type });
        } else if type_name.eq_ignore_ascii_case(b"IFCPROPERTYENUMERATEDVALUE")
            || type_name.eq_ignore_ascii_case(b"IFCPROPERTYLISTVALUE")
        {
            // IfcPropertyEnumeratedValue: (Name, Description, EnumerationValues, EnumerationReference)
            // IfcPropertyListValue:       (Name, Description, ListValues, Unit)
            // Both carry a LIST of IfcValue at arg 2. Joined with `, `
            // for the row's value string; value_type follows the first
            // member's type since enumerated/list values must be
            // homogeneous in the IFC schema.
            //
            // A common Norwegian-export pattern: fire ratings declared
            // as IfcPropertyEnumeratedValue with a single member like
            // IFCLABEL('R60'). Pre-fix these were silently dropped; now
            // they surface alongside IfcPropertySingleValue properties.
            let fields = split_top_level_args(args);
            let name = string_at(&fields, 0).unwrap_or_default();
            let (val_str, val_type) =
                parse_value_list(fields.get(2).copied());
            props.insert(step_id, Prop { name, value: val_str, value_type: val_type });
        } else if type_name.eq_ignore_ascii_case(b"IFCPROPERTYBOUNDEDVALUE") {
            // (Name, Description, UpperBoundValue, LowerBoundValue, Unit, SetPointValue)
            // Three optional IfcValues. Format: "lower..upper" if both
            // bounds present, or "..upper" / "lower.." if one-sided.
            // SetPointValue (IFC4) appended as "@setpoint" when present.
            //
            // MEP exports use this for temperature ranges, pressure
            // tolerances, flow rate windows. Pre-fix all of these were
            // silently dropped from psets.
            let fields = split_top_level_args(args);
            let name = string_at(&fields, 0).unwrap_or_default();
            let (upper_val, upper_type) =
                parse_nominal_value(fields.get(2).copied());
            let (lower_val, _) = parse_nominal_value(fields.get(3).copied());
            let (setpoint_val, _) = parse_nominal_value(fields.get(5).copied());
            let val_str = format_bounded(lower_val.as_deref(), upper_val.as_deref(), setpoint_val.as_deref());
            props.insert(step_id, Prop { name, value: val_str, value_type: upper_type });
        } else if type_name.eq_ignore_ascii_case(b"IFCCOMPLEXPROPERTY") {
            // (Name, Description, UsageName, HasProperties)
            // Note: IfcComplexProperty does NOT inherit IfcRoot — no
            // GlobalId here. The leading arg is the property name.
            let fields = split_top_level_args(args);
            let name = string_at(&fields, 0).unwrap_or_default();
            let inner_ids = ref_list_at(&fields, 3);
            complex_props.insert(step_id, (name, inner_ids));
        } else if type_name.eq_ignore_ascii_case(b"IFCRELDEFINESBYPROPERTIES") {
            // (GlobalId, OwnerHistory, Name, Description, RelatedObjects, RelatingPropertyDefinition)
            let fields = split_top_level_args(args);
            let pset_id = match fields.get(5).copied().map(parse_field) {
                Some(Field::Ref(id)) => id,
                _ => continue,
            };
            // RelatedObjects can be a list OR a single ref in IFC2X3
            // (some authoring tools emit a bare ref). Handle both.
            let relateds = match fields.get(4).copied().map(parse_field) {
                Some(Field::List(body)) => parse_ref_list(body),
                Some(Field::Ref(id)) => vec![id],
                _ => continue,
            };
            for obj_id in relateds {
                rel_pairs.push((obj_id, pset_id));
            }
        }
    }

    // Pass 2: for each (object, pset) pair, expand to one row per
    //         property in the pset.
    let mut out = PsetTable::default();
    let est = rel_pairs.len() * 8;
    out.guid.reserve(est);
    out.pset_name.reserve(est);
    out.prop_name.reserve(est);
    out.value.reserve(est);
    out.value_type.reserve(est);

    for (obj_step_id, pset_step_id) in rel_pairs {
        let guid = match product_step_to_guid.get(&obj_step_id) {
            Some(g) => g,
            None => continue, // rel pointed at a non-product (type, group, etc.)
        };
        let (pset_name, prop_ids) = match psets.get(&pset_step_id) {
            Some(x) => x,
            None => continue,
        };
        // Each top-level property in the pset resolves to one of:
        //   - A leaf IfcProperty* (single, enumerated, list, bounded)
        //     → in `props` → one output row.
        //   - An IfcComplexProperty wrapping more inner property refs
        //     → in `complex_props` → recurse, prefixing each leaf name
        //       with the complex's own name joined by `.`.
        for pid in prop_ids {
            emit_property(
                pid,
                "",
                guid,
                pset_name,
                &props,
                &complex_props,
                &mut out,
                0,
            );
        }
    }

    out
}

#[derive(Debug)]
struct Prop {
    name: String,
    value: Option<String>,
    value_type: Option<String>,
}

/// Cap on IfcComplexProperty nesting depth. The schema allows
/// arbitrary recursion; real-world exports rarely go past 2-3 levels.
/// A bounded walk protects against pathological / cyclic files.
const COMPLEX_PROP_MAX_DEPTH: usize = 8;

/// Emit one or more rows into `out` for the property at `pid`.
///
/// - Leaf property (single / enum / list / bounded) → one row with
///   `prop_name = "{prefix}{leaf.name}"`. `prefix` is empty for top-
///   level properties, or `"OuterComplex.InnerComplex."` for nested.
/// - Complex property → recurse over its inner refs with an extended
///   prefix `"{prefix}{complex.name}."`.
/// - Anything else (unknown ref target) → silently dropped, same as
///   pre-fix behaviour for leaf-only lookups.
#[allow(clippy::too_many_arguments)]
fn emit_property(
    pid: &u64,
    prefix: &str,
    guid: &str,
    pset_name: &str,
    props: &HashMap<u64, Prop>,
    complex_props: &HashMap<u64, (String, Vec<u64>)>,
    out: &mut PsetTable,
    depth: usize,
) {
    if let Some(prop) = props.get(pid) {
        let name = if prefix.is_empty() {
            prop.name.clone()
        } else {
            format!("{prefix}{}", prop.name)
        };
        out.guid.push(guid.to_string());
        out.pset_name.push(pset_name.to_string());
        out.prop_name.push(name);
        out.value.push(prop.value.clone());
        out.value_type.push(prop.value_type.clone());
        return;
    }
    if let Some((complex_name, inner_ids)) = complex_props.get(pid) {
        if depth >= COMPLEX_PROP_MAX_DEPTH {
            return;
        }
        let new_prefix = format!("{prefix}{complex_name}.");
        for inner in inner_ids {
            emit_property(
                inner,
                &new_prefix,
                guid,
                pset_name,
                props,
                complex_props,
                out,
                depth + 1,
            );
        }
    }
}

/// Parse an `IfcValue` field. STEP wraps these with a type tag:
///   IFCBOOLEAN(.T.)   IFCTEXT('hello')   IFCREAL(0.42)   IFCLABEL('lbl')
/// Returns (value_string, type_string). Either may be None on `$` etc.
///
/// Boolean and logical enum values are normalised to ifcopenshell's
/// stringification: `.T.` -> "True", `.F.` -> "False", `.U.` -> "Unknown".
/// This closes the IFC2X3 encoding gap surfaced by Edvard's v4 audit
/// (Issue #9): values were semantically correct but stringified as the
/// STEP enum literals rather than as Python booleans.
fn parse_nominal_value(raw: Option<&[u8]>) -> (Option<String>, Option<String>) {
    let raw = match raw {
        Some(r) => r,
        None => return (None, None),
    };
    let trimmed = trim(raw);
    if trimmed.is_empty() || trimmed == b"$" || trimmed == b"*" {
        return (None, None);
    }
    // Type-wrapped: TYPENAME(inner)
    if let Some((type_name, inner)) = split_type_wrapper(trimmed) {
        let inner_field = trim(inner);
        let raw_value = scalar_to_string(inner_field);
        let type_str = crate::indexer::type_name_uppercase_with_proper_case(type_name);
        // ifcopenshell stringifies IfcBoolean.T/.F via Python bool → str
        // (-> "True"/"False"), but IfcLogical.U has no bool representation
        // and falls back to the all-caps schema enum literal "UNKNOWN".
        let normalised = match (raw_value.as_deref(), type_str.as_str()) {
            (Some("T"), "IfcBoolean") | (Some("T"), "IfcLogical") => Some("True".to_string()),
            (Some("F"), "IfcBoolean") | (Some("F"), "IfcLogical") => Some("False".to_string()),
            (Some("U"), "IfcLogical") => Some("UNKNOWN".to_string()),
            _ => raw_value,
        };
        return (normalised, Some(type_str));
    }
    // Bare value (rare for IfcValue but possible).
    (scalar_to_string(trimmed), None)
}

/// Parse a `LIST OF IfcValue` field. Splits the list, runs each element
/// through `parse_nominal_value`, joins the resulting value strings with
/// `", "`. Type comes from the first member (the IFC schema requires
/// homogeneous element types within a property's value list).
///
/// Returns `(None, None)` for `$`, `*`, empty list, or a list whose
/// members all parse to None.
fn parse_value_list(raw: Option<&[u8]>) -> (Option<String>, Option<String>) {
    let raw = match raw {
        Some(r) => r,
        None => return (None, None),
    };
    let trimmed = trim(raw);
    if trimmed.is_empty() || trimmed == b"$" || trimmed == b"*" {
        return (None, None);
    }
    // The list body sits between '(' and ')'.
    let inner = match (trimmed.first(), trimmed.last()) {
        (Some(&b'('), Some(&b')')) if trimmed.len() >= 2 => &trimmed[1..trimmed.len() - 1],
        _ => return (None, None),
    };
    let mut values: Vec<String> = Vec::new();
    let mut value_type: Option<String> = None;
    for item in split_top_level_args(inner) {
        let (v, t) = parse_nominal_value(Some(item));
        if value_type.is_none() {
            value_type = t;
        }
        if let Some(s) = v {
            values.push(s);
        }
    }
    if values.is_empty() {
        (None, value_type)
    } else {
        (Some(values.join(", ")), value_type)
    }
}

/// Format an `IfcPropertyBoundedValue`'s (lower, upper, setpoint) tuple
/// into a single string. Conventions:
///   both bounds      → `"lower..upper"`
///   upper only       → `"..upper"`
///   lower only       → `"lower.."`
///   setpoint only    → `"@setpoint"`
///   bounds + setpt   → `"lower..upper@setpoint"`
///   nothing          → `None`
fn format_bounded(
    lower: Option<&str>,
    upper: Option<&str>,
    setpoint: Option<&str>,
) -> Option<String> {
    if lower.is_none() && upper.is_none() && setpoint.is_none() {
        return None;
    }
    let mut out = String::new();
    if lower.is_some() || upper.is_some() {
        if let Some(l) = lower {
            out.push_str(l);
        }
        out.push_str("..");
        if let Some(u) = upper {
            out.push_str(u);
        }
    }
    if let Some(s) = setpoint {
        out.push('@');
        out.push_str(s);
    }
    Some(out)
}

/// `TYPENAME(inner)` → (TYPENAME bytes, inner bytes). Returns None if
/// the field doesn't match the wrapper shape.
fn split_type_wrapper(field: &[u8]) -> Option<(&[u8], &[u8])> {
    // Must start with IFC (case-insensitive) and contain a parenthesis.
    if field.len() < 5 || !field[..3].eq_ignore_ascii_case(b"IFC") {
        return None;
    }
    let open = field.iter().position(|&b| b == b'(')?;
    // Last char must be `)`.
    if *field.last()? != b')' {
        return None;
    }
    Some((&field[..open], &field[open + 1..field.len() - 1]))
}

/// Render the inner scalar (string, number, enum, ref) as a normalised
/// Python-friendly value.
fn scalar_to_string(raw: &[u8]) -> Option<String> {
    let trimmed = trim(raw);
    if trimmed.is_empty() || trimmed == b"$" || trimmed == b"*" {
        return None;
    }
    match parse_field(trimmed) {
        Field::String(s) => Some(s),
        Field::Number(n) => Some(format_number(n)),
        Field::Enum(e) => Some(std::str::from_utf8(e).ok()?.to_string()),
        Field::Ref(id) => Some(format!("#{}", id)),
        Field::Null | Field::Star => None,
        Field::List(_) | Field::Other(_) => Some(
            std::str::from_utf8(trimmed)
                .ok()?
                .to_string(),
        ),
    }
}

fn format_number(n: f64) -> String {
    // Avoid "1.0e+00" style for tidy CSV/parquet:
    if n.fract() == 0.0 && n.is_finite() && n.abs() < 1e15 {
        return format!("{}", n as i64);
    }
    format!("{}", n)
}

fn trim(s: &[u8]) -> &[u8] {
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

fn string_at(fields: &[&[u8]], idx: usize) -> Option<String> {
    match parse_field(fields.get(idx)?) {
        Field::String(s) => Some(s),
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

    /// Build the bare minimum IFC envelope around a list of extra DATA
    /// statements. The wall #10 is the only product; `extra_data` is
    /// expected to declare the pset, properties, and the relation that
    /// binds them to #10.
    fn make_buf(extra_data: &str) -> String {
        format!(
            r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('psets_test.ifc','2026-05-26T00:00:00',('test'),('test'),'ifcfast','ifcfast','');
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

    fn run(buf: &str) -> PsetTable {
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
    fn text_label_value_unwraps_type_and_string() {
        // `IFCLABEL('Internal')` should yield value="Internal", type="Label".
        let buf = make_buf(
            r#"
#20=IFCPROPERTYSINGLEVALUE('LoadBearing',$,IFCLABEL('Internal'),$);
#21=IFCPROPERTYSET('2Pset00000000000000001',$,'Pset_WallCommon',$,(#20));
#22=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#10),#21);
"#,
        );
        let t = run(&buf);
        assert_eq!(t.len(), 1);
        assert_eq!(t.guid[0], "1Wall00000000000000001");
        assert_eq!(t.pset_name[0], "Pset_WallCommon");
        assert_eq!(t.prop_name[0], "LoadBearing");
        assert_eq!(t.value[0].as_deref(), Some("Internal"));
        // The type unwrap normalises `IFCLABEL` → `IfcLabel` via the
        // canonical entity-name table.
        assert_eq!(t.value_type[0].as_deref(), Some("IfcLabel"));
    }

    #[test]
    fn boolean_value_serialises_as_python_truth_string() {
        // `.T.` → `"True"` (matches ifcopenshell's Python-bool stringify).
        let buf = make_buf(
            r#"
#20=IFCPROPERTYSINGLEVALUE('IsExternal',$,IFCBOOLEAN(.T.),$);
#21=IFCPROPERTYSET('2Pset00000000000000001',$,'Pset_WallCommon',$,(#20));
#22=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#10),#21);
"#,
        );
        let t = run(&buf);
        assert_eq!(t.value[0].as_deref(), Some("True"));
        assert_eq!(t.value_type[0].as_deref(), Some("IfcBoolean"));
    }

    #[test]
    fn unknown_logical_serialises_as_uppercase_enum() {
        // IFCLOGICAL.U has no bool counterpart and must surface as
        // the all-caps schema literal "UNKNOWN" (not Python's "None").
        let buf = make_buf(
            r#"
#20=IFCPROPERTYSINGLEVALUE('Combustible',$,IFCLOGICAL(.U.),$);
#21=IFCPROPERTYSET('2Pset00000000000000001',$,'Pset_WallCommon',$,(#20));
#22=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#10),#21);
"#,
        );
        let t = run(&buf);
        assert_eq!(t.value[0].as_deref(), Some("UNKNOWN"));
        assert_eq!(t.value_type[0].as_deref(), Some("IfcLogical"));
    }

    #[test]
    fn missing_nominal_value_produces_null_row() {
        // Property with `$` for NominalValue. The row should still
        // exist (reveal-all: the prop EXISTS, its value is unknown)
        // but value and value_type must both be None.
        let buf = make_buf(
            r#"
#20=IFCPROPERTYSINGLEVALUE('Reference',$,$,$);
#21=IFCPROPERTYSET('2Pset00000000000000001',$,'Pset_WallCommon',$,(#20));
#22=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#10),#21);
"#,
        );
        let t = run(&buf);
        assert_eq!(t.len(), 1);
        assert_eq!(t.value[0], None);
        assert_eq!(t.value_type[0], None);
    }

    #[test]
    fn pset_with_multiple_properties_produces_one_row_each() {
        let buf = make_buf(
            r#"
#20=IFCPROPERTYSINGLEVALUE('IsExternal',$,IFCBOOLEAN(.T.),$);
#21=IFCPROPERTYSINGLEVALUE('LoadBearing',$,IFCBOOLEAN(.F.),$);
#22=IFCPROPERTYSINGLEVALUE('Reference',$,IFCLABEL('W-001'),$);
#23=IFCPROPERTYSET('2Pset00000000000000001',$,'Pset_WallCommon',$,(#20,#21,#22));
#24=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#10),#23);
"#,
        );
        let t = run(&buf);
        assert_eq!(t.len(), 3);
        // Every row points back to the same product + same pset.
        for i in 0..3 {
            assert_eq!(t.guid[i], "1Wall00000000000000001");
            assert_eq!(t.pset_name[i], "Pset_WallCommon");
        }
        // Each property appears exactly once.
        let prop_names: std::collections::HashSet<&str> =
            t.prop_name.iter().map(String::as_str).collect();
        assert_eq!(prop_names.len(), 3);
        assert!(prop_names.contains("IsExternal"));
        assert!(prop_names.contains("LoadBearing"));
        assert!(prop_names.contains("Reference"));
    }

    #[test]
    fn single_ref_related_object_works_like_a_list_of_one() {
        // Some IFC2X3 authoring tools emit `RelatedObjects = #10`
        // (bare ref) instead of `(#10)` (list). The extractor must
        // accept both.
        let buf = make_buf(
            r#"
#20=IFCPROPERTYSINGLEVALUE('LoadBearing',$,IFCBOOLEAN(.T.),$);
#21=IFCPROPERTYSET('2Pset00000000000000001',$,'Pset_WallCommon',$,(#20));
#22=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,#10,#21);
"#,
        );
        let t = run(&buf);
        assert_eq!(t.len(), 1);
        assert_eq!(t.guid[0], "1Wall00000000000000001");
    }

    #[test]
    fn property_for_unknown_guid_is_dropped() {
        // The product ref `#99` doesn't exist; the extractor must NOT
        // emit a row (guid lookup misses) but also must not panic.
        let buf = make_buf(
            r#"
#20=IFCPROPERTYSINGLEVALUE('LoadBearing',$,IFCBOOLEAN(.T.),$);
#21=IFCPROPERTYSET('2Pset00000000000000001',$,'Pset_WallCommon',$,(#20));
#22=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#99),#21);
"#,
        );
        let t = run(&buf);
        // Only the wall (#10) is in step_to_guid, and the rel didn't
        // include it — table should be empty.
        assert_eq!(t.len(), 0);
    }

    #[test]
    fn enumerated_value_single_member_surfaces_like_single_value() {
        // The common pattern: Norwegian fire-rating exports declare
        // FireRating as IfcPropertyEnumeratedValue with one chosen
        // member like IFCLABEL('R60'). Pre-fix this was silently
        // dropped from psets.
        let buf = make_buf(
            r#"
#20=IFCPROPERTYENUMERATEDVALUE('FireRating',$,(IFCLABEL('R60')),$);
#21=IFCPROPERTYSET('2Pset00000000000000001',$,'Pset_WallCommon',$,(#20));
#22=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#10),#21);
"#,
        );
        let t = run(&buf);
        assert_eq!(t.len(), 1);
        assert_eq!(t.prop_name[0], "FireRating");
        assert_eq!(t.value[0].as_deref(), Some("R60"));
        assert_eq!(t.value_type[0].as_deref(), Some("IfcLabel"));
    }

    #[test]
    fn enumerated_value_multi_member_joins_with_comma() {
        // Some exports list every allowable enum member (rare but
        // legal). All values get joined with ", " — the consumer can
        // split if needed.
        let buf = make_buf(
            r#"
#20=IFCPROPERTYENUMERATEDVALUE('Categories',$,(IFCLABEL('Residential'),IFCLABEL('Office')),$);
#21=IFCPROPERTYSET('2Pset00000000000000001',$,'Pset_BuildingCommon',$,(#20));
#22=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#10),#21);
"#,
        );
        let t = run(&buf);
        assert_eq!(t.len(), 1);
        assert_eq!(t.value[0].as_deref(), Some("Residential, Office"));
        assert_eq!(t.value_type[0].as_deref(), Some("IfcLabel"));
    }

    #[test]
    fn list_value_same_treatment_as_enumerated() {
        let buf = make_buf(
            r#"
#20=IFCPROPERTYLISTVALUE('AllowedTemperatures',$,(IFCREAL(18.),IFCREAL(20.),IFCREAL(22.)),$);
#21=IFCPROPERTYSET('2Pset00000000000000001',$,'Pset_SpaceThermalLoad',$,(#20));
#22=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#10),#21);
"#,
        );
        let t = run(&buf);
        assert_eq!(t.len(), 1);
        // Whole-number IfcReal scalars normalise to integer-string form
        // ("22.0" → "22") per `format_number`. The join order matches
        // the IFC list order.
        assert_eq!(t.value[0].as_deref(), Some("18, 20, 22"));
        assert_eq!(t.value_type[0].as_deref(), Some("IfcReal"));
    }

    #[test]
    fn bounded_value_both_bounds_format() {
        // MEP comfort range: room temperature 18-22°C.
        let buf = make_buf(
            r#"
#20=IFCPROPERTYBOUNDEDVALUE('TempRange',$,IFCREAL(22.),IFCREAL(18.),$,$);
#21=IFCPROPERTYSET('2Pset00000000000000001',$,'Pset_SpaceThermalLoad',$,(#20));
#22=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#10),#21);
"#,
        );
        let t = run(&buf);
        assert_eq!(t.len(), 1);
        assert_eq!(t.value[0].as_deref(), Some("18..22"));
        assert_eq!(t.value_type[0].as_deref(), Some("IfcReal"));
    }

    #[test]
    fn bounded_value_one_sided_format() {
        // Upper-only bound — common for "max pressure" properties.
        let buf = make_buf(
            r#"
#20=IFCPROPERTYBOUNDEDVALUE('MaxPressure',$,IFCREAL(2.5),$,$,$);
#21=IFCPROPERTYSET('2Pset00000000000000001',$,'Pset_Custom',$,(#20));
#22=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#10),#21);
"#,
        );
        let t = run(&buf);
        assert_eq!(t.len(), 1);
        assert_eq!(t.value[0].as_deref(), Some("..2.5"));
    }

    #[test]
    fn complex_property_flattens_to_dot_joined_names() {
        // A common structural-export pattern: profile geometry as an
        // IfcComplexProperty wrapping Width + Height single-values.
        let buf = make_buf(
            r#"
#20=IFCPROPERTYSINGLEVALUE('Width',$,IFCLENGTHMEASURE(200.),$);
#21=IFCPROPERTYSINGLEVALUE('Height',$,IFCLENGTHMEASURE(400.),$);
#22=IFCCOMPLEXPROPERTY('ProfileGeometry',$,'SIZE',(#20,#21));
#23=IFCPROPERTYSET('2Pset00000000000000001',$,'Pset_BeamCommon',$,(#22));
#24=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#10),#23);
"#,
        );
        let t = run(&buf);
        assert_eq!(t.len(), 2, "expected 2 leaf rows, got {}", t.len());
        let by_name: std::collections::HashMap<&str, &str> = (0..t.len())
            .filter_map(|i| {
                t.value[i].as_deref().map(|v| (t.prop_name[i].as_str(), v))
            })
            .collect();
        assert_eq!(by_name.get("ProfileGeometry.Width"), Some(&"200"));
        assert_eq!(by_name.get("ProfileGeometry.Height"), Some(&"400"));
    }

    #[test]
    fn nested_complex_properties_chain_their_prefixes() {
        // Complex → Complex → leaf. Each layer of nesting prepends its
        // name with a dot separator.
        let buf = make_buf(
            r#"
#20=IFCPROPERTYSINGLEVALUE('Value',$,IFCREAL(0.05),$);
#21=IFCCOMPLEXPROPERTY('SubGroup',$,'NESTED',(#20));
#22=IFCCOMPLEXPROPERTY('OuterGroup',$,'GROUP',(#21));
#23=IFCPROPERTYSET('2Pset00000000000000001',$,'Pset_Custom',$,(#22));
#24=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#10),#23);
"#,
        );
        let t = run(&buf);
        assert_eq!(t.len(), 1);
        assert_eq!(t.prop_name[0], "OuterGroup.SubGroup.Value");
        assert_eq!(t.value[0].as_deref(), Some("0.05"));
    }

    #[test]
    fn complex_property_alongside_simple_in_same_pset() {
        // The pset's top-level HasProperties list can mix complex and
        // non-complex entries.
        let buf = make_buf(
            r#"
#20=IFCPROPERTYSINGLEVALUE('Reference',$,IFCLABEL('REF-001'),$);
#21=IFCPROPERTYSINGLEVALUE('A',$,IFCREAL(1.),$);
#22=IFCPROPERTYSINGLEVALUE('B',$,IFCREAL(2.),$);
#23=IFCCOMPLEXPROPERTY('Group',$,'GROUP',(#21,#22));
#24=IFCPROPERTYSET('2Pset00000000000000001',$,'Pset_Mixed',$,(#20,#23));
#25=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#10),#24);
"#,
        );
        let t = run(&buf);
        assert_eq!(t.len(), 3);
        let names: std::collections::HashSet<&str> =
            t.prop_name.iter().map(String::as_str).collect();
        // Top-level leaf keeps its plain name.
        assert!(names.contains("Reference"));
        // Inner leaves get the group prefix.
        assert!(names.contains("Group.A"));
        assert!(names.contains("Group.B"));
    }

    #[test]
    fn bounded_value_with_setpoint() {
        // IFC4 SetPointValue: target with tolerance bounds around it.
        let buf = make_buf(
            r#"
#20=IFCPROPERTYBOUNDEDVALUE('SetpointTemp',$,IFCREAL(22.),IFCREAL(18.),$,IFCREAL(20.));
#21=IFCPROPERTYSET('2Pset00000000000000001',$,'Pset_SpaceThermalLoad',$,(#20));
#22=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#10),#21);
"#,
        );
        let t = run(&buf);
        assert_eq!(t.len(), 1);
        assert_eq!(t.value[0].as_deref(), Some("18..22@20"));
    }
}
