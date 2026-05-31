//! Parity: Rust IDS engine vs golden JSON from IfcTester.

use std::path::PathBuf;

use _core::ids::{CompiledIds, validate};
use _core::{
    entity_table::EntityTable,
    extractors::{classifications, materials, psets, quantities},
    indexer,
    source,
};

fn fixture_ifc() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/data/minimal.ifc")
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/ids/goldens/simple_wall_requirement.json")
}

fn run_rust(compiled: &CompiledIds, ifc_path: &str) -> _core::ids::ValidationReport {
    let mmap = source::open(std::path::Path::new(ifc_path)).expect("open ifc");
    let indexed = indexer::index(&mmap);
    let table = EntityTable::build(std::sync::Arc::new(mmap));
    let object_step_to_guid = _core::object_guid::build_extractor_object_map(&indexed, &table, true);
    let unit_scale = indexed.unit_scale.unwrap_or(1.0);
    let psets = psets::build(&table, &object_step_to_guid);
    let qty = quantities::build(&table, &object_step_to_guid);
    let mut classifications =
        classifications::build(&table, &object_step_to_guid);
    classifications::expand_for_ids(&table, &mut classifications, &indexed);
    let materials = materials::build(&table, &object_step_to_guid, unit_scale);
    validate(
        &indexed,
        &psets,
        &qty,
        &classifications,
        &materials,
        compiled,
        &table,
        ifc_path,
    )
}

#[test]
fn rust_xml_compile_simple_wall() {
    let ids_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/ids/fixtures/simple_wall_requirement.ids");
    if !ids_path.is_file() {
        return;
    }
    let xml = std::fs::read_to_string(&ids_path).expect("read ids");
    let compiled = _core::ids::xml::parse_ids_xml(
        ids_path.to_str().unwrap(),
        &xml,
    )
    .expect("parse xml");
    assert!(!compiled.specifications.is_empty());
}

#[test]
fn rust_matches_golden_when_present() {
    let ifc = fixture_ifc();
    let golden = golden_path();
    if !ifc.is_file() || !golden.is_file() {
        return;
    }
    let text = std::fs::read_to_string(&golden).expect("read golden");
    let golden_specs: serde_json::Value = serde_json::from_str(&text).expect("parse golden");
    let compiled_text = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/ids/fixtures/simple_wall_requirement.compiled.json"),
    )
    .unwrap_or_else(|_| {
        let ids_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/ids/fixtures/simple_wall_requirement.ids");
        let xml = std::fs::read_to_string(&ids_path).expect("read ids");
        serde_json::to_string(
            &_core::ids::xml::parse_ids_xml(ids_path.to_str().unwrap(), &xml).unwrap(),
        )
        .unwrap()
    });
    let compiled: CompiledIds = serde_json::from_str(&compiled_text).expect("compiled");
    let report = run_rust(&compiled, ifc.to_str().unwrap());
    let specs = golden_specs.as_array().expect("golden array");
    assert_eq!(report.specifications.len(), specs.len());
    for (got, exp) in report.specifications.iter().zip(specs.iter()) {
        assert_eq!(got.name, exp["name"].as_str().unwrap());
        assert_eq!(
            got.applicable_count,
            exp["applicable"].as_u64().unwrap() as usize
        );
        assert_eq!(got.failed_count, exp["failed"].as_u64().unwrap() as usize);
        let exp_guids: Vec<String> = exp["failed_guids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(got.failed_guids, exp_guids);
    }
}
