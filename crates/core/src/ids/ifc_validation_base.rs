//! IFC-scoped validation indexes (no IDS document required).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::extractors::classifications::ClassificationTable;
use crate::extractors::materials::MaterialTable;
use crate::extractors::psets::PsetTable;
use crate::extractors::quantities::QuantityTable;
use crate::indexer::IndexedFile;
use crate::object_guid::is_material_step_guid;

use super::context::{
    inherit_type_materials, inherit_type_psets, push_validation_object, ClsEntry, MatEntry,
    PropEntry, quantity_value_type,
};

/// Object pool + property/classification/material indexes built once per IFC.
#[derive(Clone)]
pub struct IfcValidationBase {
    pub guid_to_ix: HashMap<String, u32>,
    pub object_guid: Vec<String>,
    pub object_entity_upper: Vec<String>,
    pub object_name: Vec<Option<String>>,
    pub object_description: Vec<Option<String>>,
    pub object_tag: Vec<Option<String>>,
    pub object_predefined_type: Vec<Option<String>>,
    pub object_object_type: Vec<Option<String>>,
    pub object_step_id: Vec<u64>,
    pub prop_lookup: Arc<HashMap<(u32, String, String), PropEntry>>,
    pub cls_by_pix: Arc<HashMap<u32, Vec<ClsEntry>>>,
    pub mat_by_pix: Arc<HashMap<u32, Vec<MatEntry>>>,
    pub schema: String,
    pub length_unit_scale: f64,
}

impl IfcValidationBase {
    pub fn build(
        indexed: &IndexedFile,
        psets: &PsetTable,
        quantities: &QuantityTable,
        classifications: &ClassificationTable,
        materials: &MaterialTable,
        length_unit_scale: f64,
    ) -> Self {
        let mut object_guid: Vec<String> = Vec::new();
        let mut object_entity_upper: Vec<String> = Vec::new();
        let mut object_name: Vec<Option<String>> = Vec::new();
        let mut object_description: Vec<Option<String>> = Vec::new();
        let mut object_tag: Vec<Option<String>> = Vec::new();
        let mut object_predefined_type: Vec<Option<String>> = Vec::new();
        let mut object_object_type: Vec<Option<String>> = Vec::new();
        let mut object_step_id: Vec<u64> = Vec::new();
        let mut guid_to_ix: HashMap<String, u32> = HashMap::new();

        for i in 0..indexed.product_guid.len() {
            push_validation_object(
                &mut guid_to_ix,
                &mut object_guid,
                &mut object_entity_upper,
                &mut object_name,
                &mut object_description,
                &mut object_tag,
                &mut object_predefined_type,
                &mut object_object_type,
                &mut object_step_id,
                indexed.product_guid[i].clone(),
                &indexed.product_entity[i],
                indexed.product_name[i].clone(),
                indexed.product_description[i].clone(),
                indexed.product_tag[i].clone(),
                indexed.product_predefined_type[i].clone(),
                indexed.product_object_type[i].clone(),
                indexed.product_step_id[i],
            );
        }

        for i in 0..indexed.storey_guid.len() {
            push_validation_object(
                &mut guid_to_ix,
                &mut object_guid,
                &mut object_entity_upper,
                &mut object_name,
                &mut object_description,
                &mut object_tag,
                &mut object_predefined_type,
                &mut object_object_type,
                &mut object_step_id,
                indexed.storey_guid[i].clone(),
                "IFCBUILDINGSTOREY",
                indexed.storey_name[i].clone(),
                None,
                None,
                None,
                None,
                indexed.storey_step_id[i],
            );
        }

        for i in 0..indexed.space_step_id.len() {
            let step_id = indexed.space_step_id[i];
            let guid = indexed
                .space_step_id_to_guid
                .get(&step_id)
                .cloned()
                .unwrap_or_default();
            let name = indexed.space_name.get(i).and_then(|n| n.clone());
            let ptype = indexed
                .space_predefined_type
                .get(i)
                .and_then(|p| p.clone());
            push_validation_object(
                &mut guid_to_ix,
                &mut object_guid,
                &mut object_entity_upper,
                &mut object_name,
                &mut object_description,
                &mut object_tag,
                &mut object_predefined_type,
                &mut object_object_type,
                &mut object_step_id,
                guid,
                "IFCSPACE",
                name,
                None,
                None,
                ptype,
                None,
                step_id,
            );
        }

        for (&step_id, guid) in &indexed.building_step_id_to_guid {
            let ptype = indexed.building_predefined_type.get(&step_id).cloned();
            push_validation_object(
                &mut guid_to_ix,
                &mut object_guid,
                &mut object_entity_upper,
                &mut object_name,
                &mut object_description,
                &mut object_tag,
                &mut object_predefined_type,
                &mut object_object_type,
                &mut object_step_id,
                guid.clone(),
                "IFCBUILDING",
                None,
                None,
                None,
                ptype,
                None,
                step_id,
            );
        }

        for (&step_id, guid) in &indexed.site_step_id_to_guid {
            push_validation_object(
                &mut guid_to_ix,
                &mut object_guid,
                &mut object_entity_upper,
                &mut object_name,
                &mut object_description,
                &mut object_tag,
                &mut object_predefined_type,
                &mut object_object_type,
                &mut object_step_id,
                guid.clone(),
                "IFCSITE",
                None,
                None,
                None,
                None,
                None,
                step_id,
            );
        }

        for (&step_id, guid) in &indexed.project_step_id_to_guid {
            push_validation_object(
                &mut guid_to_ix,
                &mut object_guid,
                &mut object_entity_upper,
                &mut object_name,
                &mut object_description,
                &mut object_tag,
                &mut object_predefined_type,
                &mut object_object_type,
                &mut object_step_id,
                guid.clone(),
                "IFCPROJECT",
                None,
                None,
                None,
                None,
                None,
                step_id,
            );
        }

        for i in 0..indexed.type_object_guid.len() {
            let entity = indexed
                .type_object_entity
                .get(i)
                .map(|s| s.as_str())
                .unwrap_or("IFCTYPEOBJECT");
            push_validation_object(
                &mut guid_to_ix,
                &mut object_guid,
                &mut object_entity_upper,
                &mut object_name,
                &mut object_description,
                &mut object_tag,
                &mut object_predefined_type,
                &mut object_object_type,
                &mut object_step_id,
                indexed.type_object_guid[i].clone(),
                entity,
                indexed.type_object_name[i].clone(),
                None,
                None,
                None,
                None,
                indexed.type_object_step_id[i],
            );
        }

        for i in 0..indexed.group_object_guid.len() {
            push_validation_object(
                &mut guid_to_ix,
                &mut object_guid,
                &mut object_entity_upper,
                &mut object_name,
                &mut object_description,
                &mut object_tag,
                &mut object_predefined_type,
                &mut object_object_type,
                &mut object_step_id,
                indexed.group_object_guid[i].clone(),
                &indexed.group_object_entity[i],
                None,
                None,
                None,
                indexed.group_object_predefined_type[i].clone(),
                None,
                indexed.group_object_step_id[i],
            );
        }

        let mut prop_lookup: HashMap<(u32, String, String), PropEntry> =
            HashMap::with_capacity(psets.len() + quantities.len());
        for i in 0..psets.len() {
            let guid = &psets.guid[i];
            let Some(&pix) = guid_to_ix.get(guid) else {
                continue;
            };
            let key = (
                pix,
                psets.pset_name[i].clone(),
                psets.prop_name[i].clone(),
            );
            prop_lookup.insert(
                key,
                PropEntry {
                    value: psets.value[i].clone(),
                    value_type: psets.value_type[i].clone(),
                    value_defined: psets.value_defined[i].clone(),
                    value_type_defined: psets.value_type_defined[i].clone(),
                },
            );
        }

        for i in 0..quantities.len() {
            let guid = &quantities.guid[i];
            let Some(&pix) = guid_to_ix.get(guid) else {
                continue;
            };
            let key = (
                pix,
                quantities.qto_name[i].clone(),
                quantities.quantity_name[i].clone(),
            );
            let value_type = quantity_value_type(&quantities.quantity_type[i]);
            prop_lookup.insert(
                key,
                PropEntry {
                    value: quantities.value[i].clone(),
                    value_type,
                    value_defined: None,
                    value_type_defined: None,
                },
            );
        }

        inherit_type_psets(indexed, &guid_to_ix, &mut prop_lookup);

        let schema = indexed.schema.clone();

        let mut seen_cls_guids = HashSet::new();
        for i in 0..classifications.len() {
            let guid = classifications.guid[i].clone();
            if guid_to_ix.contains_key(&guid) {
                continue;
            }
            if !seen_cls_guids.insert(guid.clone()) {
                continue;
            }
            let entity = if is_material_step_guid(&guid) {
                "IFCMATERIAL"
            } else {
                "IFCOBJECT"
            };
            let step_id = guid
                .strip_prefix("STEP#")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            push_validation_object(
                &mut guid_to_ix,
                &mut object_guid,
                &mut object_entity_upper,
                &mut object_name,
                &mut object_description,
                &mut object_tag,
                &mut object_predefined_type,
                &mut object_object_type,
                &mut object_step_id,
                guid,
                entity,
                None,
                None,
                None,
                None,
                None,
                step_id,
            );
        }

        let mut cls_by_pix: HashMap<u32, Vec<ClsEntry>> =
            HashMap::with_capacity(classifications.len());
        for i in 0..classifications.len() {
            let Some(&pix) = guid_to_ix.get(&classifications.guid[i]) else {
                continue;
            };
            cls_by_pix.entry(pix).or_default().push(ClsEntry {
                system_name: classifications.system_name[i].clone(),
                identification: classifications.identification[i].clone(),
                name: classifications.name[i].clone(),
            });
        }

        let mut mat_by_pix: HashMap<u32, Vec<MatEntry>> = HashMap::with_capacity(materials.len());
        for i in 0..materials.len() {
            let Some(&pix) = guid_to_ix.get(&materials.guid[i]) else {
                continue;
            };
            mat_by_pix.entry(pix).or_default().push(MatEntry {
                material_name: materials.material_name[i].clone(),
                category: materials.category[i].clone(),
                layer_set_name: materials.layer_set_name[i].clone(),
                linked_material_name: materials.linked_material_name[i].clone(),
                linked_material_category: materials.linked_material_category[i].clone(),
                role: materials.role[i].to_string(),
            });
        }

        inherit_type_materials(indexed, &guid_to_ix, &mut mat_by_pix);

        Self {
            guid_to_ix,
            object_guid,
            object_entity_upper,
            object_name,
            object_description,
            object_tag,
            object_predefined_type,
            object_object_type,
            object_step_id,
            prop_lookup: Arc::new(prop_lookup),
            cls_by_pix: Arc::new(cls_by_pix),
            mat_by_pix: Arc::new(mat_by_pix),
            schema,
            length_unit_scale,
        }
    }
}
