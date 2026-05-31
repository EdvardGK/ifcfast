//! End-to-end test for `mesh::cut_openings`.
//!
//! Feeds a synthetic IFC4 wall authored as
//! `IfcBooleanClippingResult(wall_extrusion, opening_extrusion)`
//! through the mesh extractor, captures the emitted `ProductMesh`,
//! and asserts:
//!   1. Without cutting, the mesh has two `MeshSegment`s tagged
//!      `boolean_first_operand|*` and `boolean_second_operand|*`.
//!   2. After `cut_openings::apply()`, the mesh has one segment
//!      tagged `cut_openings` and the host volume is reduced by the
//!      opening volume (within tessellation tolerance).

#![cfg(all(feature = "mesh", feature = "csg"))]

use _core::geom::csg;
use _core::mesh::cut_openings::{apply, Outcome};
use _core::mesh::{mesh_ifc_streaming, ProductMesh, ProductSink};

/// Wall: 1000 × 200 mm cross-section extruded 3000 mm. Cut with an
/// opening 500 × 200 mm extruded 2000 mm, positioned at the wall's
/// near face. Boolean is a CLIPPING result (difference).
const WALL_WITH_OPENING: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('wall.ifc','2026-05-31T00:00:00',('test'),('skiplum'),'ifcfast','ifcfast','');
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
#40=IFCRECTANGLEPROFILEDEF(.AREA.,'OpeningRect',#41,500.,200.);
#41=IFCAXIS2PLACEMENT2D(#7,$);
#42=IFCAXIS2PLACEMENT3D(#43,$,$);
#43=IFCCARTESIANPOINT((0.,0.,500.));
#44=IFCEXTRUDEDAREASOLID(#40,#42,#32,2000.);
#50=IFCBOOLEANCLIPPINGRESULT(.DIFFERENCE.,#33,#44);
#60=IFCSHAPEREPRESENTATION(#5,'Body','Clipping',(#50));
#61=IFCPRODUCTDEFINITIONSHAPE($,$,(#60));
#70=IFCWALL('7Wall00000000000000001',$,'TestWall',$,$,#16,#61,'tag',.STANDARD.);
#80=IFCRELCONTAINEDINSPATIALSTRUCTURE('8RelC00000000000000001',$,$,$,(#70),#12);
ENDSEC;
END-ISO-10303-21;
"#;

struct CaptureSink {
    products: Vec<ProductMesh>,
}

impl ProductSink for CaptureSink {
    fn on_product(&mut self, mesh: ProductMesh) {
        self.products.push(mesh);
    }
}

fn capture_wall_mesh() -> ProductMesh {
    let mut sink = CaptureSink {
        products: Vec::new(),
    };
    let _ = mesh_ifc_streaming(WALL_WITH_OPENING.as_bytes(), &mut sink);
    sink.products
        .into_iter()
        .find(|m| m.entity == "IfcWall")
        .expect("wall product must reach the sink")
}

#[test]
fn reveal_all_emits_both_operand_segments() {
    let mesh = capture_wall_mesh();
    assert!(
        mesh.segments.len() >= 2,
        "expected >=2 segments (host + opening), got {}: {:?}",
        mesh.segments.len(),
        mesh.segments.iter().map(|s| s.source.as_str()).collect::<Vec<_>>()
    );
    let has_host = mesh.segments.iter().any(|s| s.source.contains("boolean_first_operand"));
    let has_cutter = mesh
        .segments
        .iter()
        .any(|s| s.source.contains("boolean_second_operand"));
    assert!(has_host, "expected a boolean_first_operand segment");
    assert!(has_cutter, "expected a boolean_second_operand segment");
}

#[test]
fn cut_openings_produces_single_segment_and_reduced_volume() {
    let mut mesh = capture_wall_mesh();

    // Capture the host-only volume (before cut, by isolating the
    // first-operand triangles) so we can assert the post-cut volume
    // is the host volume minus the opening's overlap with it. The
    // mesh.vertices are in source units (mm) — manifold operates in
    // those same units, so volumes come out in mm³. We divide by
    // 1e9 to get m³ for legibility.
    let mm3_to_m3 = 1.0e-9_f64;

    // Volume of full host extrusion: 1000mm × 200mm × 3000mm = 6e8 mm³ = 0.6 m³.
    // Volume of opening that lies inside host: opening is at z ∈ [500, 2500] mm,
    // 500mm × 200mm cross-section centred on the same XY axes as the host
    // (so its XY footprint is fully inside the host XY footprint).
    // Opening volume within host = 500 × 200 × 2000 = 2e8 mm³ = 0.2 m³.
    // Expected post-cut volume = 0.6 − 0.2 = 0.4 m³.

    let outcome = apply(&mut mesh);
    assert_eq!(outcome, Outcome::Cut, "cut must succeed on this fixture");
    assert_eq!(
        mesh.segments.len(),
        1,
        "post-cut mesh must collapse to a single 'cut_openings' segment"
    );
    assert_eq!(mesh.segments[0].source, "cut_openings");

    let m = csg::build_manifold(&mesh.vertices, &mesh.indices)
        .expect("post-cut wall is manifold");
    let volume_m3 = m.volume() * mm3_to_m3;
    let expected = 0.4_f64;
    assert!(
        (volume_m3 - expected).abs() < 0.02,
        "expected ~{expected} m³ after cut, got {volume_m3} m³"
    );
}

#[test]
fn cut_openings_is_a_no_op_on_solid_wall_without_boolean() {
    // Build a minimal fixture identical to WALL_WITH_OPENING but with
    // a simple extrusion representation instead of the boolean — the
    // cut_openings path should leave it alone.
    const PLAIN_WALL: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('plain.ifc','2026-05-31T00:00:00',('test'),('skiplum'),'ifcfast','ifcfast','');
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
#50=IFCWALL('7Wall00000000000000001',$,'PlainWall',$,$,#16,#41,'tag',.STANDARD.);
#60=IFCRELCONTAINEDINSPATIALSTRUCTURE('8RelC00000000000000001',$,$,$,(#50),#12);
ENDSEC;
END-ISO-10303-21;
"#;
    let mut sink = CaptureSink {
        products: Vec::new(),
    };
    let _ = mesh_ifc_streaming(PLAIN_WALL.as_bytes(), &mut sink);
    let mut mesh = sink
        .products
        .into_iter()
        .find(|m| m.entity == "IfcWall")
        .expect("plain wall must reach the sink");

    let before_verts = mesh.vertices.clone();
    let before_idx = mesh.indices.clone();
    let outcome = apply(&mut mesh);
    assert_eq!(outcome, Outcome::Passthrough);
    assert_eq!(mesh.vertices, before_verts, "passthrough must not mutate verts");
    assert_eq!(mesh.indices, before_idx, "passthrough must not mutate indices");
}
