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
    product_step_to_guid: &HashMap<u64, String>,
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
        let guid = match product_step_to_guid.get(&obj_step_id) {
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
    match parse_field(*fields.get(idx)?) {
        Field::String(s) => Some(s),
        _ => None,
    }
}

fn number_at(fields: &[&[u8]], idx: usize) -> Option<f64> {
    match parse_field(*fields.get(idx)?) {
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
