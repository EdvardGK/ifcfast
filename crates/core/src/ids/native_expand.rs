//! Expand IDS validation pool and attribute index from [`EntityTable`] (no IfcOpenShell).

use std::collections::{HashMap, HashSet};

use crate::entity_table::EntityTable;
use crate::indexer::IndexedFile;
use crate::lexer::split_top_level_args;

use super::attribute_read::{
    entity_type_matches_any, global_id_from_record, matching_attribute_names,
    read_attribute_at_index,
};
use super::attribute_schema;
use super::compiled::{CompiledFacet, CompiledIds, FacetKind};
use super::context::push_validation_object;
use super::extract_needs::ExtractNeeds;
use crate::object_guid::material_step_guid;

/// Entity+attribute-only IDS: skip full-pool hydrate/attr scan when extractors are unused.
pub fn overlay_can_scope(needs: &ExtractNeeds, ids: &CompiledIds) -> bool {
    if needs.any() {
        return false;
    }
    !collect_ids_entity_types(ids).is_empty()
}

pub fn scoped_entity_type_filter(ids: &CompiledIds) -> Vec<String> {
    collect_ids_entity_types(ids).into_iter().collect()
}

pub fn collect_ids_entity_types(ids: &CompiledIds) -> HashSet<String> {
    let mut types = HashSet::new();
    for spec in &ids.specifications {
        for facet in spec
            .applicability
            .iter()
            .chain(spec.requirements.iter())
        {
            if facet.kind == FacetKind::Entity {
                for n in &facet.entity_names {
                    types.insert(n.to_ascii_uppercase());
                }
            }
        }
    }
    types
}

/// Entity types already represented in the tier-1 index / base pool.
fn type_covered_by_indexed(entity_upper: &str, indexed: &IndexedFile) -> bool {
    match entity_upper {
        "IFCBUILDINGSTOREY" | "IFCSPACE" | "IFCBUILDING" | "IFCSITE" | "IFCPROJECT" => true,
        "IFCTYPEOBJECT" | "IFCGROUP" => true,
        _ => indexed
            .product_entity
            .iter()
            .any(|e| e.eq_ignore_ascii_case(entity_upper)),
    }
}

pub fn expand_ids_entities_from_table(
    table: &EntityTable,
    indexed: &IndexedFile,
    schema: &str,
    ids: &CompiledIds,
    guid_to_ix: &mut HashMap<String, u32>,
    object_guid: &mut Vec<String>,
    object_entity_upper: &mut Vec<String>,
    object_name: &mut Vec<Option<String>>,
    object_description: &mut Vec<Option<String>>,
    object_tag: &mut Vec<Option<String>>,
    object_predefined_type: &mut Vec<Option<String>>,
    object_object_type: &mut Vec<Option<String>>,
    object_step_id: &mut Vec<u64>,
) {
    let types = collect_ids_entity_types(ids);
    if types.is_empty() {
        return;
    }

    let scan_types: Vec<String> = types
        .iter()
        .filter(|t| !type_covered_by_indexed(t, indexed))
        .cloned()
        .collect();

    if scan_types.is_empty() {
        return;
    }

    let wanted = scan_types;
    for (step_id, type_bytes, args) in table.iter() {
        let Ok(ent) = std::str::from_utf8(type_bytes) else {
            continue;
        };
        if !entity_type_matches_any(ent, &wanted) {
            continue;
        }
        let ent_up = ent.to_ascii_uppercase();
        let guid = if ent_up == "IFCMATERIAL" {
            material_step_guid(step_id)
        } else {
            global_id_from_record(table, schema, step_id, &ent_up, args)
        };
        if guid_to_ix.contains_key(&guid) {
            continue;
        }
        let fields = split_top_level_args(args);
        let name = read_named_attr(schema, &ent_up, &fields, "Name");
        let description = read_named_attr(schema, &ent_up, &fields, "Description");
        let tag = read_named_attr(schema, &ent_up, &fields, "Tag");
        let ptype = read_named_attr(schema, &ent_up, &fields, "PredefinedType");
        let otype = read_named_attr(schema, &ent_up, &fields, "ObjectType");
        push_validation_object(
            guid_to_ix,
            object_guid,
            object_entity_upper,
            object_name,
            object_description,
            object_tag,
            object_predefined_type,
            object_object_type,
            object_step_id,
            guid,
            &ent_up,
            name,
            description,
            tag,
            ptype,
            otype,
            step_id,
        );
    }
}

fn read_named_attr(
    schema: &str,
    entity_upper: &str,
    fields: &[&[u8]],
    attr: &str,
) -> Option<String> {
    let idx = attribute_schema::attribute_index(schema, entity_upper, attr)? as usize;
    read_attribute_at_index(fields, idx)
}

fn attribute_facets<'a>(ids: &'a CompiledIds) -> Vec<&'a CompiledFacet> {
    let mut out = Vec::new();
    for spec in &ids.specifications {
        for facet in spec
            .applicability
            .iter()
            .chain(spec.requirements.iter())
        {
            if facet.kind == FacetKind::Attribute {
                out.push(facet);
            }
        }
    }
    out
}

pub fn build_attrs_from_table(
    table: &EntityTable,
    schema: &str,
    ids: &CompiledIds,
    object_guid: &[String],
    object_entity_upper: &[String],
    object_step_id: &[u64],
    entity_filter: Option<&[String]>,
) -> HashMap<String, HashMap<String, String>> {
    let facets = attribute_facets(ids);
    if facets.is_empty() {
        return HashMap::new();
    }
    let mut out: HashMap<String, HashMap<String, String>> = HashMap::new();
    for pix in 0..object_guid.len() {
        let ent = &object_entity_upper[pix];
        if let Some(wanted) = entity_filter {
            if !entity_type_matches_any(ent, wanted) {
                continue;
            }
        }
        let step_id = object_step_id[pix];
        let Some((_, args)) = table.get(step_id) else {
            continue;
        };
        let fields = split_top_level_args(args);
        let mut row: HashMap<String, String> = HashMap::new();
        for facet in &facets {
            let names = matching_attribute_names(
                schema,
                ent,
                facet.attribute_name.as_deref(),
                &facet.attribute_names,
                facet.attribute_name_constraint.as_ref(),
            );
            for name in names {
                if row.contains_key(&name) {
                    continue;
                }
                if let Some(idx) = attribute_schema::attribute_index(schema, ent, &name) {
                    if let Some(v) = read_attribute_at_index(&fields, idx as usize) {
                        row.insert(name, v);
                    }
                }
            }
        }
        if !row.is_empty() {
            out.insert(object_guid[pix].clone(), row);
        }
    }
    out
}
