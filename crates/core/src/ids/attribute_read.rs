//! Read IFC entity attributes from STEP via EXPRESS indices (IfcTester-aligned).

use crate::entity_table::EntityTable;
use crate::lexer::{parse_field, split_top_level_args, Field};

use super::attribute_schema;
use super::compiled::ValueConstraint;
use super::entity_schema::entity_is_subtype_or_equal;
use super::restrictions::value_matches;

/// `IFCNORMALISEDRATIOMEASURE(0.5)` and similar wrapped types.
fn wrapped_measure_string(raw: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(raw).ok()?.trim();
    if !s.starts_with("IFC") {
        return None;
    }
    let open = s.find('(')?;
    let close = s.rfind(')')?;
    if close <= open + 1 {
        return None;
    }
    let inner = trim_ascii(s[open + 1..close].as_bytes());
    field_to_ids_string(parse_field(inner))
}

fn trim_ascii(s: &[u8]) -> &[u8] {
    let mut start = 0usize;
    while start < s.len() && s[start].is_ascii_whitespace() {
        start += 1;
    }
    let mut end = s.len();
    while end > start && s[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    &s[start..end]
}

/// Serialize a STEP field to an IDS/IfcTester comparison string.
pub fn field_to_ids_string(field: Field<'_>) -> Option<String> {
    match field {
        Field::Null | Field::Star => None,
        Field::String(s) => {
            if s.is_empty() || s == "UNKNOWN" {
                None
            } else {
                Some(s)
            }
        }
        Field::Ref(id) => Some(format!("#{id}")),
        Field::Number(n) => {
            if n.fract() == 0.0 && n.is_finite() && n.abs() < 9e15 {
                Some(format!("{}", n as i64))
            } else {
                Some(n.to_string())
            }
        }
        Field::Enum(e) => {
            let name = std::str::from_utf8(e).ok()?.to_ascii_uppercase();
            match name.as_str() {
                "T" | "TRUE" => Some("true".into()),
                "F" | "FALSE" => Some("false".into()),
                "U" | "UNKNOWN" => Some("UNKNOWN".into()),
                _ => Some(name),
            }
        }
        Field::List(body) => {
            if body.iter().all(|b| b.is_ascii_whitespace()) {
                return None;
            }
            let parts: Vec<String> = split_top_level_args(body)
                .into_iter()
                .filter_map(|f| field_to_ids_string(parse_field(f)))
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join(","))
            }
        }
        Field::Other(raw) => wrapped_measure_string(raw),
    }
}

pub fn read_attribute_at_index(fields: &[&[u8]], index: usize) -> Option<String> {
    let raw = fields.get(index)?;
    field_to_ids_string(parse_field(raw))
}

pub fn read_entity_attribute(
    table: &EntityTable,
    schema: &str,
    step_id: u64,
    entity_upper: &str,
    attr_name: &str,
) -> Option<String> {
    let idx = attribute_schema::attribute_index(schema, entity_upper, attr_name)? as usize;
    let (_, args) = table.get(step_id)?;
    let fields = split_top_level_args(args);
    read_attribute_at_index(&fields, idx)
}

pub fn matching_attribute_names(
    schema: &str,
    entity_upper: &str,
    facet_name: Option<&str>,
    facet_names: &[String],
    name_constraint: Option<&ValueConstraint>,
) -> Vec<String> {
    if let Some(n) = facet_name {
        return vec![n.to_string()];
    }
    if !facet_names.is_empty() {
        return facet_names.to_vec();
    }
    if let Some(c) = name_constraint {
        return attribute_schema::entity_attribute_name_list(schema, entity_upper)
            .into_iter()
            .filter(|n| value_matches(Some(n.as_str()), Some(c)))
            .collect();
    }
    vec!["Name".into()]
}

pub fn global_id_from_record(
    table: &EntityTable,
    schema: &str,
    step_id: u64,
    entity_upper: &str,
    args: &[u8],
) -> String {
    if let Some(g) = read_entity_attribute(table, schema, step_id, entity_upper, "GlobalId") {
        return g;
    }
    let fields = split_top_level_args(args);
    if let Some(idx) = attribute_schema::attribute_index(schema, entity_upper, "GlobalId") {
        if let Some(g) = read_attribute_at_index(&fields, idx as usize) {
            return g;
        }
    }
    format!("#{step_id}")
}

pub fn entity_type_matches_any(schema_entity: &str, wanted: &[String]) -> bool {
    for w in wanted {
        if entity_is_subtype_or_equal(schema_entity, w) {
            return true;
        }
    }
    false
}
