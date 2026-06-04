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
use _core::indexer;
use _core::mesh::cut_openings::{apply, CrossProductCut, Outcome, Routed};
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

/// Single-clip case: wall clipped by an `IfcHalfSpaceSolid` with
/// `AgreementFlag = .F.`. Under the de-facto IFC convention (what
/// ifcopenshell / Revit emit and consume), `.F.` keeps the -normal
/// side, so the upper half of the wall (z > 1500 mm) is removed and
/// the lower half remains. Expected post-cut volume:
/// 1000 × 200 × 1500 mm³ = 0.3 m³ (half of the original 0.6 m³
/// extrusion). Pre-fix this returned ~0.6 m³ because the half-space
/// cutter was a 0.01 mm-thick visual stand-in; now it goes through
/// `mesh::halfspace_clip`.
const WALL_WITH_HALFSPACE_CLIP: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('hs.ifc','2026-06-03T00:00:00',('test'),('skiplum'),'ifcfast','ifcfast','');
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
#12=IFCBUILDINGSTOREY('3Stor000000000000000001',$,'L1',$,$,#15,$,$,.ELEMENT.,0.0);
#15=IFCLOCALPLACEMENT($,#6);
#16=IFCLOCALPLACEMENT(#15,#6);
#20=IFCRELAGGREGATES('4Agg000000000000000001',$,$,$,#1,(#10));
#21=IFCRELAGGREGATES('5Agg000000000000000001',$,$,$,#10,(#11));
#22=IFCRELAGGREGATES('6Agg000000000000000001',$,$,$,#11,(#12));
#30=IFCRECTANGLEPROFILEDEF(.AREA.,'WallRect',#31,1000.,200.);
#31=IFCAXIS2PLACEMENT2D(#7,$);
#32=IFCDIRECTION((0.,0.,1.));
#33=IFCEXTRUDEDAREASOLID(#30,#6,#32,3000.);
#50=IFCCARTESIANPOINT((0.,0.,1500.));
#51=IFCAXIS2PLACEMENT3D(#50,$,$);
#52=IFCPLANE(#51);
#53=IFCHALFSPACESOLID(#52,.F.);
#60=IFCBOOLEANCLIPPINGRESULT(.DIFFERENCE.,#33,#53);
#70=IFCSHAPEREPRESENTATION(#5,'Body','Clipping',(#60));
#71=IFCPRODUCTDEFINITIONSHAPE($,$,(#70));
#80=IFCWALL('7Wall00000000000000001',$,'Clipped',$,$,#16,#71,'tag',.STANDARD.);
#90=IFCRELCONTAINEDINSPATIALSTRUCTURE('8Rel000000000000000001',$,$,$,(#80),#12);
ENDSEC;
END-ISO-10303-21;
"#;

#[test]
fn halfspace_clip_with_agreement_false_keeps_lower_half() {
    let mut sink = CaptureSink {
        products: Vec::new(),
    };
    let _ = mesh_ifc_streaming(WALL_WITH_HALFSPACE_CLIP.as_bytes(), &mut sink);
    let mut mesh = sink
        .products
        .into_iter()
        .find(|m| m.entity == "IfcWall")
        .expect("clipped wall must reach the sink");

    let outcome = apply(&mut mesh);
    assert_eq!(outcome, Outcome::Cut);
    let m = csg::build_manifold(&mesh.vertices, &mesh.indices)
        .expect("post-clip wall must be a closed manifold");
    let vol = m.volume() * 1.0e-9_f64;
    assert!(
        (vol - 0.3).abs() < 0.02,
        "expected ~0.3 m³ (lower half), got {vol} m³"
    );
    // Bbox check: surviving half must be z ∈ [0, 1500].
    let mut zmax = f32::NEG_INFINITY;
    for chunk in mesh.vertices.chunks_exact(3) {
        zmax = zmax.max(chunk[2]);
    }
    assert!(zmax <= 1510.0, "upper half should be gone, got zmax={zmax}");
}

/// Deep-BCR case mirroring the Sannergata wall topology (GH #39):
/// a wall extrusion clipped by THREE stacked `IfcBooleanClippingResult`
/// levels of `IfcHalfSpaceSolid`. Each clip is along an axis-aligned
/// plane perpendicular to a different world axis.
///
/// Geometry: 2000 × 200 × 3000 mm extrusion, profile centred on
/// origin so the box occupies X ∈ [-1000, 1000], Y ∈ [-100, 100],
/// Z ∈ [0, 3000]. Three clipping planes, all `AgreementFlag = .T.` —
/// under the de-facto IFC convention `.T.` keeps the +normal side:
///   1. Through (500, 0, 0), normal +X → keeps X ∈ [500, 1000]
///      (width 500 mm).
///   2. Through (0, 50, 0), normal +Y → keeps Y ∈ [50, 100]
///      (thickness 50 mm).
///   3. Through (0, 0, 1000), normal +Z → keeps Z ∈ [1000, 3000]
///      (height 2000 mm).
///
/// Expected post-clip volume: 500 × 50 × 2000 = 5e7 mm³ = 0.05 m³.
const WALL_WITH_DEEP_BCR: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('deep_bcr.ifc','2026-06-03T00:00:00',('test'),('skiplum'),'ifcfast','ifcfast','');
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
#12=IFCBUILDINGSTOREY('3Stor000000000000000001',$,'L1',$,$,#15,$,$,.ELEMENT.,0.0);
#15=IFCLOCALPLACEMENT($,#6);
#16=IFCLOCALPLACEMENT(#15,#6);
#20=IFCRELAGGREGATES('4Agg000000000000000001',$,$,$,#1,(#10));
#21=IFCRELAGGREGATES('5Agg000000000000000001',$,$,$,#10,(#11));
#22=IFCRELAGGREGATES('6Agg000000000000000001',$,$,$,#11,(#12));
#30=IFCRECTANGLEPROFILEDEF(.AREA.,'WallRect',#31,2000.,200.);
#31=IFCAXIS2PLACEMENT2D(#7,$);
#32=IFCDIRECTION((0.,0.,1.));
#33=IFCEXTRUDEDAREASOLID(#30,#6,#32,3000.);
#40=IFCCARTESIANPOINT((500.,0.,0.));
#41=IFCDIRECTION((1.,0.,0.));
#42=IFCDIRECTION((0.,1.,0.));
#43=IFCAXIS2PLACEMENT3D(#40,#41,#42);
#44=IFCPLANE(#43);
#45=IFCHALFSPACESOLID(#44,.T.);
#50=IFCBOOLEANCLIPPINGRESULT(.DIFFERENCE.,#33,#45);
#60=IFCCARTESIANPOINT((0.,50.,0.));
#61=IFCDIRECTION((0.,1.,0.));
#62=IFCDIRECTION((1.,0.,0.));
#63=IFCAXIS2PLACEMENT3D(#60,#61,#62);
#64=IFCPLANE(#63);
#65=IFCHALFSPACESOLID(#64,.T.);
#70=IFCBOOLEANCLIPPINGRESULT(.DIFFERENCE.,#50,#65);
#80=IFCCARTESIANPOINT((0.,0.,1000.));
#81=IFCAXIS2PLACEMENT3D(#80,$,$);
#82=IFCPLANE(#81);
#83=IFCHALFSPACESOLID(#82,.T.);
#90=IFCBOOLEANCLIPPINGRESULT(.DIFFERENCE.,#70,#83);
#100=IFCSHAPEREPRESENTATION(#5,'Body','Clipping',(#90));
#101=IFCPRODUCTDEFINITIONSHAPE($,$,(#100));
#110=IFCWALL('7Wall00000000000000001',$,'DeepClipped',$,$,#16,#101,'tag',.STANDARD.);
#120=IFCRELCONTAINEDINSPATIALSTRUCTURE('8Rel000000000000000001',$,$,$,(#110),#12);
ENDSEC;
END-ISO-10303-21;
"#;

#[test]
fn deep_bcr_with_three_halfspaces_cuts_correctly() {
    // Three sequential clips, all AgreementFlag = .T. — same topology
    // as Sannergata wall #299591 in GH #39, at miniature scale. The
    // failure mode the report describes (host fully consumed by
    // stacked half-space cutters going through manifold-csg) must
    // NOT reproduce here.
    let mut sink = CaptureSink {
        products: Vec::new(),
    };
    let _ = mesh_ifc_streaming(WALL_WITH_DEEP_BCR.as_bytes(), &mut sink);
    let entities: Vec<String> = sink.products.iter().map(|m| m.entity.clone()).collect();
    let mut mesh = sink
        .products
        .into_iter()
        .find(|m| m.entity == "IfcWall")
        .unwrap_or_else(|| panic!("deep-clipped wall must reach the sink; got entities: {:?}", entities));

    let outcome = apply(&mut mesh);
    assert_eq!(
        outcome,
        Outcome::Cut,
        "three sequential half-space clips must succeed via halfspace_clip"
    );
    let m = csg::build_manifold(&mesh.vertices, &mesh.indices)
        .expect("post-clip wall must remain a closed manifold");
    let vol = m.volume() * 1.0e-9_f64;
    let expected = 0.05_f64; // 500 × 50 × 2000 mm³
    assert!(
        (vol - expected).abs() < 0.005,
        "expected ~{expected} m³ after 3-deep clipping, got {vol} m³"
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

// ---------- Cross-product IfcRelVoidsElement (GH #21) ----------

/// Same wall + opening geometry as `WALL_WITH_OPENING`, but authored
/// the cross-product way: plain `IfcWall` extrusion + separately-
/// modelled `IfcOpeningElement` extrusion + an `IfcRelVoidsElement`
/// linking them. Older Revit exports and some MEP authoring tools
/// produce this pattern instead of `IfcBooleanClippingResult`.
///
/// Geometry: wall 1000×200×3000 mm at world origin (centred profile);
/// opening 500×200×2000 mm at world origin with the extrusion itself
/// placed at z=500 — so the opening box occupies the same volume
/// inside the wall as the in-rep fixture. Expected post-cut wall
/// volume: 0.6 − 0.2 = 0.4 m³.
const WALL_WITH_CROSS_PRODUCT_OPENING: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('voids.ifc','2026-05-31T00:00:00',('test'),('skiplum'),'ifcfast','ifcfast','');
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
#34=IFCSHAPEREPRESENTATION(#5,'Body','SweptSolid',(#33));
#35=IFCPRODUCTDEFINITIONSHAPE($,$,(#34));
#40=IFCRECTANGLEPROFILEDEF(.AREA.,'OpeningRect',#41,500.,200.);
#41=IFCAXIS2PLACEMENT2D(#7,$);
#42=IFCAXIS2PLACEMENT3D(#43,$,$);
#43=IFCCARTESIANPOINT((0.,0.,500.));
#44=IFCEXTRUDEDAREASOLID(#40,#42,#32,2000.);
#45=IFCSHAPEREPRESENTATION(#5,'Body','SweptSolid',(#44));
#46=IFCPRODUCTDEFINITIONSHAPE($,$,(#45));
#50=IFCWALL('7Wall00000000000000001',$,'TestWall',$,$,#16,#35,'tag',.STANDARD.);
#51=IFCOPENINGELEMENT('8Open00000000000000001',$,'Opening',$,$,#16,#46,'tag',.OPENING.);
#60=IFCRELVOIDSELEMENT('9Rel000000000000000001',$,$,$,#50,#51);
#70=IFCRELCONTAINEDINSPATIALSTRUCTURE('ARelC0000000000000001',$,$,$,(#50),#12);
ENDSEC;
END-ISO-10303-21;
"#;

#[test]
fn cross_product_voids_indexer_captures_relation() {
    let idx = indexer::index(WALL_WITH_CROSS_PRODUCT_OPENING.as_bytes());
    assert_eq!(
        idx.voids_opening.len(),
        1,
        "indexer must record the single IfcRelVoidsElement",
    );
    assert_eq!(idx.voids_host.len(), 1);
    // Host (RelatingBuildingElement = #50, the wall) and opening
    // (RelatedOpeningElement = #51) must be parsed in the documented
    // order — the cross-product cut buffer relies on this.
    assert_eq!(idx.voids_host[0], 50);
    assert_eq!(idx.voids_opening[0], 51);
}

#[test]
fn cross_product_voids_reveal_all_emits_both_products() {
    // With cut_openings off (the default reveal-all behaviour), both
    // the wall AND the opening reach the sink — neither is suppressed.
    let mut sink = CaptureSink {
        products: Vec::new(),
    };
    let _ = mesh_ifc_streaming(
        WALL_WITH_CROSS_PRODUCT_OPENING.as_bytes(),
        &mut sink,
    );
    let entities: Vec<&str> = sink
        .products
        .iter()
        .map(|m| m.entity.as_str())
        .collect();
    assert!(
        entities.iter().any(|e| *e == "IfcWall"),
        "reveal-all must keep the wall; got {entities:?}",
    );
    assert!(
        entities.iter().any(|e| *e == "IfcOpeningElement"),
        "reveal-all must keep the opening as a separate product; got {entities:?}",
    );
}

#[test]
fn cross_product_voids_fold_subtracts_opening_volume() {
    let buf = WALL_WITH_CROSS_PRODUCT_OPENING.as_bytes();
    let idx = indexer::index(buf);

    // Build the cut buffer from the indexer's relationship arrays —
    // same construction `extract_meshes` performs in production.
    let mut cross = CrossProductCut::from_indexer(&idx.voids_opening, &idx.voids_host);
    assert!(!cross.is_empty(), "cut buffer must see the void relation");

    // Stream the file; route every emitted product through the cut
    // buffer the same way `MeshSink` does. Opening should be
    // suppressed, wall should be held; no PassThrough products in
    // this fixture (no spatial structure products are meshable).
    let mut sink = CaptureSink {
        products: Vec::new(),
    };
    let _ = mesh_ifc_streaming(buf, &mut sink);
    let mut passthrough: Vec<ProductMesh> = Vec::new();
    let mut suppressed = 0_usize;
    let mut held = 0_usize;
    for mesh in sink.products {
        match cross.route(mesh) {
            Routed::Suppressed => suppressed += 1,
            Routed::Held => held += 1,
            Routed::PassThrough(m) => passthrough.push(m),
        }
    }
    assert_eq!(suppressed, 1, "the opening must be suppressed (cutter, not visible)");
    assert_eq!(held, 1, "the wall must be held for the fold");
    assert!(
        passthrough.is_empty(),
        "no products should pass through in this minimal fixture, got {} entities: {:?}",
        passthrough.len(),
        passthrough.iter().map(|m| m.entity.as_str()).collect::<Vec<_>>(),
    );

    // Flush — fold the wall with its arrived openings.
    let folded = cross.flush();
    assert_eq!(folded.len(), 1, "exactly one host emerges from the flush");
    let (wall_mesh, outcome) = &folded[0];
    assert_eq!(*outcome, Outcome::Cut, "the cross-product cut must succeed");
    assert_eq!(wall_mesh.entity, "IfcWall");
    assert_eq!(
        wall_mesh.segments.len(),
        1,
        "folded mesh must collapse to a single segment",
    );
    assert_eq!(wall_mesh.segments[0].source, "cut_openings");

    // Volume check: 0.6 m³ host − 0.2 m³ opening = 0.4 m³.
    let m = csg::build_manifold(&wall_mesh.vertices, &wall_mesh.indices)
        .expect("post-cut wall must be a closed manifold");
    let volume_m3 = m.volume() * 1.0e-9_f64;
    let expected = 0.4_f64;
    assert!(
        (volume_m3 - expected).abs() < 0.02,
        "expected ~{expected} m³ after cross-product cut, got {volume_m3} m³",
    );
}

#[test]
fn cross_product_buffer_is_empty_when_no_voids_in_file() {
    // PLAIN_WALL (from the in-rep test above) carries no
    // IfcRelVoidsElement — the cross-product cut buffer should
    // construct empty, so the wrapper short-circuits and the hot
    // path stays identical to the no-cut behaviour.
    let buf = b"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('x'),'2;1');
FILE_NAME('x.ifc','x',('x'),('x'),'x','x','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCPROJECT('0',$,'p',$,$,$,$,$,$);
ENDSEC;
END-ISO-10303-21;
";
    let idx = indexer::index(buf);
    let cross = CrossProductCut::from_indexer(&idx.voids_opening, &idx.voids_host);
    assert!(cross.is_empty(), "no voids → buffer must be empty");
}

/// GH #52: IfcPolygonalBoundedHalfSpace where BaseSurface.Position
/// (the plane) and IfcPolygonalBoundedHalfSpace.Position (the polygon
/// frame) diverge. Sannergata wall #50724 in miniature.
///
/// Geometry: wall extrusion is the standard 1000 × 200 × 3000 mm box
/// (X ∈ [-500, 500], Y ∈ [-100, 100], Z ∈ [0, 3000]). Cutter:
///   - BaseSurface plane normal = (0, 0, -1), origin = (0, 0, 2500) —
///     normal points DOWN. Under the de-facto `.T.` convention we
///     keep +normal side, i.e. Pz < 2500 → upper 500 mm of the wall
///     is removed.
///   - PolygonalBoundary polygon (large enough to cover the whole
///     wall footprint in the polygon's local XY).
///   - IfcPolygonalBoundedHalfSpace.Position = identity (axis +Z).
///
/// BaseSurface.Position.Axis = (0, 0, -1), polygon-Position.Axis =
/// (0, 0, 1). Pre-#52, the slab used the polygon's Position, so its
/// world normal was +Z and `clip_by_plane` would have kept Pz < 2500
/// (the LOWER half — exactly opposite of what BaseSurface said).
/// Wait — that happens to match in this axis-symmetric case. The
/// REAL pre-#52 failure showed up on the Sannergata-style cutter
/// with normal (-0.02, 0, -0.9998); axis-aligned cases were
/// degenerate. Let me build the actual failure case.
///
/// Replicating Sannergata #50724 at miniature scale: BaseSurface
/// plane normal = (0, 0, -1), origin = (0, 0, 2500) — keeps lower
/// half (z < 2500). With the polygon-Position being +Z (default),
/// pre-#52 the slab's normal landed on the polygon's +Z → clip kept
/// the side based on polygon-Z, which IS (0, 0, +1) — opposite of
/// the BaseSurface normal. Post-#52 the slab takes BaseSurface's
/// normal direct so the clip uses the right direction.
const WALL_DIVERGING_POSITION_HALFSPACE: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('gh52.ifc','2026-06-04T00:00:00',('test'),('skiplum'),'ifcfast','ifcfast','');
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
#12=IFCBUILDINGSTOREY('3Stor000000000000000001',$,'L1',$,$,#15,$,$,.ELEMENT.,0.0);
#15=IFCLOCALPLACEMENT($,#6);
#16=IFCLOCALPLACEMENT(#15,#6);
#20=IFCRELAGGREGATES('4Agg000000000000000001',$,$,$,#1,(#10));
#21=IFCRELAGGREGATES('5Agg000000000000000001',$,$,$,#10,(#11));
#22=IFCRELAGGREGATES('6Agg000000000000000001',$,$,$,#11,(#12));
#30=IFCRECTANGLEPROFILEDEF(.AREA.,'WallRect',#31,1000.,200.);
#31=IFCAXIS2PLACEMENT2D(#7,$);
#32=IFCDIRECTION((0.,0.,1.));
#33=IFCEXTRUDEDAREASOLID(#30,#6,#32,3000.);
#40=IFCCARTESIANPOINT((0.,0.,2500.));
#41=IFCDIRECTION((0.,0.,-1.));
#42=IFCDIRECTION((1.,0.,0.));
#43=IFCAXIS2PLACEMENT3D(#40,#41,#42);
#44=IFCPLANE(#43);
#50=IFCAXIS2PLACEMENT3D(#7,$,$);
#51=IFCCARTESIANPOINT((-1000.,-500.));
#52=IFCCARTESIANPOINT(( 1000.,-500.));
#53=IFCCARTESIANPOINT(( 1000., 500.));
#54=IFCCARTESIANPOINT((-1000., 500.));
#55=IFCCARTESIANPOINT((-1000.,-500.));
#56=IFCPOLYLINE((#51,#52,#53,#54,#55));
#57=IFCPOLYGONALBOUNDEDHALFSPACE(#44,.T.,#50,#56);
#60=IFCBOOLEANCLIPPINGRESULT(.DIFFERENCE.,#33,#57);
#70=IFCSHAPEREPRESENTATION(#5,'Body','Clipping',(#60));
#71=IFCPRODUCTDEFINITIONSHAPE($,$,(#70));
#80=IFCWALL('7Wall00000000000000001',$,'GH52',$,$,#16,#71,'tag',.STANDARD.);
#90=IFCRELCONTAINEDINSPATIALSTRUCTURE('8Rel000000000000000001',$,$,$,(#80),#12);
ENDSEC;
END-ISO-10303-21;
"#;

#[test]
fn polygonal_bounded_uses_base_surface_position_normal() {
    // Wall is X∈[-500,500], Y∈[-100,100], Z∈[0,3000]. Volume = 0.6 m³.
    // Cutter's BaseSurface plane normal points -Z (down) at z=2500.
    // Under .T. (de-facto convention) we keep +normal side, i.e.
    // -(-Z) = +Z below the plane meaning Pz < 2500. Wait — keep
    // +normal side means (P-origin)·normal > 0:
    //   normal = (0, 0, -1), origin = (0, 0, 2500).
    //   keep where -1*(Pz - 2500) > 0 → Pz < 2500.
    // So we keep the lower 2500 mm of the wall. Volume = 1000 × 200
    // × 2500 = 5e8 mm³ = 0.5 m³.
    let mut sink = CaptureSink { products: Vec::new() };
    let _ = mesh_ifc_streaming(WALL_DIVERGING_POSITION_HALFSPACE.as_bytes(), &mut sink);
    let mut mesh = sink
        .products
        .into_iter()
        .find(|m| m.entity == "IfcWall")
        .expect("wall must reach the sink");
    let outcome = apply(&mut mesh);
    assert_eq!(outcome, Outcome::Cut);
    let m = csg::build_manifold(&mesh.vertices, &mesh.indices)
        .expect("post-clip wall is a closed manifold");
    let vol_m3 = m.volume() * 1.0e-9_f64;
    let expected = 0.5_f64;
    assert!(
        (vol_m3 - expected).abs() < 0.02,
        "expected ~{expected} m³ (lower 2500 mm kept), got {vol_m3} m³ — \
         pre-#52 this used the polygon-Position's +Z normal instead of \
         BaseSurface.Position's -Z and clipped the wrong half (or \
         emptied entirely when BaseSurface was tilted)",
    );
    // Bbox: top of the kept body is z ≈ 2500.
    let mut zmax = f32::NEG_INFINITY;
    for chunk in mesh.vertices.chunks_exact(3) {
        zmax = zmax.max(chunk[2]);
    }
    assert!(zmax <= 2510.0, "upper half should be gone, zmax = {zmax}");
}
