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
//!
//! Discovery is the shared typed pass in [`super::property_graph`]; this
//! module is the flattened public view of it, with its row order, number
//! formatting, SI-only `unit_step_id` fallback and marker rows unchanged.

use std::collections::{HashMap, HashSet};

#[cfg(test)]
use super::property_graph::is_unhandled_quantity;
use super::property_graph::{GraphScope, PropClass, PropDef, PropertyGraph, PsetKind};
use crate::entity_table::EntityTable;
#[cfg(test)]
use crate::lexer::split_top_level_args;
use crate::lexer::{parse_field, Field};

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

pub fn build(table: &EntityTable, product_step_to_guid: &HashMap<u64, String>) -> QuantityTable {
    let graph = PropertyGraph::build_scoped(table, GraphScope::QUANTITIES);
    build_from_graph(&graph, product_step_to_guid)
}

/// [`build`] from an already-built graph (which must include
/// [`GraphScope::quantities`]).
///
/// Rows: every `(object, IfcElementQuantity)` pair of
/// `IfcRelDefinesByProperties` in relation order, one row per leaf
/// quantity (`IfcPhysicalComplexQuantity` members flatten to
/// `Wrapper.Width`, GH #76 item 6); then, by product step id, the type's
/// `HasPropertySets` quantities not shadowed by an instance row on the
/// same guid (GH #45, instance wins).
///
/// `unit_step_id` is the quantity's own `Unit`, else the project
/// `IfcSIUnit` of its kind (GH #43). `IfcConversionBasedUnit` /
/// `IfcDerivedUnit` project units are not a fallback target here; the
/// general resolver is `crate::units::UnitTable`.
pub fn build_from_graph(
    graph: &PropertyGraph,
    product_step_to_guid: &HashMap<u64, String>,
) -> QuantityTable {
    assert!(
        graph.scope.quantities,
        "quantities::build_from_graph needs a PropertyGraph built with quantities in scope"
    );
    let mut out = QuantityTable::default();

    // (guid → "qto_name\tquantity_name" keys emitted on the instance
    // side), so same-named type quantities are suppressed: instance wins,
    // ifcopenshell `should_inherit=True` (GH #36 / #45).
    let mut seen_per_product: HashMap<&str, HashSet<String>> =
        HashMap::with_capacity(product_step_to_guid.len());

    for (obj_step_id, set_step_id) in &graph.defines {
        // The same relation type also points at IfcPropertySet; only
        // IfcElementQuantity targets count here.
        let set = match graph.sets.get(set_step_id) {
            Some(s) if s.kind == PsetKind::ElementQuantity => s,
            _ => continue,
        };
        let guid = match product_step_to_guid.get(obj_step_id) {
            Some(g) => g.as_str(),
            None => continue,
        };
        let mut emitted_names: Vec<String> = Vec::new();
        graph.walk_set_leaves(set, &mut |path, def| {
            let name = quantity_name(path, &def.name);
            push_row(
                &mut out,
                graph,
                guid,
                &set.name,
                name.clone(),
                def,
                "instance",
            );
            emitted_names.push(name);
        });
        if !emitted_names.is_empty() {
            let seen = seen_per_product.entry(guid).or_default();
            for name in emitted_names {
                seen.insert(format!("{}\t{name}", set.name));
            }
        }
    }

    // Type inheritance. Sorted by product step id (GH #152: HashMap order
    // leaked into the row order).
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
            let candidate_ids = match graph.type_sets.get(type_step_id) {
                Some(v) => v,
                None => continue,
            };
            let already_seen = seen_per_product.get(guid).unwrap_or(&empty);
            for set_id in candidate_ids {
                // HasPropertySets also lists IfcPropertySets (psets.rs).
                let set = match graph.sets.get(set_id) {
                    Some(s) if s.kind == PsetKind::ElementQuantity => s,
                    _ => continue,
                };
                graph.walk_set_leaves(set, &mut |path, def| {
                    let name = quantity_name(path, &def.name);
                    if already_seen.contains(&format!("{}\t{name}", set.name)) {
                        return;
                    }
                    push_row(&mut out, graph, guid, &set.name, name, def, "type");
                });
            }
        }
    }

    out
}

/// One row for a leaf quantity definition.
fn push_row(
    out: &mut QuantityTable,
    graph: &PropertyGraph,
    guid: &str,
    qto_name: &str,
    name: String,
    def: &PropDef,
    source: &str,
) {
    let (kind, value) = match quantity_kind(def.class) {
        Some(kind) => {
            let value = match def.values.first().map(|v| parse_field(v.src)) {
                Some(Field::Number(n)) => Some(format_number(n)),
                _ => None,
            };
            (kind.to_string(), value)
        }
        // An IfcQuantity* class we can't parse (IFC4X3 IfcQuantityNumber,
        // …): value null, quantity_type carries the marker so "no
        // quantity authored" and "ifcfast can't read this" stay distinct
        // (GH #159).
        None => (
            format!(
                "unhandled:{}",
                std::str::from_utf8(def.entity)
                    .map(|s| s.to_ascii_uppercase())
                    .unwrap_or_else(|_| "IFCQUANTITY?".to_string())
            ),
            None,
        ),
    };
    let unit = def.unit_step.or_else(|| {
        unit_type_for_quantity_class(def.class)
            .and_then(|ut| graph.quantity_default_units.get(ut).copied())
    });
    out.guid.push(guid.to_string());
    out.qto_name.push(qto_name.to_string());
    out.quantity_name.push(name);
    out.value.push(value);
    out.quantity_type.push(kind);
    out.unit_step_id.push(unit);
    out.source.push(source.to_string());
}

/// Dot-join the enclosing complex names and the leaf name. An unnamed
/// wrapper contributes nothing (`join_name("", x) == x`), unlike the pset
/// side's bare `.`; both are long-standing and kept.
fn quantity_name(path: &[&str], leaf: &str) -> String {
    let mut prefix = String::new();
    for p in path {
        prefix = join_name(&prefix, p);
    }
    join_name(&prefix, leaf)
}

/// Dot-join a name prefix with a leaf name (`""` prefix → leaf as-is).
fn join_name(prefix: &str, leaf: &str) -> String {
    if prefix.is_empty() {
        leaf.to_string()
    } else {
        format!("{prefix}.{leaf}")
    }
}

/// The `quantity_type` column value of a recognised quantity class.
fn quantity_kind(class: PropClass) -> Option<&'static str> {
    Some(match class {
        PropClass::QuantityArea => "Area",
        PropClass::QuantityLength => "Length",
        PropClass::QuantityVolume => "Volume",
        PropClass::QuantityCount => "Count",
        PropClass::QuantityWeight => "Weight",
        PropClass::QuantityTime => "Time",
        _ => return None,
    })
}

/// The `IfcUnitEnum` literal a quantity class falls back to. `Count` is
/// dimensionless: a null `unit_step_id` is the correct, terminal answer.
fn unit_type_for_quantity_class(class: PropClass) -> Option<&'static str> {
    match class {
        PropClass::QuantityLength => Some("LENGTHUNIT"),
        PropClass::QuantityArea => Some("AREAUNIT"),
        PropClass::QuantityVolume => Some("VOLUMEUNIT"),
        PropClass::QuantityWeight => Some("MASSUNIT"),
        PropClass::QuantityTime => Some("TIMEUNIT"),
        _ => None,
    }
}

fn format_number(n: f64) -> String {
    if n.fract() == 0.0 && n.is_finite() && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{}", n)
    }
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
        let buf = r#"ISO-10303-21;
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
        .to_string();
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
        assert_eq!(
            t.len(),
            1,
            "expected one inherited quantity, got {}",
            t.len()
        );
        assert_eq!(t.guid[0], "1Wall00000000000000001");
        assert_eq!(t.qto_name[0], "Qto_TypeBase");
        assert_eq!(t.quantity_name[0], "GrossWeight");
        assert_eq!(t.value[0].as_deref(), Some("42.5"));
        assert_eq!(t.source[0], "type");
    }

    #[test]
    fn bare_type_product_quantity_inherits_to_instance() {
        // GH #69 — quantity-side of the bare-base-class drop. A bare
        // `IFCTYPEPRODUCT` (HasPropertySets at slot 6) carrying an
        // IfcElementQuantity must inherit to its related instance,
        // tagged source="type". Before the fix `is_type_object`
        // rejected the bare base class and the quantity dropped.
        let buf = make_buf(
            r#"
#30=IFCQUANTITYWEIGHT('GrossWeight',$,$,42.5);
#31=IFCELEMENTQUANTITY('4Qto0000000000000001',$,'Qto_TypeBase',$,$,(#30));
#32=IFCTYPEPRODUCT('5Type0000000000000001',$,'Basic Roof',$,$,(#31),$,$);
#33=IFCRELDEFINESBYTYPE('6RelType000000000000001',$,$,$,(#10),#32);
"#,
        );
        let t = run(&buf);
        assert_eq!(
            t.len(),
            1,
            "expected one inherited quantity, got {}",
            t.len()
        );
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
        let buf = r#"ISO-10303-21;
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
        .to_string();
        let t = run(&buf);
        assert_eq!(t.len(), 2);
        let guids: std::collections::HashSet<&str> = t.guid.iter().map(String::as_str).collect();
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
    fn dangling_dup_si_unit_does_not_clobber_assigned_default() {
        // GH #76 item 4. Two SIUnits share UnitType LENGTHUNIT: #3 is the
        // one the IfcUnitAssignment references; #80 is a dangling duplicate
        // declared AFTER it (e.g. nested in an unresolved
        // IfcConversionBasedUnit). Pre-fix the last-write-wins map let #80
        // overwrite #3, then the membership filter dropped #80 (not
        // assigned) — leaving unit_step_id=None. The assigned #3 must win.
        let buf = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('qto_test.ifc','2026-06-13T00:00:00',('test'),('test'),'ifcfast','ifcfast','');
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
#41=IFCQUANTITYLENGTH('Length',$,$,3.0);
#47=IFCELEMENTQUANTITY('2Qto000000000000000001',$,'Qto_X',$,$,(#41));
#48=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#10),#47);
#80=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);
ENDSEC;
END-ISO-10303-21;
"#
        .to_string();
        let t = run(&buf);
        assert_eq!(t.len(), 1);
        // The assigned SIUnit #3 must resolve, not None and not #80.
        assert_eq!(t.unit_step_id[0], Some(3));
    }

    #[test]
    fn set_valued_relating_property_definition_inline_list() {
        // GH #76 item 5. RelatingPropertyDefinition given as an inline
        // IfcPropertySetDefinitionSet list `((#21,#23))`. Both element
        // quantities must bind to the product. Pre-fix the non-Ref field
        // hit `_ => continue` and zero rows came through.
        let buf = make_buf(
            r#"
#20=IFCQUANTITYAREA('NetArea',$,$,12.5);
#21=IFCELEMENTQUANTITY('2Qto000000000000000001',$,'Qto_A',$,$,(#20));
#22=IFCQUANTITYVOLUME('NetVolume',$,$,2.5);
#23=IFCELEMENTQUANTITY('4Qto000000000000000001',$,'Qto_B',$,$,(#22));
#25=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#10),(#21,#23));
"#,
        );
        let t = run(&buf);
        assert_eq!(t.len(), 2, "both element quantities in the set must bind");
        let names: std::collections::HashSet<&str> =
            t.quantity_name.iter().map(String::as_str).collect();
        assert!(names.contains("NetArea"));
        assert!(names.contains("NetVolume"));
    }

    #[test]
    fn set_valued_relating_property_definition_typed_wrapper() {
        // GH #76 item 5. The typed `IFCPROPERTYSETDEFINITIONSET((...))`
        // wrapper form.
        let buf = make_buf(
            r#"
#20=IFCQUANTITYAREA('NetArea',$,$,12.5);
#21=IFCELEMENTQUANTITY('2Qto000000000000000001',$,'Qto_A',$,$,(#20));
#22=IFCQUANTITYVOLUME('NetVolume',$,$,2.5);
#23=IFCELEMENTQUANTITY('4Qto000000000000000001',$,'Qto_B',$,$,(#22));
#25=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#10),IFCPROPERTYSETDEFINITIONSET((#21,#23)));
"#,
        );
        let t = run(&buf);
        assert_eq!(t.len(), 2, "typed-wrapper set must bind both quantities");
        let names: std::collections::HashSet<&str> =
            t.quantity_name.iter().map(String::as_str).collect();
        assert!(names.contains("NetArea"));
        assert!(names.contains("NetVolume"));
    }

    #[test]
    fn physical_complex_quantity_nested_members_surface() {
        // GH #76 item 6. An IfcPhysicalComplexQuantity bundling two nested
        // simple quantities. Pre-fix the whole complex was dropped and the
        // nested `Width`/`Height` vanished. Now they surface as
        // dot-prefixed rows `Profile.Width` / `Profile.Height`.
        let buf = make_buf(
            r#"
#18=IFCQUANTITYLENGTH('Width',$,$,0.3);
#19=IFCQUANTITYLENGTH('Height',$,$,0.6);
#20=IFCPHYSICALCOMPLEXQUANTITY('Profile',$,(#18,#19),'shape','geometry',$);
#21=IFCQUANTITYAREA('NetArea',$,$,12.5);
#24=IFCELEMENTQUANTITY('2Qto000000000000000001',$,'Qto_Wall',$,$,(#20,#21));
#25=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#10),#24);
"#,
        );
        let t = run(&buf);
        let by_name: std::collections::HashMap<&str, Option<&str>> = (0..t.len())
            .map(|i| (t.quantity_name[i].as_str(), t.value[i].as_deref()))
            .collect();
        // Sibling plain quantity still there.
        assert_eq!(by_name.get("NetArea"), Some(&Some("12.5")));
        // Nested members surface dot-prefixed with the wrapper name.
        assert_eq!(by_name.get("Profile.Width"), Some(&Some("0.3")));
        assert_eq!(by_name.get("Profile.Height"), Some(&Some("0.6")));
        assert_eq!(t.len(), 3);
    }

    #[test]
    fn unassigned_si_unit_does_not_leak_as_project_default() {
        // A file that declares an SIUnit but doesn't reference it from
        // any IfcUnitAssignment (e.g. a dangling SIUnit nested inside
        // a never-resolved IfcConversionBasedUnit). That SIUnit must
        // NOT silently become the project default — fallback stays
        // None.
        let buf = r#"ISO-10303-21;
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
        .to_string();
        let t = run(&buf);
        assert_eq!(t.len(), 1);
        // SIUnit #3 exists but isn't referenced by any
        // IfcUnitAssignment, so it doesn't qualify as a project
        // default for fallback.
        assert_eq!(t.unit_step_id[0], None);
    }
}

#[cfg(test)]
mod unhandled_quantity_tests {
    use super::*;
    use std::collections::HashMap;

    const BUF_HEAD: &str = "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION((''),'2;1');\n\
FILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n\
#10=IFCWALL('1Wall00000000000000001',$,'W',$,$,$,$,'t',.STANDARD.);\n";

    fn run(data: &str) -> QuantityTable {
        let buf = format!("{BUF_HEAD}{data}ENDSEC;\nEND-ISO-10303-21;\n");
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

    /// GH #159: an IfcQuantity* class we can't parse must surface a
    /// marker row (mirroring psets.rs's `unhandled:IFCXXX`), not vanish.
    /// A consumer has to be able to tell "no quantity authored" from
    /// "ifcfast has a blind spot here".
    #[test]
    fn unrecognised_quantity_class_emits_marker_row() {
        let t = run("#20=IFCQUANTITYAREA('NetArea',$,$,12.5);\n\
             #21=IFCQUANTITYNUMBER('Pieces',$,$,7.);\n\
             #24=IFCELEMENTQUANTITY('2Qto000000000000000001',$,'Qto_Test',$,$,(#20,#21));\n\
             #25=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#10),#24);\n");
        assert_eq!(
            t.len(),
            2,
            "both the known and the unknown quantity emit a row"
        );
        let i = t
            .quantity_name
            .iter()
            .position(|n| n == "Pieces")
            .expect("the unhandled quantity must still produce a row");
        assert_eq!(t.quantity_type[i], "unhandled:IFCQUANTITYNUMBER");
        assert_eq!(t.value[i], None, "an unparsed quantity has no value");
        assert_eq!(t.unit_step_id[i], None);
        // The recognised one is untouched.
        let j = 1 - i;
        assert_eq!(t.quantity_type[j], "Area");
        assert_eq!(t.value[j].as_deref(), Some("12.5"));
    }

    #[test]
    fn is_unhandled_quantity_rule() {
        assert!(is_unhandled_quantity(b"IFCQUANTITYNUMBER"));
        assert!(!is_unhandled_quantity(b"IFCQUANTITYAREA"));
        assert!(!is_unhandled_quantity(b"IFCQUANTITY"));
        assert!(!is_unhandled_quantity(b"IFCPHYSICALCOMPLEXQUANTITY"));
        assert!(!is_unhandled_quantity(b"IFCELEMENTQUANTITY"));
    }
}
