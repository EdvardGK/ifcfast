//! Unified step-id → GlobalId map for every `IfcObjectDefinition` the indexer
//! tracks: products, storeys, spatial structure, type objects, and spaces.
//!
//! Extractors (`psets`, `classifications`, `materials`, `quantities`) resolve
//! `IfcRelAssociates*` / `IfcRelDefinesByProperties` related-object refs through
//! this map instead of products-only.

use std::collections::HashMap;

use crate::entity_table::EntityTable;
use crate::indexer::IndexedFile;

/// Stable synthetic GlobalId for non-rooted `IfcMaterial` instances (no GUID in IFC4).
pub fn material_step_guid(step_id: u64) -> String {
    format!("STEP#{step_id}")
}

pub fn is_material_step_guid(guid: &str) -> bool {
    guid.starts_with("STEP#")
}

/// Merge all indexer GUID columns into one lookup table.
pub fn build_object_step_to_guid(indexed: &IndexedFile) -> HashMap<u64, String> {
    let cap = indexed.product_step_id.len()
        + indexed.storey_step_id.len()
        + indexed.type_object_step_id.len()
        + indexed.group_object_step_id.len()
        + indexed.site_step_id_to_guid.len()
        + indexed.building_step_id_to_guid.len()
        + indexed.project_step_id_to_guid.len()
        + indexed.space_step_id_to_guid.len();
    let mut map = HashMap::with_capacity(cap);

    for (sid, guid) in indexed
        .product_step_id
        .iter()
        .zip(indexed.product_guid.iter())
    {
        map.insert(*sid, guid.clone());
    }
    for (sid, guid) in indexed
        .storey_step_id
        .iter()
        .zip(indexed.storey_guid.iter())
    {
        map.insert(*sid, guid.clone());
    }
    for (sid, guid) in indexed
        .type_object_step_id
        .iter()
        .zip(indexed.type_object_guid.iter())
    {
        map.insert(*sid, guid.clone());
    }
    for (sid, guid) in &indexed.site_step_id_to_guid {
        map.insert(*sid, guid.clone());
    }
    for (sid, guid) in &indexed.building_step_id_to_guid {
        map.insert(*sid, guid.clone());
    }
    for (sid, guid) in &indexed.project_step_id_to_guid {
        map.insert(*sid, guid.clone());
    }
    for (sid, guid) in &indexed.space_step_id_to_guid {
        map.insert(*sid, guid.clone());
    }
    for (sid, guid) in indexed
        .group_object_step_id
        .iter()
        .zip(indexed.group_object_guid.iter())
    {
        map.insert(*sid, guid.clone());
    }

    map
}

/// Register `IfcMaterial` step ids so classifiers can target non-rooted resources.
pub fn append_material_step_guids(table: &EntityTable, map: &mut HashMap<u64, String>) {
    for (step_id, type_name, _) in table.iter() {
        if type_name.eq_ignore_ascii_case(b"IFCMATERIAL") {
            map.entry(step_id)
                .or_insert_with(|| material_step_guid(step_id));
        }
    }
}

/// Object map for extractors: indexed products/spatial + optional material step ids.
pub fn build_extractor_object_map(
    indexed: &IndexedFile,
    table: &EntityTable,
    scan_materials: bool,
) -> HashMap<u64, String> {
    let mut map = build_object_step_to_guid(indexed);
    if scan_materials {
        append_material_step_guids(table, &mut map);
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::open;
    use std::path::PathBuf;

    fn minimal_ifc() -> Option<Vec<u8>> {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/data/minimal.ifc");
        if !p.is_file() {
            return None;
        }
        let src = open(&p).ok()?;
        Some(src.as_ref().to_vec())
    }

    #[test]
    fn merges_product_storey_and_spatial_maps() {
        let Some(buf) = minimal_ifc() else {
            return;
        };
        let indexed = crate::indexer::index(&buf);
        let map = build_object_step_to_guid(&indexed);

        assert!(!indexed.product_step_id.is_empty());
        for (sid, guid) in indexed
            .product_step_id
            .iter()
            .zip(indexed.product_guid.iter())
        {
            assert_eq!(map.get(sid), Some(guid));
        }
        for (sid, guid) in &indexed.project_step_id_to_guid {
            assert_eq!(map.get(sid), Some(guid));
        }
    }

    #[test]
    fn type_object_step_ids_resolve() {
        let buf = br#"ISO-10303-21;
HEADER;FILE_SCHEMA(('IFC4'));ENDSEC;
DATA;
#1=IFCPROJECT('0Prj000000000000000001',$,'p',$,$,$,$,$,#2);
#2=IFCUNITASSIGNMENT(($));
#10=IFCWALLTYPE('1Typ000000000000000001',$,'WT',$,$,$,$,$,$,.STANDARD.);
ENDSEC;
END-ISO-10303-21;
"#;
        let indexed = crate::indexer::index(buf);
        let map = build_object_step_to_guid(&indexed);
        assert_eq!(indexed.type_object_step_id.len(), 1);
        assert_eq!(
            map.get(&indexed.type_object_step_id[0]),
            Some(&indexed.type_object_guid[0])
        );
    }

    #[test]
    fn material_step_ids_use_step_prefix() {
        let buf = br#"ISO-10303-21;
HEADER;FILE_SCHEMA(('IFC4'));ENDSEC;
DATA;
#1=IFCPROJECT('0Prj000000000000000001',$,'p',$,$,$,$,$,#2);
#2=IFCUNITASSIGNMENT(($));
#16=IFCMATERIAL('Material',$,$);
ENDSEC;
END-ISO-10303-21;
"#;
        let indexed = crate::indexer::index(buf);
        let table = crate::entity_table::EntityTable::build_from_slice(buf);
        let map = build_extractor_object_map(&indexed, &table, true);
        assert_eq!(map.get(&16), Some(&material_step_guid(16)));
    }
}
