//! End-to-end clash integration tests.
//!
//! Feeds a synthetic IFC4 fixture through `Bundle::build` →
//! `mesh_ifc_streaming` → `ParquetSink` → `clash::clash`, then asserts
//! the resulting `ClashReport` matches the geometric truth of the
//! fixture (two overlapping walls clash hard; with tolerance, a near-
//! miss pair becomes a clearance row).

#![cfg(all(feature = "bundle", feature = "clash"))]

use std::fs;
use std::path::PathBuf;

use _core::bundle::parquet_sink::ParquetSink;
use _core::bundle::Bundle;
use _core::clash::{
    clash as run_clash, write_clashes_parquet, ClashCategory, ClashKind, ClashOptions,
};
use _core::mesh::mesh_ifc_streaming;

/// Two walls offset along X by 500 mm. Each is a 1000 mm × 200 mm
/// rectangular cross-section extruded 3000 mm along +Z. Wall A is at
/// the placement origin; Wall B's placement origin is at (500, 0, 0)
/// mm. With cross-sections centered on their placement, the meshes
/// overlap in `x ∈ [0, 0.5]`, `y ∈ [-0.1, 0.1]`, `z ∈ [0, 3]` (metres).
const TWO_OVERLAPPING_WALLS: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('clash.ifc','2026-05-30T00:00:00',('test'),('skiplum'),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCPROJECT('0Test000000000000000001',$,'p',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#3,#4));
#3=IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.);
#4=IFCSIUNIT(*,.PLANEANGLEUNIT.,$,.RADIAN.);
#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);
#6=IFCAXIS2PLACEMENT3D(#7,$,$);
#7=IFCCARTESIANPOINT((0.,0.,0.));
#10=IFCSITE('1Site000000000000000001',$,'s',$,$,#15,$,$,.ELEMENT.,$,$,$,$,$);
#11=IFCBUILDING('2Bldg000000000000000001',$,'b',$,$,#15,$,$,.ELEMENT.,$,$,$);
#12=IFCBUILDINGSTOREY('3Stor000000000000000001',$,'Level 1',$,$,#15,$,$,.ELEMENT.,0.0);
#15=IFCLOCALPLACEMENT($,#6);
#16=IFCLOCALPLACEMENT(#15,#6);
#20=IFCRELAGGREGATES('4Agg000000000000000001',$,$,$,#1,(#10));
#21=IFCRELAGGREGATES('5Agg000000000000000001',$,$,$,#10,(#11));
#22=IFCRELAGGREGATES('6Agg000000000000000001',$,$,$,#11,(#12));
#30=IFCRECTANGLEPROFILEDEF(.AREA.,'WallRect',#31,1000.,200.);
#31=IFCAXIS2PLACEMENT2D(#7,$);
#32=IFCDIRECTION((0.,0.,1.));
#33=IFCEXTRUDEDAREASOLID(#30,#6,#32,3000.);
#40=IFCSHAPEREPRESENTATION(#5,'Body','SweptSolid',(#33));
#41=IFCPRODUCTDEFINITIONSHAPE($,$,(#40));
#50=IFCWALL('7WallA0000000000000001',$,'WallA',$,$,#16,#41,'A',.STANDARD.);
#60=IFCCARTESIANPOINT((500.,0.,0.));
#61=IFCAXIS2PLACEMENT3D(#60,$,$);
#62=IFCLOCALPLACEMENT(#15,#61);
#70=IFCWALL('8WallB0000000000000001',$,'WallB',$,$,#62,#41,'B',.STANDARD.);
#80=IFCRELCONTAINEDINSPATIALSTRUCTURE('9RelC00000000000000001',$,$,$,(#50,#70),#12);
ENDSEC;
END-ISO-10303-21;
"#;

/// Two walls 1000 mm apart along X (no overlap, 0.5 m gap between
/// surfaces). At tolerance 0 they shouldn't clash; at tolerance 0.6 m
/// the broad+narrow phase emits a clearance pair.
const TWO_NEAR_MISS_WALLS: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('clash.ifc','2026-05-30T00:00:00',('test'),('skiplum'),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCPROJECT('0Test000000000000000001',$,'p',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#3,#4));
#3=IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.);
#4=IFCSIUNIT(*,.PLANEANGLEUNIT.,$,.RADIAN.);
#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);
#6=IFCAXIS2PLACEMENT3D(#7,$,$);
#7=IFCCARTESIANPOINT((0.,0.,0.));
#10=IFCSITE('1Site000000000000000001',$,'s',$,$,#15,$,$,.ELEMENT.,$,$,$,$,$);
#11=IFCBUILDING('2Bldg000000000000000001',$,'b',$,$,#15,$,$,.ELEMENT.,$,$,$);
#12=IFCBUILDINGSTOREY('3Stor000000000000000001',$,'Level 1',$,$,#15,$,$,.ELEMENT.,0.0);
#15=IFCLOCALPLACEMENT($,#6);
#16=IFCLOCALPLACEMENT(#15,#6);
#20=IFCRELAGGREGATES('4Agg000000000000000001',$,$,$,#1,(#10));
#21=IFCRELAGGREGATES('5Agg000000000000000001',$,$,$,#10,(#11));
#22=IFCRELAGGREGATES('6Agg000000000000000001',$,$,$,#11,(#12));
#30=IFCRECTANGLEPROFILEDEF(.AREA.,'WallRect',#31,1000.,200.);
#31=IFCAXIS2PLACEMENT2D(#7,$);
#32=IFCDIRECTION((0.,0.,1.));
#33=IFCEXTRUDEDAREASOLID(#30,#6,#32,3000.);
#40=IFCSHAPEREPRESENTATION(#5,'Body','SweptSolid',(#33));
#41=IFCPRODUCTDEFINITIONSHAPE($,$,(#40));
#50=IFCWALL('7WallA0000000000000001',$,'WallA',$,$,#16,#41,'A',.STANDARD.);
#60=IFCCARTESIANPOINT((1500.,0.,0.));
#61=IFCAXIS2PLACEMENT3D(#60,$,$);
#62=IFCLOCALPLACEMENT(#15,#61);
#70=IFCWALL('8WallB0000000000000001',$,'WallB',$,$,#62,#41,'B',.STANDARD.);
#80=IFCRELCONTAINEDINSPATIALSTRUCTURE('9RelC00000000000000001',$,$,$,(#50,#70),#12);
ENDSEC;
END-ISO-10303-21;
"#;

fn bundle_to_parquet(buf: &[u8]) -> PathBuf {
    let out_dir = std::env::temp_dir().join(format!(
        "ifcfast-clash-it-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    fs::create_dir_all(&out_dir).unwrap();
    let bundle = Bundle::build(buf);
    let mut sink = ParquetSink::create_in_dir(&out_dir, &bundle).unwrap();
    let _ = mesh_ifc_streaming(buf, &mut sink);
    sink.finish().unwrap();
    out_dir
}

#[test]
fn two_overlapping_walls_produce_one_hard_clash() {
    let out_dir = bundle_to_parquet(TWO_OVERLAPPING_WALLS.as_bytes());
    let report = run_clash(&out_dir, &ClashOptions::default()).unwrap();

    assert_eq!(
        report.pairs.len(),
        1,
        "expected exactly one clash pair, got {}",
        report.pairs.len()
    );
    let p = &report.pairs[0];
    assert_eq!(p.kind, ClashKind::Hard);
    assert_eq!(p.min_distance_m, 0.0);
    assert_eq!(p.class_a, "Wall");
    assert_eq!(p.class_b, "Wall");
    // Wall-vs-Wall is neither insulation nor a same-family MEP joint
    // nor non-physical → default "clash" category.
    assert_eq!(p.category, ClashCategory::Clash);
    let guids = [p.guid_a.as_str(), p.guid_b.as_str()];
    assert!(guids.contains(&"7WallA0000000000000001"));
    assert!(guids.contains(&"8WallB0000000000000001"));
}

#[test]
fn near_miss_walls_only_clash_with_tolerance() {
    let out_dir = bundle_to_parquet(TWO_NEAR_MISS_WALLS.as_bytes());

    // 0 tolerance — no pairs.
    let hard = run_clash(&out_dir, &ClashOptions::default()).unwrap();
    assert!(hard.pairs.is_empty(), "near-miss walls should not hard-clash");

    // The cross-section ends at x=500 mm (wall A: centered on origin,
    // half-width 500). Wall B's placement is at x=1500 mm, its cross-
    // section starts at x=1000 mm. Gap = 500 mm. Tolerance 0.6 m
    // closes it.
    let with_tol = run_clash(
        &out_dir,
        &ClashOptions {
            tolerance_m: 0.6,
            ..ClashOptions::default()
        },
    )
    .unwrap();
    assert_eq!(with_tol.pairs.len(), 1, "expected one clearance pair");
    assert_eq!(with_tol.pairs[0].kind, ClashKind::Clearance);
    assert!(
        with_tol.pairs[0].min_distance_m > 0.0
            && with_tol.pairs[0].min_distance_m <= 0.6,
        "expected positive clearance ≤ 0.6 m, got {}",
        with_tol.pairs[0].min_distance_m
    );
}

#[test]
fn write_clashes_parquet_roundtrips() {
    use arrow::array::{Array, Float32Array, StringArray, UInt64Array};
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

    let out_dir = bundle_to_parquet(TWO_OVERLAPPING_WALLS.as_bytes());
    let report = run_clash(&out_dir, &ClashOptions::default()).unwrap();

    let path = out_dir.join("clashes.parquet");
    write_clashes_parquet(&path, &report.pairs).unwrap();

    let file = std::fs::File::open(&path).unwrap();
    let builder = ParquetRecordBatchReaderBuilder::try_new(file).unwrap();
    let reader = builder.build().unwrap();
    let batches: Vec<_> = reader.collect::<Result<Vec<_>, _>>().unwrap();
    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total_rows, 1, "one clash row written");

    let batch = &batches[0];
    let kind = batch
        .column_by_name("kind")
        .unwrap()
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    let category = batch
        .column_by_name("category")
        .unwrap()
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    let distance = batch
        .column_by_name("min_distance_m")
        .unwrap()
        .as_any()
        .downcast_ref::<Float32Array>()
        .unwrap();
    let ifc_id_a = batch
        .column_by_name("ifc_id_a")
        .unwrap()
        .as_any()
        .downcast_ref::<UInt64Array>()
        .unwrap();
    assert_eq!(kind.value(0), "hard");
    assert_eq!(category.value(0), "clash");
    assert_eq!(distance.value(0), 0.0);
    assert!(ifc_id_a.value(0) > 0);
}

#[test]
fn empty_pair_list_still_writes_parseable_parquet() {
    let path = std::env::temp_dir().join(format!(
        "ifcfast-clash-empty-{}.parquet",
        std::process::id()
    ));
    write_clashes_parquet(&path, &[]).unwrap();

    let file = std::fs::File::open(&path).unwrap();
    let builder =
        parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(file).unwrap();
    let reader = builder.build().unwrap();
    let batches: Vec<_> = reader.collect::<Result<Vec<_>, _>>().unwrap();
    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total_rows, 0, "empty pairs write an empty table");
}
