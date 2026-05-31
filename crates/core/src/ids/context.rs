//! Validation context: products + spatial containers + property index.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::extractors::classifications::ClassificationTable;
use crate::extractors::materials::MaterialTable;
use crate::extractors::psets::PsetTable;
use crate::extractors::quantities::QuantityTable;
use crate::entity_table::EntityTable;
use crate::indexer::IndexedFile;

use super::compiled::CompiledIds;
use super::ifc_validation_base::IfcValidationBase;
use super::extract_needs::ExtractNeeds;
use super::native_expand::{
    build_attrs_from_table, expand_ids_entities_from_table, overlay_can_scope,
    scoped_entity_type_filter,
};

pub struct ValidationContext<'a> {
    pub indexed: &'a IndexedFile,
    pub psets: &'a PsetTable,
    pub ids: &'a CompiledIds,
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
    /// Attribute values keyed by object guid (from STEP + EXPRESS schema).
    pub attrs_by_guid: HashMap<String, HashMap<String, String>>,
    pub schema: String,
    /// File length-unit size in metres (e.g. 0.001 for millimetre IFC files).
    pub length_unit_scale: f64,
}

#[derive(Clone)]
pub struct ClsEntry {
    pub system_name: Option<String>,
    pub identification: Option<String>,
    pub name: Option<String>,
}

#[derive(Clone)]
pub struct MatEntry {
    pub material_name: Option<String>,
    pub category: Option<String>,
    pub layer_set_name: Option<String>,
    pub linked_material_name: Option<String>,
    pub linked_material_category: Option<String>,
    pub role: String,
}

#[derive(Clone)]
pub struct PropEntry {
    pub value: Option<String>,
    pub value_type: Option<String>,
    pub value_defined: Option<String>,
    pub value_type_defined: Option<String>,
}

impl<'a> ValidationContext<'a> {
    /// Full build (cold path): IFC base + IDS overlay.
    pub fn build(
        indexed: &'a IndexedFile,
        psets: &'a PsetTable,
        quantities: &QuantityTable,
        classifications: &ClassificationTable,
        materials: &MaterialTable,
        ids: &'a CompiledIds,
        table: &EntityTable,
        length_unit_scale: f64,
    ) -> Self {
        let base = IfcValidationBase::build(
            indexed,
            psets,
            quantities,
            classifications,
            materials,
            length_unit_scale,
        );
        Self::from_base(indexed, psets, ids, table, &base)
    }

    /// Fast path: reuse prepared IFC indexes; apply IDS-specific overlay only.
    pub fn from_base(
        indexed: &'a IndexedFile,
        psets: &'a PsetTable,
        ids: &'a CompiledIds,
        table: &EntityTable,
        base: &IfcValidationBase,
    ) -> Self {
        let mut guid_to_ix = base.guid_to_ix.clone();
        let mut object_guid = base.object_guid.clone();
        let mut object_entity_upper = base.object_entity_upper.clone();
        let mut object_name = base.object_name.clone();
        let mut object_description = base.object_description.clone();
        let mut object_tag = base.object_tag.clone();
        let mut object_predefined_type = base.object_predefined_type.clone();
        let mut object_object_type = base.object_object_type.clone();
        let mut object_step_id = base.object_step_id.clone();
        let prop_lookup = Arc::clone(&base.prop_lookup);
        let cls_by_pix = Arc::clone(&base.cls_by_pix);
        let mat_by_pix = Arc::clone(&base.mat_by_pix);
        let schema = base.schema.clone();
        let length_unit_scale = base.length_unit_scale;

        expand_ids_entities_from_table(
            table,
            indexed,
            &schema,
            ids,
            &mut guid_to_ix,
            &mut object_guid,
            &mut object_entity_upper,
            &mut object_name,
            &mut object_description,
            &mut object_tag,
            &mut object_predefined_type,
            &mut object_object_type,
            &mut object_step_id,
        );

        let needs = ExtractNeeds::from_compiled(ids);
        let entity_filter = overlay_can_scope(&needs, ids).then(|| scoped_entity_type_filter(ids));

        let attrs_by_guid = build_attrs_from_table(
            table,
            &schema,
            ids,
            &object_guid,
            &object_entity_upper,
            &object_step_id,
            entity_filter.as_deref(),
        );

        hydrate_object_types(
            table,
            &schema,
            &mut object_predefined_type,
            &mut object_object_type,
            &object_entity_upper,
            &object_step_id,
        );

        Self {
            indexed,
            psets,
            ids,
            guid_to_ix,
            object_guid,
            object_entity_upper,
            object_name,
            object_description,
            object_tag,
            object_predefined_type,
            object_object_type,
            object_step_id,
            prop_lookup,
            cls_by_pix,
            mat_by_pix,
            attrs_by_guid,
            schema,
            length_unit_scale,
        }
    }

    pub fn object_count(&self) -> usize {
        self.object_guid.len()
    }

    pub fn attrs_for_pix(&self, pix: u32) -> Option<&HashMap<String, String>> {
        self.attrs_by_guid.get(&self.object_guid[pix as usize])
    }

    pub fn schema_matches_spec(&self, ifc_versions: &[String]) -> bool {
        if ifc_versions.is_empty() {
            return true;
        }
        let schema = self.schema.to_uppercase();
        ifc_versions.iter().any(|v| {
            let v = v.to_uppercase();
            v == schema
                || schema.starts_with(&v)
                || v.starts_with(&schema)
                || (v == "IFC4" && schema.starts_with("IFC4"))
                || (v == "IFC2X3" && schema == "IFC2X3")
        })
    }

    pub fn entity_names_set(names: &[String]) -> HashSet<&str> {
        names.iter().map(|s| s.as_str()).collect()
    }
}

pub(crate) fn push_validation_object(
    guid_to_ix: &mut HashMap<String, u32>,
    object_guid: &mut Vec<String>,
    object_entity_upper: &mut Vec<String>,
    object_name: &mut Vec<Option<String>>,
    object_description: &mut Vec<Option<String>>,
    object_tag: &mut Vec<Option<String>>,
    object_predefined_type: &mut Vec<Option<String>>,
    object_object_type: &mut Vec<Option<String>>,
    object_step_id: &mut Vec<u64>,
    guid: String,
    entity: &str,
    name: Option<String>,
    description: Option<String>,
    tag: Option<String>,
    ptype: Option<String>,
    otype: Option<String>,
    step_id: u64,
) {
    if guid_to_ix.contains_key(&guid) {
        return;
    }
    let ix = object_guid.len() as u32;
    guid_to_ix.insert(guid.clone(), ix);
    object_guid.push(guid);
    object_entity_upper.push(entity.to_uppercase());
    object_name.push(name);
    object_description.push(description);
    object_tag.push(tag);
    object_predefined_type.push(ptype);
    object_object_type.push(otype);
    object_step_id.push(step_id);
}

pub(crate) fn hydrate_object_types(
    table: &EntityTable,
    schema: &str,
    object_predefined_type: &mut [Option<String>],
    object_object_type: &mut [Option<String>],
    object_entity_upper: &[String],
    object_step_id: &[u64],
) {
    for pix in 0..object_predefined_type.len() {
        let step_id = object_step_id[pix];
        let ent = &object_entity_upper[pix];
        if object_predefined_type[pix].is_none() {
            if let Some(v) =
                super::attribute_read::read_entity_attribute(table, schema, step_id, ent, "PredefinedType")
            {
                object_predefined_type[pix] = Some(v);
            }
        }
        if object_object_type[pix].is_none() {
            object_object_type[pix] = ["ObjectType", "ElementType", "ProcessType"]
                .iter()
                .find_map(|&n| {
                    super::attribute_read::read_entity_attribute(table, schema, step_id, ent, n)
                });
        }
    }
}

pub(crate) fn quantity_value_type(quantity_type: &str) -> Option<String> {
    match quantity_type {
        "Area" => Some("IfcAreaMeasure".into()),
        "Length" => Some("IfcLengthMeasure".into()),
        "Volume" => Some("IfcVolumeMeasure".into()),
        "Count" => Some("IfcCountMeasure".into()),
        "Weight" => Some("IfcMassMeasure".into()),
        "Time" => Some("IfcTimeMeasure".into()),
        _ => None,
    }
}

/// Copy type-object pset/qto rows onto products when the product has no local row.
pub(crate) fn inherit_type_psets(
    indexed: &IndexedFile,
    guid_to_ix: &HashMap<String, u32>,
    prop_lookup: &mut HashMap<(u32, String, String), PropEntry>,
) {
    let mut product_step_to_pix: HashMap<u64, u32> = HashMap::new();
    for i in 0..indexed.product_guid.len() {
        if let Some(&pix) = guid_to_ix.get(&indexed.product_guid[i]) {
            product_step_to_pix.insert(indexed.product_step_id[i], pix);
        }
    }

    let mut type_step_to_pix: HashMap<u64, u32> = HashMap::new();
    for i in 0..indexed.type_object_step_id.len() {
        if let Some(&pix) = guid_to_ix.get(&indexed.type_object_guid[i]) {
            type_step_to_pix.insert(indexed.type_object_step_id[i], pix);
        }
    }

    let mut product_to_type_ix: HashMap<u32, u32> = HashMap::new();
    for (prod_sid, type_sid) in indexed
        .defines_by_type_product
        .iter()
        .zip(indexed.defines_by_type_type.iter())
    {
        if let (Some(&pix), Some(&tix)) = (
            product_step_to_pix.get(prod_sid),
            type_step_to_pix.get(type_sid),
        ) {
            product_to_type_ix.insert(pix, tix);
        }
    }

    let type_entries: Vec<((u32, String, String), PropEntry)> = prop_lookup
        .iter()
        .filter(|((pix, _, _), _)| type_step_to_pix.values().any(|t| t == pix))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    for (prod_pix, type_pix) in product_to_type_ix {
        for ((pix, pset, prop), entry) in &type_entries {
            if *pix != type_pix {
                continue;
            }
            let key = (prod_pix, pset.clone(), prop.clone());
            prop_lookup.entry(key).or_insert_with(|| entry.clone());
        }
    }
}

/// Type-assigned materials visible on product occurrences (IfcRelDefinesByType).
pub(crate) fn inherit_type_materials(
    indexed: &IndexedFile,
    guid_to_ix: &HashMap<String, u32>,
    mat_by_pix: &mut HashMap<u32, Vec<MatEntry>>,
) {
    let mut product_step_to_pix: HashMap<u64, u32> = HashMap::new();
    for i in 0..indexed.product_guid.len() {
        if let Some(&pix) = guid_to_ix.get(&indexed.product_guid[i]) {
            product_step_to_pix.insert(indexed.product_step_id[i], pix);
        }
    }
    let mut type_step_to_pix: HashMap<u64, u32> = HashMap::new();
    for i in 0..indexed.type_object_step_id.len() {
        if let Some(&pix) = guid_to_ix.get(&indexed.type_object_guid[i]) {
            type_step_to_pix.insert(indexed.type_object_step_id[i], pix);
        }
    }

    for (prod_sid, type_sid) in indexed
        .defines_by_type_product
        .iter()
        .zip(indexed.defines_by_type_type.iter())
    {
        let (Some(&prod_pix), Some(&type_pix)) = (
            product_step_to_pix.get(prod_sid),
            type_step_to_pix.get(type_sid),
        ) else {
            continue;
        };
        let Some(type_rows) = mat_by_pix.get(&type_pix).cloned() else {
            continue;
        };
        let entry = mat_by_pix.entry(prod_pix).or_default();
        for row in type_rows {
            let dup = entry.iter().any(|e| {
                e.material_name == row.material_name
                    && e.category == row.category
                    && e.layer_set_name == row.layer_set_name
                    && e.role == row.role
            });
            if !dup {
                entry.push(row);
            }
        }
    }
}
