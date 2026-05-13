//! Material assignment extraction.
//!
//! Walks `IfcRelAssociatesMaterial` and emits long-format rows. The
//! `RelatingMaterial` ref can point at any of several IFC material types;
//! we resolve each into a flat list of (material_name, layer_thickness,
//! layer_index, category) entries.
//!
//! Output schema:
//!     (guid, role, layer_index, material_name, layer_thickness_mm, category)
//!
//! `role`:
//!   `"direct"`       — `IfcMaterial` directly assigned
//!   `"list"`         — `IfcMaterialList` (one row per material)
//!   `"layer"`        — `IfcMaterialLayer` inside `IfcMaterialLayerSet` /
//!                      `IfcMaterialLayerSetUsage` (one row per layer)
//!   `"constituent"`  — `IfcMaterialConstituent` inside `IfcMaterialConstituentSet`
//!   `"profile"`      — `IfcMaterialProfile` inside `IfcMaterialProfileSet`
//!
//! Phase 1 covers IfcMaterial / IfcMaterialList / IfcMaterialLayerSet /
//! IfcMaterialLayerSetUsage. Constituent + profile sets are uncommon in
//! Norwegian/Nordic practice; folded in if/when they surface.

use std::collections::HashMap;

use crate::entity_table::EntityTable;
use crate::lexer::{parse_field, split_top_level_args, Field};

#[derive(Debug, Default)]
pub struct MaterialTable {
    pub guid: Vec<String>,
    pub role: Vec<&'static str>,
    pub layer_index: Vec<i32>, // -1 for non-layered roles
    pub material_name: Vec<Option<String>>,
    pub layer_thickness_mm: Vec<Option<f64>>,
    pub category: Vec<Option<String>>,
}

impl MaterialTable {
    pub fn len(&self) -> usize {
        self.guid.len()
    }
}

pub fn build(
    table: &EntityTable,
    product_step_to_guid: &HashMap<u64, String>,
) -> MaterialTable {
    // Pass 1: index every material-related entity by step_id so we can
    //         resolve refs cheaply during the second pass.
    let mut materials: HashMap<u64, MaterialRecord> = HashMap::with_capacity(2048);
    let mut layer_sets: HashMap<u64, Vec<u64>> = HashMap::with_capacity(512);
    let mut layer_set_usages: HashMap<u64, u64> = HashMap::with_capacity(512);
    let mut layers: HashMap<u64, LayerRecord> = HashMap::with_capacity(4096);
    let mut material_lists: HashMap<u64, Vec<u64>> = HashMap::with_capacity(256);

    // Collect rel-pairs: (related_object_step_ids, relating_material_ref)
    let mut rel_pairs: Vec<(u64, u64)> = Vec::with_capacity(8192);

    for (step_id, type_name, args) in table.iter() {
        if type_name.eq_ignore_ascii_case(b"IFCMATERIAL") {
            // IFC2X3: (Name)
            // IFC4:   (Name, Description, Category)
            let fields = split_top_level_args(args);
            let name = string_at(&fields, 0);
            let category = string_at(&fields, 2);
            materials.insert(step_id, MaterialRecord { name, category });
        } else if type_name.eq_ignore_ascii_case(b"IFCMATERIALLAYER") {
            // IFC2X3: (Material, LayerThickness, IsVentilated)
            // IFC4:   (Material, LayerThickness, IsVentilated, Name, Description, Category, Priority)
            let fields = split_top_level_args(args);
            let material_ref = match fields.first().copied().map(parse_field) {
                Some(Field::Ref(id)) => Some(id),
                _ => None,
            };
            let thickness = number_at(&fields, 1);
            // IFC4 layer-name overrides Material.Name when present.
            let name_override = string_at(&fields, 3);
            let category_override = string_at(&fields, 5);
            layers.insert(
                step_id,
                LayerRecord {
                    material_ref,
                    thickness_mm: thickness,
                    name_override,
                    category_override,
                },
            );
        } else if type_name.eq_ignore_ascii_case(b"IFCMATERIALLAYERSET") {
            // (MaterialLayers, LayerSetName, ...)
            let fields = split_top_level_args(args);
            layer_sets.insert(step_id, ref_list_at(&fields, 0));
        } else if type_name.eq_ignore_ascii_case(b"IFCMATERIALLAYERSETUSAGE") {
            // (ForLayerSet, LayerSetDirection, DirectionSense, OffsetFromReferenceLine)
            let fields = split_top_level_args(args);
            if let Some(Field::Ref(id)) = fields.first().copied().map(parse_field) {
                layer_set_usages.insert(step_id, id);
            }
        } else if type_name.eq_ignore_ascii_case(b"IFCMATERIALLIST") {
            // (Materials: LIST OF IfcMaterial)
            let fields = split_top_level_args(args);
            material_lists.insert(step_id, ref_list_at(&fields, 0));
        } else if type_name.eq_ignore_ascii_case(b"IFCRELASSOCIATESMATERIAL") {
            // (GlobalId, OwnerHistory, Name, Description, RelatedObjects, RelatingMaterial)
            let fields = split_top_level_args(args);
            let relating = match fields.get(5).copied().map(parse_field) {
                Some(Field::Ref(id)) => id,
                _ => continue,
            };
            let relateds = match fields.get(4).copied().map(parse_field) {
                Some(Field::List(body)) => parse_ref_list(body),
                Some(Field::Ref(id)) => vec![id],
                _ => continue,
            };
            for obj_id in relateds {
                rel_pairs.push((obj_id, relating));
            }
        }
    }

    let mut out = MaterialTable::default();

    for (obj_step_id, relating_id) in rel_pairs {
        let guid = match product_step_to_guid.get(&obj_step_id) {
            Some(g) => g,
            None => continue,
        };

        // Resolve `relating_id` against each known material container type.
        if let Some(mat) = materials.get(&relating_id) {
            push_row(&mut out, guid, "direct", -1, mat.name.clone(), None, mat.category.clone());
            continue;
        }
        if let Some(list) = material_lists.get(&relating_id) {
            for (i, mid) in list.iter().enumerate() {
                if let Some(mat) = materials.get(mid) {
                    push_row(
                        &mut out, guid, "list", i as i32,
                        mat.name.clone(), None, mat.category.clone(),
                    );
                }
            }
            continue;
        }
        // IfcMaterialLayerSetUsage → IfcMaterialLayerSet.
        let lset_id = layer_set_usages
            .get(&relating_id)
            .copied()
            .or(Some(relating_id))
            .unwrap();
        if let Some(layer_ids) = layer_sets.get(&lset_id) {
            for (i, lid) in layer_ids.iter().enumerate() {
                if let Some(layer) = layers.get(lid) {
                    let mat = layer.material_ref.and_then(|mid| materials.get(&mid));
                    let name = layer
                        .name_override
                        .clone()
                        .or_else(|| mat.and_then(|m| m.name.clone()));
                    let category = layer
                        .category_override
                        .clone()
                        .or_else(|| mat.and_then(|m| m.category.clone()));
                    push_row(
                        &mut out, guid, "layer", i as i32,
                        name, layer.thickness_mm, category,
                    );
                }
            }
            continue;
        }
        // Unknown relating type (constituent set, profile set, etc.) — record
        // the GUID with a placeholder so the row count reflects reality.
        push_row(&mut out, guid, "unknown", -1, None, None, None);
    }

    out
}

fn push_row(
    out: &mut MaterialTable,
    guid: &str,
    role: &'static str,
    layer_index: i32,
    name: Option<String>,
    thickness: Option<f64>,
    category: Option<String>,
) {
    out.guid.push(guid.to_string());
    out.role.push(role);
    out.layer_index.push(layer_index);
    out.material_name.push(name);
    out.layer_thickness_mm.push(thickness);
    out.category.push(category);
}

struct MaterialRecord {
    name: Option<String>,
    category: Option<String>,
}

struct LayerRecord {
    material_ref: Option<u64>,
    thickness_mm: Option<f64>,
    name_override: Option<String>,
    category_override: Option<String>,
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
