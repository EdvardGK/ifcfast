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
//!     (guid, qto_name, quantity_name, value, quantity_type, unit)
//!
//! Where:
//!   `qto_name`       = e.g. "Qto_WallBaseQuantities"
//!   `quantity_name`  = e.g. "NetArea", "Length", "GrossVolume"
//!   `value`          = numeric value as string (downstream parses to f64)
//!   `quantity_type`  = "Area" | "Length" | "Volume" | "Count" |
//!                      "Weight" | "Time"
//!   `unit`           = step_id of the IfcUnit (often null; the project's
//!                      default unit assignment applies)

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
    object_step_to_guid: &HashMap<u64, String>,
) -> QuantityTable {
    // Pass 1: index IfcElementQuantity records and their physical quantity refs.
    //         Also index each physical-simple-quantity record.
    let mut qtos: HashMap<u64, (String, Vec<u64>)> = HashMap::with_capacity(1024);
    let mut quantities: HashMap<u64, Quantity> = HashMap::with_capacity(8192);
    let mut rel_pairs: Vec<(u64, u64)> = Vec::with_capacity(8192);

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
        }
    }

    let mut out = QuantityTable::default();
    for (obj_step_id, rel_obj_id) in rel_pairs {
        // Only act on rels that point at IfcElementQuantity (the same
        // rel type also points at IfcPropertySet — we filter via the
        // qtos table).
        let (qto_name, qty_ids) = match qtos.get(&rel_obj_id) {
            Some(x) => x,
            None => continue,
        };
        let guid = match object_step_to_guid.get(&obj_step_id) {
            Some(g) => g,
            None => continue,
        };
        for qid in qty_ids {
            let q = match quantities.get(qid) {
                Some(q) => q,
                None => continue,
            };
            out.guid.push(guid.clone());
            out.qto_name.push(qto_name.clone());
            out.quantity_name.push(q.name.clone());
            out.value.push(q.value.clone());
            out.quantity_type.push(q.kind.to_string());
            out.unit_step_id.push(q.unit_step_id);
        }
    }

    out
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
        let table = crate::entity_table::EntityTable::build_from_slice(buf.as_bytes());
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
}
