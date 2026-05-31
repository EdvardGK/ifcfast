//! Bundle integration tests — end-to-end substrate writes.
//!
//! Each test feeds a synthetic IFC4 buffer through `Bundle::build` +
//! `mesh_ifc_streaming` + `ParquetSink`, then reads the resulting
//! `instances.parquet` / `representations.parquet` back via the
//! `parquet` crate and asserts on the table shape and content. This
//! locks in the substrate schema and the cross-module contract
//! between bundle / mesh / parquet_sink, which had zero coverage
//! prior to this file.

#![cfg(feature = "bundle")]

use std::fs;

use _core::bundle::parquet_sink::ParquetSink;
use _core::bundle::Bundle;
use _core::mesh::mesh_ifc_streaming;

use arrow::array::{
    Array, AsArray, FixedSizeListArray, Float32Array, StringArray, UInt32Array, UInt64Array,
};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

/// One IfcWall with a single boolean pset + one IfcSpace aggregated
/// under a storey. Exercises:
///   - geometric product → mesh + rep + instance row
///   - geometryless product (the space has no Representation arg, but
///     still has psets via IfcRelAggregates → storey)
///   - storey resolution via the aggregate chain
///   - psets carried into the instance row
const MIXED_FIXTURE: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('bundle.ifc','2026-05-26T00:00:00',('test'),('skiplum'),'ifcfast','ifcfast','');
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
#50=IFCWALL('7Wall00000000000000001',$,'TestWall',$,$,#16,#41,'tag',.STANDARD.);
#51=IFCRELCONTAINEDINSPATIALSTRUCTURE('8RelC00000000000000001',$,$,$,(#50),#12);
#60=IFCSPACE('9Spc0000000000000000001',$,'Office 101',$,$,#16,$,'Office',.ELEMENT.,.SPACE.,$);
#61=IFCRELAGGREGATES('ASpcAgg000000000000001',$,$,$,#12,(#60));
#70=IFCPROPERTYSINGLEVALUE('IsExternal',$,IFCBOOLEAN(.T.),$);
#71=IFCPROPERTYSET('BPset000000000000000001',$,'Pset_WallCommon',$,(#70));
#72=IFCRELDEFINESBYPROPERTIES('CRel000000000000000001',$,$,$,(#50),#71);
ENDSEC;
END-ISO-10303-21;
"#;

fn bundle_to_parquet(buf: &[u8]) -> (Bundle, std::path::PathBuf) {
    let out_dir = std::env::temp_dir().join(format!(
        "ifcfast-bundle-it-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    fs::create_dir_all(&out_dir).unwrap();

    let bundle = Bundle::build(buf);
    let mut sink = ParquetSink::create_in_dir(&out_dir, &bundle)
        .expect("ParquetSink::create_in_dir");
    let _stats = mesh_ifc_streaming(buf, &mut sink);
    sink.finish().expect("sink finish");

    (bundle, out_dir)
}

fn read_parquet(path: &std::path::Path) -> Vec<arrow::record_batch::RecordBatch> {
    let file = std::fs::File::open(path).unwrap();
    let builder = ParquetRecordBatchReaderBuilder::try_new(file).unwrap();
    let reader = builder.build().unwrap();
    reader.collect::<Result<Vec<_>, _>>().unwrap()
}

#[test]
fn end_to_end_writes_wall_and_space_with_aggregate_storey() {
    let (bundle, out_dir) = bundle_to_parquet(MIXED_FIXTURE.as_bytes());
    assert!(bundle.product_count() >= 2);

    // Read the instances table back.
    let batches = read_parquet(&out_dir.join("instances.parquet"));
    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert!(total_rows >= 2, "expected ≥2 instance rows, got {total_rows}");

    // Collect everything into per-column maps for easier asserts.
    let mut guids: Vec<String> = Vec::new();
    let mut classes: Vec<String> = Vec::new();
    let mut storey_names: Vec<Option<String>> = Vec::new();
    let mut rep_ids: Vec<Option<u64>> = Vec::new();
    for batch in &batches {
        let g = batch
            .column_by_name("guid")
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        let c = batch
            .column_by_name("class")
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        let s = batch
            .column_by_name("storey_name")
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        let r = batch
            .column_by_name("rep_id")
            .unwrap()
            .as_any()
            .downcast_ref::<UInt64Array>()
            .unwrap();
        for i in 0..batch.num_rows() {
            guids.push(g.value(i).to_string());
            classes.push(c.value(i).to_string());
            storey_names
                .push(if s.is_null(i) { None } else { Some(s.value(i).to_string()) });
            rep_ids
                .push(if r.is_null(i) { None } else { Some(r.value(i)) });
        }
    }

    // Both the wall and the space must appear with their authored GUIDs.
    let wall_idx = guids
        .iter()
        .position(|g| g == "7Wall00000000000000001")
        .expect("wall instance row");
    let space_idx = guids
        .iter()
        .position(|g| g == "9Spc0000000000000000001")
        .expect("space instance row");

    // Class normalisation: "IfcWall" → "Wall", "IfcSpace" → "Space".
    assert_eq!(classes[wall_idx], "Wall");
    assert_eq!(classes[space_idx], "Space");

    // The wall has body geometry → rep_id populated.
    assert!(rep_ids[wall_idx].is_some(), "wall must carry a rep_id");

    // The space has no Representation but is in the substrate via the
    // geometryless-opt-in path → rep_id null.
    assert!(
        rep_ids[space_idx].is_none(),
        "space's instance row should have rep_id = NULL"
    );

    // Both products were aggregated/contained under Level 1, so the
    // storey_name column should reflect that via either the contained_in
    // path (wall) or the aggregate chain (space, which is aggregated
    // directly under the storey).
    assert_eq!(storey_names[wall_idx].as_deref(), Some("Level 1"));
    assert_eq!(storey_names[space_idx].as_deref(), Some("Level 1"));
}

#[test]
fn psets_propagate_into_instance_payload() {
    let (_bundle, out_dir) = bundle_to_parquet(MIXED_FIXTURE.as_bytes());
    let batches = read_parquet(&out_dir.join("instances.parquet"));

    // The wall has Pset_WallCommon.IsExternal = True. Confirm it lands
    // in the per-instance psets list-struct.
    let mut found = false;
    for batch in &batches {
        let guid_col = batch
            .column_by_name("guid")
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        let psets_col = batch.column_by_name("psets").unwrap().as_list::<i32>();

        for i in 0..batch.num_rows() {
            if guid_col.value(i) != "7Wall00000000000000001" {
                continue;
            }
            let pset_struct = psets_col.value(i);
            let pset_struct = pset_struct.as_struct();
            let set_names = pset_struct.column(0).as_string::<i32>();
            let prop_names = pset_struct.column(1).as_string::<i32>();
            let values = pset_struct.column(2).as_string_opt::<i32>().unwrap();
            for j in 0..pset_struct.len() {
                if set_names.value(j) == "Pset_WallCommon"
                    && prop_names.value(j) == "IsExternal"
                {
                    assert!(!values.is_null(j));
                    assert_eq!(values.value(j), "True");
                    found = true;
                }
            }
        }
    }
    assert!(found, "Pset_WallCommon.IsExternal=True missing from wall instance");
}

#[test]
fn mesh_quality_column_classifies_unit_cube_as_closed() {
    let (_bundle, out_dir) = bundle_to_parquet(MIXED_FIXTURE.as_bytes());
    let batches = read_parquet(&out_dir.join("instances.parquet"));

    // The wall is a closed extruded solid; mesh_quality must be "closed".
    // The space has no geometry → "degenerate".
    let mut wall_mq: Option<String> = None;
    let mut space_mq: Option<String> = None;
    for batch in &batches {
        let guid = batch
            .column_by_name("guid")
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        let mq = batch
            .column_by_name("mesh_quality")
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        for i in 0..batch.num_rows() {
            if guid.value(i) == "7Wall00000000000000001" {
                wall_mq = Some(mq.value(i).to_string());
            } else if guid.value(i) == "9Spc0000000000000000001" {
                space_mq = Some(mq.value(i).to_string());
            }
        }
    }
    assert_eq!(wall_mq.as_deref(), Some("closed"));
    assert_eq!(space_mq.as_deref(), Some("degenerate"));
}

/// Locks in the v0.4.19 fingerprint columns (`centroid_xyz`,
/// `vertex_count`, `triangle_count`) on the instance table.
///
/// Asserts:
///   - the wall (extruded solid) carries a centroid lying inside its
///     AABB and positive vertex / triangle counts;
///   - the geometryless space falls back to `placement_xyz` for its
///     centroid (instead of collapsing to world origin) and reports
///     zero vertices / triangles.
#[test]
fn fingerprint_columns_carry_centroid_and_counts() {
    let (_bundle, out_dir) = bundle_to_parquet(MIXED_FIXTURE.as_bytes());
    let batches = read_parquet(&out_dir.join("instances.parquet"));

    struct FpRow {
        bmin: [f32; 3],
        bmax: [f32; 3],
        centroid: [f32; 3],
        placement: [f32; 3],
        verts: u32,
        tris: u32,
    }
    let mut wall: Option<FpRow> = None;
    let mut space: Option<FpRow> = None;

    for batch in &batches {
        let guid = batch
            .column_by_name("guid")
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        let bmin = batch
            .column_by_name("bbox_min_xyz")
            .unwrap()
            .as_any()
            .downcast_ref::<FixedSizeListArray>()
            .unwrap();
        let bmax = batch
            .column_by_name("bbox_max_xyz")
            .unwrap()
            .as_any()
            .downcast_ref::<FixedSizeListArray>()
            .unwrap();
        let centroid = batch
            .column_by_name("centroid_xyz")
            .expect("centroid_xyz column must exist")
            .as_any()
            .downcast_ref::<FixedSizeListArray>()
            .unwrap();
        let placement = batch
            .column_by_name("placement_xyz")
            .unwrap()
            .as_any()
            .downcast_ref::<FixedSizeListArray>()
            .unwrap();
        let v_count = batch
            .column_by_name("vertex_count")
            .expect("vertex_count column must exist")
            .as_any()
            .downcast_ref::<UInt32Array>()
            .unwrap();
        let t_count = batch
            .column_by_name("triangle_count")
            .expect("triangle_count column must exist")
            .as_any()
            .downcast_ref::<UInt32Array>()
            .unwrap();

        let read_xyz = |arr: &FixedSizeListArray, i: usize| -> [f32; 3] {
            let v = arr.value(i);
            let f = v.as_any().downcast_ref::<Float32Array>().unwrap();
            [f.value(0), f.value(1), f.value(2)]
        };

        for i in 0..batch.num_rows() {
            let row = FpRow {
                bmin: read_xyz(bmin, i),
                bmax: read_xyz(bmax, i),
                centroid: read_xyz(centroid, i),
                placement: read_xyz(placement, i),
                verts: v_count.value(i),
                tris: t_count.value(i),
            };
            match guid.value(i) {
                "7Wall00000000000000001" => wall = Some(row),
                "9Spc0000000000000000001" => space = Some(row),
                _ => {}
            }
        }
    }

    let w = wall.expect("wall row");
    let (wmin, wmax, wcen, wv, wt) = (w.bmin, w.bmax, w.centroid, w.verts, w.tris);
    // Centroid inside the AABB on every axis (the wall is a proper solid).
    for k in 0..3 {
        assert!(
            wcen[k] >= wmin[k] && wcen[k] <= wmax[k],
            "wall centroid axis {k} ({}) outside bbox [{}, {}]",
            wcen[k],
            wmin[k],
            wmax[k]
        );
    }
    // Centroid equals AABB midpoint.
    for k in 0..3 {
        let mid = (wmin[k] + wmax[k]) * 0.5;
        assert!(
            (wcen[k] - mid).abs() < 1e-3,
            "wall centroid axis {k} not at AABB midpoint: {} vs {}",
            wcen[k],
            mid
        );
    }
    assert!(wv > 0, "wall must have vertices, got {wv}");
    assert!(wt > 0, "wall must have triangles, got {wt}");

    let s = space.expect("space row");
    let (smin, smax, scen, sp, sv, st) =
        (s.bmin, s.bmax, s.centroid, s.placement, s.verts, s.tris);
    // Geometryless: bbox collapsed to origin.
    assert_eq!(smin, [0.0, 0.0, 0.0]);
    assert_eq!(smax, [0.0, 0.0, 0.0]);
    // Centroid fallback: should equal placement_xyz, NOT world origin
    // (unless placement IS origin — which it is in this fixture, so
    // both conditions hold simultaneously). The contract is "equals
    // placement_xyz"; assert exactly that.
    assert_eq!(
        scen, sp,
        "geometryless space centroid must fall back to placement_xyz"
    );
    assert_eq!(sv, 0, "space must report zero vertices");
    assert_eq!(st, 0, "space must report zero triangles");
}
