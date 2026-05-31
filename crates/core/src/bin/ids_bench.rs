//! Benchmark IDS validation: index, pset extract, validate.

use std::env;
use std::fs;
use std::time::Instant;

use _core::ids::{CompiledIds, validate};
use _core::source::open;
use _core::{
    entity_table::EntityTable,
    extractors::{classifications, materials, psets, quantities},
    indexer,
};

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: ifcfast-ids-bench <ifc_path> <compiled.json|ids_path>");
        std::process::exit(1);
    }
    let ifc_path = &args[1];
    let ids_arg = &args[2];

    let compiled: CompiledIds = if ids_arg.ends_with(".json") {
        let text = fs::read_to_string(ids_arg).expect("read compiled json");
        serde_json::from_str(&text).expect("parse compiled json")
    } else {
        let xml = fs::read_to_string(ids_arg).expect("read ids xml");
        _core::ids::xml::parse_ids_xml(ids_arg, &xml).expect("parse ids xml")
    };

    let t0 = Instant::now();
    let mmap = open(std::path::Path::new(ifc_path)).expect("open ifc");
    let open_ms = t0.elapsed().as_secs_f64() * 1000.0;

    let t1 = Instant::now();
    let indexed = indexer::index(&mmap);
    let index_ms = t1.elapsed().as_secs_f64() * 1000.0;

    let t2 = Instant::now();
    let table = EntityTable::build(std::sync::Arc::new(mmap));
    let table_ms = t2.elapsed().as_secs_f64() * 1000.0;

    let object_step_to_guid = _core::object_guid::build_extractor_object_map(&indexed, &table, true);

    let t3 = Instant::now();
    let unit_scale = indexed.unit_scale.unwrap_or(1.0);
    let psets = psets::build(&table, &object_step_to_guid);
    let quantities = quantities::build(&table, &object_step_to_guid);
    let mut classifications =
        classifications::build(&table, &object_step_to_guid);
    classifications::expand_for_ids(&table, &mut classifications, &indexed);
    let materials = materials::build(&table, &object_step_to_guid, unit_scale);
    let pset_ms = t3.elapsed().as_secs_f64() * 1000.0;

    let t4 = Instant::now();
    let mut report = validate(
        &indexed,
        &psets,
        &quantities,
        &classifications,
        &materials,
        &compiled,
        &table,
        ifc_path,
    );
    let validate_ms = t4.elapsed().as_secs_f64() * 1000.0;

    report.open_ms = open_ms;
    report.index_ms = index_ms;
    report.pset_extract_ms = table_ms + pset_ms;
    report.validate_ms = validate_ms;

    println!(
        "{{\"open_ms\":{:.3},\"index_ms\":{:.3},\"pset_extract_ms\":{:.3},\"validate_ms\":{:.3},\"total_ms\":{:.3},\"specs\":{}}}",
        open_ms,
        index_ms,
        report.pset_extract_ms,
        validate_ms,
        open_ms + index_ms + report.pset_extract_ms + validate_ms,
        report.specifications.len()
    );
}
