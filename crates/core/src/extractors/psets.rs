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
//! Single, enumerated, list, bounded, table and complex properties are
//! flattened to one string per row; any other `IfcProperty*Value` class
//! surfaces as an `unhandled:IFCXXX` marker row (GH #38).
//!
//! Discovery is the shared typed pass in [`super::property_graph`]; this
//! module is the flattened public view of it. Its formatting rules, row
//! order and marker rows predate the graph and are kept byte for byte.

use std::collections::{HashMap, HashSet};

use super::property_graph::{
    split_type_wrapper, trim, GraphScope, PropClass, PropDef, PropertyGraph, PsetKind, TypedValue,
};
use crate::entity_table::EntityTable;
#[cfg(test)]
use crate::lexer::split_top_level_args;
use crate::lexer::{parse_field, Field};

/// Long-format pset rows in column-major layout.
///
/// `source` is `"instance"` for properties declared directly on a
/// product via `IfcRelDefinesByProperties`, and `"type"` for properties
/// inherited from the product's `IfcTypeObject` via `IfcRelDefinesByType
/// → RelatingType.HasPropertySets`. Inheritance matches ifcopenshell's
/// `should_inherit=True` default: an instance-declared property
/// shadows a same-named type property (instance wins on collision; no
/// `source="type"` row is emitted in that case).
#[derive(Debug, Default)]
pub struct PsetTable {
    pub guid: Vec<String>,
    pub pset_name: Vec<String>,
    pub prop_name: Vec<String>,
    pub value: Vec<Option<String>>,
    pub value_type: Vec<Option<String>>,
    pub source: Vec<String>,
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
pub fn build(table: &EntityTable, product_step_to_guid: &HashMap<u64, String>) -> PsetTable {
    let graph = PropertyGraph::build_scoped(table, GraphScope::PROPERTIES);
    build_from_graph(&graph, product_step_to_guid)
}

/// [`build`] from an already-built graph (which must include
/// [`GraphScope::properties`]).
///
/// Rows: first every `(object, IfcPropertySet)` pair of
/// `IfcRelDefinesByProperties` in relation order, one row per leaf
/// property (complex properties flatten to dot-joined names, e.g.
/// `"ProfileGeometry.Width"`); then, by product step id, the leaves of
/// each product's type `HasPropertySets` whose `(pset_name, prop_name)`
/// no instance row on the same guid already carries (instance wins,
/// ifcopenshell `should_inherit=True`).
pub fn build_from_graph(
    graph: &PropertyGraph,
    product_step_to_guid: &HashMap<u64, String>,
) -> PsetTable {
    assert!(
        graph.scope.properties,
        "psets::build_from_graph needs a PropertyGraph built with properties in scope"
    );
    // The flattened (value, value_type) of every leaf definition, once per
    // definition (many rows share one definition).
    let mut formatted: Vec<Option<(Option<String>, Option<String>)>> =
        vec![None; graph.props.len()];
    for def in graph.props.values() {
        formatted[def.ord] = flatten_value(def);
    }

    let mut out = PsetTable::default();
    let est = graph.defines.len() * 8 + graph.object_type.len() * 4;
    out.guid.reserve(est);
    out.pset_name.reserve(est);
    out.prop_name.reserve(est);
    out.value.reserve(est);
    out.value_type.reserve(est);
    out.source.reserve(est);

    // (guid → "pset_name\tprop_name" keys emitted on the instance side).
    // Keyed by guid, not step id, so two steps sharing a guid shadow each
    // other exactly as they always have.
    let mut seen_per_product: HashMap<&str, HashSet<String>> =
        HashMap::with_capacity(product_step_to_guid.len());

    for (obj_step_id, set_step_id) in &graph.defines {
        let guid = match product_step_to_guid.get(obj_step_id) {
            Some(g) => g.as_str(),
            None => continue, // rel pointed at a non-product (type, group, etc.)
        };
        let set = match graph.sets.get(set_step_id) {
            Some(s) if s.kind == PsetKind::PropertySet => s,
            _ => continue,
        };
        let mut emitted_names: Vec<String> = Vec::new();
        graph.walk_set_leaves(set, &mut |path, def| {
            let Some((value, value_type)) = &formatted[def.ord] else {
                return;
            };
            let name = prop_name(path, &def.name);
            out.guid.push(guid.to_string());
            out.pset_name.push(set.name.clone());
            out.prop_name.push(name.clone());
            out.value.push(value.clone());
            out.value_type.push(value_type.clone());
            out.source.push("instance".to_string());
            emitted_names.push(name);
        });
        if !emitted_names.is_empty() {
            let seen = seen_per_product.entry(guid).or_default();
            for n in emitted_names {
                seen.insert(format!("{}\t{n}", set.name));
            }
        }
    }

    // Type inheritance. Sorted by product step id: iterating the HashMap
    // leaked std's per-process RandomState into the ROW ORDER of this half
    // of the table (GH #152).
    if !graph.type_sets.is_empty() && !graph.object_type.is_empty() {
        let mut inherit: Vec<(u64, u64)> = graph
            .object_type
            .iter()
            .map(|(product, type_id)| (*product, *type_id))
            .collect();
        inherit.sort_unstable();
        let empty = HashSet::new();
        for (product_step_id, type_step_id) in &inherit {
            let guid = match product_step_to_guid.get(product_step_id) {
                Some(g) => g.as_str(),
                None => continue,
            };
            let type_set_ids = match graph.type_sets.get(type_step_id) {
                Some(v) => v,
                None => continue,
            };
            let already_seen = seen_per_product.get(guid).unwrap_or(&empty);
            for set_id in type_set_ids {
                let set = match graph.sets.get(set_id) {
                    Some(s) if s.kind == PsetKind::PropertySet => s,
                    _ => continue,
                };
                graph.walk_set_leaves(set, &mut |path, def| {
                    let Some((value, value_type)) = &formatted[def.ord] else {
                        return;
                    };
                    let name = prop_name(path, &def.name);
                    if already_seen.contains(&format!("{}\t{name}", set.name)) {
                        return;
                    }
                    out.guid.push(guid.to_string());
                    out.pset_name.push(set.name.clone());
                    out.prop_name.push(name);
                    out.value.push(value.clone());
                    out.value_type.push(value_type.clone());
                    out.source.push("type".to_string());
                });
            }
        }
    }

    out
}

/// `"{complex}.{complex}.{leaf}"`: each enclosing complex name followed by
/// a dot, then the leaf name (an unnamed complex contributes a bare `.`).
fn prop_name(path: &[&str], leaf: &str) -> String {
    if path.is_empty() {
        return leaf.to_string();
    }
    let mut s = String::new();
    for p in path {
        s.push_str(p);
        s.push('.');
    }
    s.push_str(leaf);
    s
}

/// The row's `(value, value_type)` for a leaf property definition; `None`
/// for anything that is not a property-family leaf.
///
/// - single: the NominalValue unwrapped (`IFCLABEL('x')` → `"x"`,
///   `"IfcLabel"`);
/// - enumerated / list: members joined with `", "`, type from the first
///   typed member (IFC requires homogeneous lists);
/// - bounded: `"lower..upper"`, `"..upper"`, `"lower.."`, `@setpoint`
///   appended when present; type from the upper bound;
/// - table: `"d1=>v1, d2=>v2"` (`?` for a null member); type from
///   DefinedValues;
/// - reference and unknown `IfcProperty*Value` classes: value `None`,
///   value_type `"unhandled:IFCXXX"` (GH #38) — the blind spot stays
///   visible.
fn flatten_value(def: &PropDef) -> Option<(Option<String>, Option<String>)> {
    Some(match def.class {
        PropClass::SingleValue => parse_nominal_value(def.values.first().map(|v| v.src)),
        PropClass::EnumeratedValue | PropClass::ListValue => parse_value_list(&def.values),
        PropClass::BoundedValue => {
            let at = |i: usize| def.values.get(i).map(|v| v.src);
            let (lower_val, _) = parse_nominal_value(at(0));
            let (upper_val, upper_type) = parse_nominal_value(at(1));
            let (setpoint_val, _) = parse_nominal_value(at(2));
            let val_str = format_bounded(
                lower_val.as_deref(),
                upper_val.as_deref(),
                setpoint_val.as_deref(),
            );
            (val_str, upper_type)
        }
        PropClass::TableValue => {
            let defining_vals = parse_value_list_raw(&def.defining_values);
            let (defined_vals, defined_type) = parse_value_list_with_each(&def.values);
            let val_str = if defining_vals.is_empty() && defined_vals.is_empty() {
                None
            } else {
                let pairs: Vec<String> = defining_vals
                    .iter()
                    .zip(defined_vals.iter())
                    .map(|(d, v)| {
                        let ds = d.as_deref().unwrap_or("?");
                        let vs = v.as_deref().unwrap_or("?");
                        format!("{ds}=>{vs}")
                    })
                    .collect();
                if pairs.is_empty() {
                    None
                } else {
                    Some(pairs.join(", "))
                }
            };
            (val_str, defined_type)
        }
        PropClass::ReferenceValue | PropClass::UnhandledProperty => {
            let marker = format!(
                "unhandled:{}",
                std::str::from_utf8(def.entity)
                    .map(|s| s.to_ascii_uppercase())
                    .unwrap_or_else(|_| "IFCPROPERTY?".to_string())
            );
            (None, Some(marker))
        }
        _ => return None,
    })
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

/// Flatten a `LIST OF IfcValue`'s members: each through
/// `parse_nominal_value`, values joined with `", "`, type from the first
/// member that has one. `(None, None)` for no members; `(None, type)` when
/// every member is null.
fn parse_value_list(items: &[TypedValue]) -> (Option<String>, Option<String>) {
    let mut values: Vec<String> = Vec::new();
    let mut value_type: Option<String> = None;
    for item in items {
        let (v, t) = parse_nominal_value(Some(item.src));
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

/// Each member's value, nulls kept, so two lists can be paired by
/// position (`IfcPropertyTableValue`'s DefiningValues / DefinedValues).
fn parse_value_list_raw(items: &[TypedValue]) -> Vec<Option<String>> {
    items
        .iter()
        .map(|item| parse_nominal_value(Some(item.src)).0)
        .collect()
}

/// [`parse_value_list_raw`] plus the type of the first member that has one.
fn parse_value_list_with_each(items: &[TypedValue]) -> (Vec<Option<String>>, Option<String>) {
    let mut values: Vec<Option<String>> = Vec::with_capacity(items.len());
    let mut value_type: Option<String> = None;
    for item in items {
        let (v, t) = parse_nominal_value(Some(item.src));
        if value_type.is_none() {
            value_type = t;
        }
        values.push(v);
    }
    (values, value_type)
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
        Field::List(_) | Field::Other(_) => Some(std::str::from_utf8(trimmed).ok()?.to_string()),
    }
}

fn format_number(n: f64) -> String {
    // Avoid "1.0e+00" style for tidy CSV/parquet:
    if n.fract() == 0.0 && n.is_finite() && n.abs() < 1e15 {
        return format!("{}", n as i64);
    }
    format!("{}", n)
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
            .filter_map(|i| t.value[i].as_deref().map(|v| (t.prop_name[i].as_str(), v)))
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

    #[test]
    fn instance_pset_row_is_marked_source_instance() {
        // Baseline: every row from the pre-#36 RelDefinesByProperties
        // path now carries `source = "instance"`.
        let buf = make_buf(
            r#"
#20=IFCPROPERTYSINGLEVALUE('LoadBearing',$,IFCBOOLEAN(.T.),$);
#21=IFCPROPERTYSET('2Pset00000000000000001',$,'Pset_WallCommon',$,(#20));
#22=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#10),#21);
"#,
        );
        let t = run(&buf);
        assert_eq!(t.len(), 1);
        assert_eq!(t.source[0], "instance");
    }

    #[test]
    fn type_inherited_pset_surfaces_on_instance_with_source_type() {
        // The GH #36 repro: a type's HasPropertySets must be attached
        // to every IfcRelDefinesByType-related instance, tagged
        // `source = "type"` so consumers can distinguish provenance.
        //
        // IfcBuildingElementProxy on a proxy type that carries
        // Pset_ManufacturerTypeInformation — the canonical Revit /
        // Tekla "manufacturer lives on the type" pattern.
        let buf = make_buf(
            r#"
#30=IFCPROPERTYSINGLEVALUE('Manufacturer',$,IFCLABEL('Wurth'),$);
#31=IFCPROPERTYSET('4PsetType00000000000001',$,'Pset_ManufacturerTypeInformation',$,(#30));
#32=IFCBUILDINGELEMENTPROXYTYPE('5Type0000000000000001',$,'Wedge Anchor W-FAZ',$,$,(#31),$,$,$,$);
#33=IFCRELDEFINESBYTYPE('6RelType000000000000001',$,$,$,(#10),#32);
"#,
        );
        let t = run(&buf);
        assert_eq!(t.len(), 1, "expected 1 inherited row, got {}", t.len());
        assert_eq!(t.guid[0], "1Wall00000000000000001");
        assert_eq!(t.pset_name[0], "Pset_ManufacturerTypeInformation");
        assert_eq!(t.prop_name[0], "Manufacturer");
        assert_eq!(t.value[0].as_deref(), Some("Wurth"));
        assert_eq!(t.source[0], "type");
    }

    #[test]
    fn bare_type_product_pset_inherits_to_instance() {
        // GH #69: a bare `IFCTYPEPRODUCT` (no `*Type` suffix) is what
        // Revit emits for types lacking a schema-specific subtype. Its
        // HasPropertySets (slot 6) must inherit to every
        // IfcRelDefinesByType-related instance, tagged source="type".
        // Before the fix `is_type_object` rejected the bare base class
        // (ends in PRODUCT, not TYPE) and the pset silently dropped.
        let buf = make_buf(
            r#"
#30=IFCPROPERTYSINGLEVALUE('Manufacturer',$,IFCLABEL('Wurth'),$);
#31=IFCPROPERTYSET('4PsetType00000000000001',$,'Pset_ManufacturerTypeInformation',$,(#30));
#32=IFCTYPEPRODUCT('5Type0000000000000001',$,'Basic Roof',$,$,(#31),$,$);
#33=IFCRELDEFINESBYTYPE('6RelType000000000000001',$,$,$,(#10),#32);
"#,
        );
        let t = run(&buf);
        assert_eq!(t.len(), 1, "expected 1 inherited row, got {}", t.len());
        assert_eq!(t.guid[0], "1Wall00000000000000001");
        assert_eq!(t.pset_name[0], "Pset_ManufacturerTypeInformation");
        assert_eq!(t.prop_name[0], "Manufacturer");
        assert_eq!(t.value[0].as_deref(), Some("Wurth"));
        assert_eq!(t.source[0], "type");
    }

    #[test]
    fn bare_type_object_pset_inherits_to_instance() {
        // GH #69 sibling: the other bare base class `IFCTYPEOBJECT`
        // (HasPropertySets at slot 6) must inherit identically.
        let buf = make_buf(
            r#"
#30=IFCPROPERTYSINGLEVALUE('Manufacturer',$,IFCLABEL('Wurth'),$);
#31=IFCPROPERTYSET('4PsetType00000000000001',$,'Pset_ManufacturerTypeInformation',$,(#30));
#32=IFCTYPEOBJECT('5Type0000000000000001',$,'Generic Type',$,$,(#31));
#33=IFCRELDEFINESBYTYPE('6RelType000000000000001',$,$,$,(#10),#32);
"#,
        );
        let t = run(&buf);
        assert_eq!(t.len(), 1, "expected 1 inherited row, got {}", t.len());
        assert_eq!(t.value[0].as_deref(), Some("Wurth"));
        assert_eq!(t.source[0], "type");
    }

    #[test]
    fn instance_value_shadows_same_named_type_property() {
        // ifcopenshell's `should_inherit=True` semantics: when the
        // instance declares the same (pset_name, prop_name) as its
        // type, the instance value wins. The type row is suppressed
        // (NOT emitted alongside) so consumers see exactly one row
        // per logical property.
        let buf = make_buf(
            r#"
#20=IFCPROPERTYSINGLEVALUE('Manufacturer',$,IFCLABEL('Hilti'),$);
#21=IFCPROPERTYSET('2Pset00000000000000001',$,'Pset_ManufacturerTypeInformation',$,(#20));
#22=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#10),#21);
#30=IFCPROPERTYSINGLEVALUE('Manufacturer',$,IFCLABEL('Wurth'),$);
#31=IFCPROPERTYSET('4PsetType00000000000001',$,'Pset_ManufacturerTypeInformation',$,(#30));
#32=IFCBUILDINGELEMENTPROXYTYPE('5Type0000000000000001',$,'Wedge Anchor W-FAZ',$,$,(#31),$,$,$,$);
#33=IFCRELDEFINESBYTYPE('6RelType000000000000001',$,$,$,(#10),#32);
"#,
        );
        let t = run(&buf);
        assert_eq!(
            t.len(),
            1,
            "instance must shadow type, got {} rows",
            t.len()
        );
        assert_eq!(t.value[0].as_deref(), Some("Hilti"));
        assert_eq!(t.source[0], "instance");
    }

    #[test]
    fn instance_and_type_pset_with_distinct_props_both_surface() {
        // Distinct (pset_name, prop_name) tuples don't collide.
        // Instance row carries source="instance", type row
        // source="type". Order is instance-first by construction.
        let buf = make_buf(
            r#"
#20=IFCPROPERTYSINGLEVALUE('LoadBearing',$,IFCBOOLEAN(.T.),$);
#21=IFCPROPERTYSET('2Pset00000000000000001',$,'Pset_WallCommon',$,(#20));
#22=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#10),#21);
#30=IFCPROPERTYSINGLEVALUE('Manufacturer',$,IFCLABEL('Wurth'),$);
#31=IFCPROPERTYSET('4PsetType00000000000001',$,'Pset_ManufacturerTypeInformation',$,(#30));
#32=IFCBUILDINGELEMENTPROXYTYPE('5Type0000000000000001',$,'Wedge Anchor W-FAZ',$,$,(#31),$,$,$,$);
#33=IFCRELDEFINESBYTYPE('6RelType000000000000001',$,$,$,(#10),#32);
"#,
        );
        let t = run(&buf);
        assert_eq!(t.len(), 2);
        let by_name: std::collections::HashMap<&str, (&str, &str)> = (0..t.len())
            .map(|i| {
                (
                    t.prop_name[i].as_str(),
                    (t.value[i].as_deref().unwrap_or(""), t.source[i].as_str()),
                )
            })
            .collect();
        assert_eq!(by_name.get("LoadBearing"), Some(&("True", "instance")));
        assert_eq!(by_name.get("Manufacturer"), Some(&("Wurth", "type")));
    }

    #[test]
    fn type_with_no_associated_products_emits_no_rows() {
        // An IfcTypeObject with psets but no IfcRelDefinesByType
        // pointing to a product is a no-op. Don't invent rows keyed
        // on the type's GUID — types aren't products.
        let buf = make_buf(
            r#"
#30=IFCPROPERTYSINGLEVALUE('Manufacturer',$,IFCLABEL('Wurth'),$);
#31=IFCPROPERTYSET('4PsetType00000000000001',$,'Pset_ManufacturerTypeInformation',$,(#30));
#32=IFCBUILDINGELEMENTPROXYTYPE('5Type0000000000000001',$,'Orphan Type',$,$,(#31),$,$,$,$);
"#,
        );
        let t = run(&buf);
        assert_eq!(t.len(), 0);
    }

    #[test]
    fn type_inheritance_fans_out_across_multiple_related_instances() {
        // One IfcRelDefinesByType with two RelatedObjects: both
        // products must inherit the type's psets. This is the bulk
        // case on real exports — one IfcWallType backing 200 wall
        // instances.
        let buf = r#"ISO-10303-21;
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
#10=IFCWALL('1Wall00000000000000001',$,'W1',$,$,$,$,'t1',.STANDARD.);
#11=IFCWALL('1Wall00000000000000002',$,'W2',$,$,$,$,'t2',.STANDARD.);
#30=IFCPROPERTYSINGLEVALUE('FireRating',$,IFCLABEL('R60'),$);
#31=IFCPROPERTYSET('4PsetType00000000000001',$,'Pset_WallTypeCommon',$,(#30));
#32=IFCWALLTYPE('5Type0000000000000001',$,'200mm Concrete',$,$,(#31),$,$,$,.STANDARD.);
#33=IFCRELDEFINESBYTYPE('6RelType000000000000001',$,$,$,(#10,#11),#32);
ENDSEC;
END-ISO-10303-21;
"#
        .to_string();
        let t = run(&buf);
        assert_eq!(t.len(), 2);
        let guids: std::collections::HashSet<&str> = t.guid.iter().map(String::as_str).collect();
        assert!(guids.contains("1Wall00000000000000001"));
        assert!(guids.contains("1Wall00000000000000002"));
        for i in 0..t.len() {
            assert_eq!(t.source[i], "type");
            assert_eq!(t.prop_name[i], "FireRating");
        }
    }

    #[test]
    fn property_table_value_serialises_paired_columns() {
        // IfcPropertyTableValue carries two parallel `LIST OF
        // IfcValue`s — DefiningValues (the lookup axis) and
        // DefinedValues (the payload axis). Pre-GH-#38 the whole
        // entity was silently dropped. Post-fix, one row per
        // `(prop_name)` with the table serialised as
        // `"d1=>v1, d2=>v2, ..."` and value_type carrying the payload
        // axis type (DefinedValues — what the consumer asked for).
        //
        // Real-world use: MEP fitting curves (flow rate → pressure
        // drop), geotechnical reports (depth → bearing capacity).
        let buf = make_buf(
            r#"
#20=IFCPROPERTYTABLEVALUE('PressureDrop',$,(IFCREAL(0.1),IFCREAL(0.2),IFCREAL(0.4)),(IFCREAL(5.),IFCREAL(20.),IFCREAL(80.)),$,$,$,$);
#21=IFCPROPERTYSET('2Pset00000000000000001',$,'Pset_DuctFitting',$,(#20));
#22=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#10),#21);
"#,
        );
        let t = run(&buf);
        assert_eq!(t.len(), 1);
        assert_eq!(t.prop_name[0], "PressureDrop");
        assert_eq!(t.value[0].as_deref(), Some("0.1=>5, 0.2=>20, 0.4=>80"));
        // value_type follows the payload axis (DefinedValues) — what
        // a downstream consumer would actually read out of the
        // lookup.
        assert_eq!(t.value_type[0].as_deref(), Some("IfcReal"));
    }

    #[test]
    fn unknown_property_class_surfaces_as_unhandled_marker() {
        // GH #38 spec: a property class ifcfast doesn't know how to
        // parse must NOT be silently dropped — emit a row tagged
        // `value_type = "unhandled:IFCXXX"` so consumers can detect
        // the blind spot and (eventually) report or work around it.
        //
        // IfcPropertyReferenceValue is a real IFC4 IfcSimpleProperty
        // subclass that ifcfast doesn't yet parse — used by structural
        // exports to point at a named curve / list / table. It matches
        // the `IFCPROPERTY*VALUE` shape that triggers the unhandled
        // arm.
        let buf = make_buf(
            r#"
#20=IFCPROPERTYREFERENCEVALUE('CurveRef',$,$,#5);
#21=IFCPROPERTYSET('2Pset00000000000000001',$,'Pset_Custom',$,(#20));
#22=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#10),#21);
"#,
        );
        let t = run(&buf);
        assert_eq!(t.len(), 1);
        assert_eq!(t.prop_name[0], "CurveRef");
        assert_eq!(t.value[0], None);
        assert_eq!(
            t.value_type[0].as_deref(),
            Some("unhandled:IFCPROPERTYREFERENCEVALUE")
        );
        assert_eq!(t.source[0], "instance");
    }

    #[test]
    fn ifcpropertysettemplate_does_not_misclassify_as_unhandled() {
        // The unhandled fallback's filter is `IFCPROPERTY*` prefix +
        // `VALUE` suffix. IfcPropertySetTemplate / IfcPropertyTemplate
        // / IfcPropertyEnumeration etc. share the prefix but aren't
        // properties — they must NOT trigger a marker row even if
        // they happen to appear in a file. This test pins the
        // `*VALUE` suffix rule by introducing an
        // IfcPropertySetTemplate next to a real handled property.
        let buf = make_buf(
            r#"
#15=IFCPROPERTYSETTEMPLATE('4Tpl000000000000000001',$,'Pset_WallCommon_Template',.PSET_TYPEDRIVENONLY.,'IfcWall',$,$);
#20=IFCPROPERTYSINGLEVALUE('IsExternal',$,IFCBOOLEAN(.T.),$);
#21=IFCPROPERTYSET('2Pset00000000000000001',$,'Pset_WallCommon',$,(#20));
#22=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#10),#21);
"#,
        );
        let t = run(&buf);
        // Exactly one row from the IfcPropertySingleValue — the
        // IfcPropertySetTemplate must not produce a phantom unhandled
        // marker row.
        assert_eq!(t.len(), 1);
        assert_eq!(t.prop_name[0], "IsExternal");
        assert!(
            !t.value_type[0]
                .as_deref()
                .unwrap_or("")
                .starts_with("unhandled:"),
            "got unexpected unhandled marker: {:?}",
            t.value_type[0]
        );
    }

    #[test]
    fn type_inheritance_works_on_ifc2x3_doorstyle() {
        // IFC2x3 collapsed `IfcDoorType` into `IfcDoorStyle` (and
        // similarly for windows). These don't follow the "IFCxxxTYPE"
        // suffix rule but ARE valid RelatingType targets on 2x3 files.
        // The indexer's TypeObject classifier has a special case for
        // them; the pset extractor must mirror it or 100% of door/window
        // typing leaks silently on the IFC2x3 long-tail.
        let buf = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [CoordinationView]'),'2;1');
FILE_NAME('psets_test.ifc','2026-05-26T00:00:00',('test'),('test'),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC2X3'));
ENDSEC;
DATA;
#1=IFCPROJECT('0Test000000000000000001',$,'p',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#3));
#3=IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.);
#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);
#6=IFCAXIS2PLACEMENT3D(#7,$,$);
#7=IFCCARTESIANPOINT((0.,0.,0.));
#10=IFCDOOR('1Door00000000000000001',$,'D',$,$,$,$,'t',2100.,900.);
#30=IFCPROPERTYSINGLEVALUE('FireRating',$,IFCLABEL('EI60'),$);
#31=IFCPROPERTYSET('4PsetType00000000000001',$,'Pset_DoorCommon',$,(#30));
#32=IFCDOORSTYLE('5Type0000000000000001',$,'Office Door',$,$,(#31),$,$,.NOTDEFINED.,.NOTDEFINED.,.F.,.F.);
#33=IFCRELDEFINESBYTYPE('6RelType000000000000001',$,$,$,(#10),#32);
ENDSEC;
END-ISO-10303-21;
"#.to_string();
        let t = run(&buf);
        assert_eq!(t.len(), 1);
        assert_eq!(t.guid[0], "1Door00000000000000001");
        assert_eq!(t.value[0].as_deref(), Some("EI60"));
        assert_eq!(t.source[0], "type");
    }

    #[test]
    fn set_valued_relating_property_definition_inline_list() {
        // GH #76 item 5. IFC4 RelatingPropertyDefinition can be an
        // IfcPropertySetDefinitionSet — an inline list of pset refs
        // `((#21,#23))`. Pre-fix the non-Ref field hit `_ => continue` and
        // BOTH psets dropped (zero rows). Both must bind to the product.
        let buf = make_buf(
            r#"
#20=IFCPROPERTYSINGLEVALUE('LoadBearing',$,IFCBOOLEAN(.T.),$);
#21=IFCPROPERTYSET('2Pset00000000000000001',$,'Pset_A',$,(#20));
#22=IFCPROPERTYSINGLEVALUE('FireRating',$,IFCLABEL('EI60'),$);
#23=IFCPROPERTYSET('4Pset00000000000000001',$,'Pset_B',$,(#22));
#25=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#10),(#21,#23));
"#,
        );
        let t = run(&buf);
        assert_eq!(t.len(), 2, "both psets in the set must surface");
        let by_pset: std::collections::HashMap<&str, &str> = (0..t.len())
            .map(|i| (t.pset_name[i].as_str(), t.prop_name[i].as_str()))
            .collect();
        assert_eq!(by_pset.get("Pset_A"), Some(&"LoadBearing"));
        assert_eq!(by_pset.get("Pset_B"), Some(&"FireRating"));
    }

    #[test]
    fn set_valued_relating_property_definition_typed_wrapper() {
        // GH #76 item 5. The typed `IFCPROPERTYSETDEFINITIONSET((...))`
        // wrapper form of the same set.
        let buf = make_buf(
            r#"
#20=IFCPROPERTYSINGLEVALUE('LoadBearing',$,IFCBOOLEAN(.T.),$);
#21=IFCPROPERTYSET('2Pset00000000000000001',$,'Pset_A',$,(#20));
#22=IFCPROPERTYSINGLEVALUE('FireRating',$,IFCLABEL('EI60'),$);
#23=IFCPROPERTYSET('4Pset00000000000000001',$,'Pset_B',$,(#22));
#25=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#10),IFCPROPERTYSETDEFINITIONSET((#21,#23)));
"#,
        );
        let t = run(&buf);
        assert_eq!(t.len(), 2, "typed-wrapper set must surface both psets");
        let by_pset: std::collections::HashMap<&str, &str> = (0..t.len())
            .map(|i| (t.pset_name[i].as_str(), t.prop_name[i].as_str()))
            .collect();
        assert_eq!(by_pset.get("Pset_A"), Some(&"LoadBearing"));
        assert_eq!(by_pset.get("Pset_B"), Some(&"FireRating"));
    }

    #[test]
    fn single_ref_relating_property_definition_still_works() {
        // Regression guard for GH #76 item 5: the common bare-ref form
        // must keep working after the set-valued tolerance was added.
        let buf = make_buf(
            r#"
#20=IFCPROPERTYSINGLEVALUE('LoadBearing',$,IFCBOOLEAN(.T.),$);
#21=IFCPROPERTYSET('2Pset00000000000000001',$,'Pset_A',$,(#20));
#22=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#10),#21);
"#,
        );
        let t = run(&buf);
        assert_eq!(t.len(), 1);
        assert_eq!(t.pset_name[0], "Pset_A");
        assert_eq!(t.prop_name[0], "LoadBearing");
    }
}
