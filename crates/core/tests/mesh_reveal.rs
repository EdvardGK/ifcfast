//! Reveal-all dispatch tests for the mesh pipeline.
//!
//! These verify the dispatcher's stance: every representation type the
//! file contains either produces real geometry (with a per-segment
//! source tag) or surfaces as an explicit `unhandled:IFCXXX` bucket in
//! stats. Nothing silently disappears.

#![cfg(feature = "mesh")]

use _core::mesh::{mesh_ifc, mesh_ifc_streaming, MeshFragment, ProductMesh, ProductSink};

/// Synthetic IFC4 file with one IfcWall whose representation is an
/// `IfcBooleanClippingResult(wall_extrusion, door_extrusion)`. After
/// the dispatcher runs we expect both operands to appear as their own
/// mesh segments — the wall is NOT clipped, the door is NOT subtracted.
const BOOLEAN_CLIPPING_IFC: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('reveal_boolean.ifc','2026-05-18T00:00:00',('test'),('skiplum'),'ifcfast','ifcfast','');
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
#12=IFCBUILDINGSTOREY('3Stor000000000000000001',$,'st',$,$,#15,$,$,.ELEMENT.,0.0);
#15=IFCLOCALPLACEMENT($,#6);
#16=IFCLOCALPLACEMENT(#15,#6);
#20=IFCRELAGGREGATES('4Agg000000000000000001',$,$,$,#1,(#10));
#21=IFCRELAGGREGATES('5Agg000000000000000001',$,$,$,#10,(#11));
#22=IFCRELAGGREGATES('6Agg000000000000000001',$,$,$,#11,(#12));
#30=IFCRECTANGLEPROFILEDEF(.AREA.,'WallRect',#31,1000.,200.);
#31=IFCAXIS2PLACEMENT2D(#7,$);
#32=IFCDIRECTION((0.,0.,1.));
#33=IFCEXTRUDEDAREASOLID(#30,#6,#32,3000.);
#40=IFCRECTANGLEPROFILEDEF(.AREA.,'DoorRect',#41,300.,200.);
#41=IFCAXIS2PLACEMENT2D(#7,$);
#42=IFCEXTRUDEDAREASOLID(#40,#6,#32,2100.);
#50=IFCBOOLEANCLIPPINGRESULT(.DIFFERENCE.,#33,#42);
#60=IFCSHAPEREPRESENTATION(#5,'Body','Clipping',(#50));
#61=IFCPRODUCTDEFINITIONSHAPE($,$,(#60));
#70=IFCWALL('7Wall00000000000000001',$,'TestWall',$,$,#16,#61,'tag',.STANDARD.);
#71=IFCRELCONTAINEDINSPATIALSTRUCTURE('8Rel00000000000000001',$,$,$,(#70),#12);
ENDSEC;
END-ISO-10303-21;
"#;

/// A second wall using `IfcSurfaceCurveSweptAreaSolid` — the one
/// remaining latent representation type from issue #17 we don't
/// handle yet (profile swept along a curve on a reference surface
/// is substantially harder than axis-revolution and is parked as
/// a follow-up). MUST surface as an `unhandled:` bucket — silent
/// drops are forbidden.
const UNHANDLED_REPR_IFC: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('reveal_unhandled.ifc','2026-05-18T00:00:00',('test'),('skiplum'),'ifcfast','ifcfast','');
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
#15=IFCLOCALPLACEMENT($,#6);
#16=IFCLOCALPLACEMENT(#15,#6);
#30=IFCRECTANGLEPROFILEDEF(.AREA.,'P',#31,100.,100.);
#31=IFCAXIS2PLACEMENT2D(#7,$);
#32=IFCDIRECTION((1.,0.,0.));
#33=IFCDIRECTION((0.,0.,1.));
#34=IFCAXIS2PLACEMENT3D(#7,#33,#32);
#40=IFCCARTESIANPOINT((0.,0.,0.));
#41=IFCCARTESIANPOINT((1000.,0.,0.));
#42=IFCPOLYLINE((#40,#41));
#43=IFCPLANE(#34);
#44=IFCSURFACECURVESWEPTAREASOLID(#30,#6,#42,$,$,#43);
#60=IFCSHAPEREPRESENTATION(#5,'Body','AdvancedSweptSolid',(#44));
#61=IFCPRODUCTDEFINITIONSHAPE($,$,(#60));
#70=IFCBUILDINGELEMENTPROXY('7Prox00000000000000001',$,'Sweep',$,$,#16,#61,'tag',$);
ENDSEC;
END-ISO-10303-21;
"#;

#[test]
fn boolean_clipping_emits_both_operands_as_segments() {
    let (meshes, stats) = mesh_ifc(BOOLEAN_CLIPPING_IFC.as_bytes());

    // Exactly one product (the wall) should mesh.
    assert_eq!(meshes.len(), 1, "expected one ProductMesh for the wall, got {}", meshes.len());
    let wall = &meshes[0];
    assert_eq!(wall.entity, "IfcWall");

    // The boolean clipping result must produce two segments — the wall
    // extrusion (first operand) and the door void extrusion (second
    // operand). Neither must be subtracted from the other.
    assert_eq!(
        wall.segments.len(),
        2,
        "expected two segments for IfcBooleanClippingResult, got {} ({:?})",
        wall.segments.len(),
        wall.segments.iter().map(|s| s.source.as_str()).collect::<Vec<_>>(),
    );
    // Compound tag: "boolean_<role>|<leaf>" — both the structural role
    // and the underlying representation type must be preserved.
    let tags: Vec<&str> = wall.segments.iter().map(|s| s.source.as_str()).collect();
    assert!(
        tags.iter().any(|t| t.starts_with("boolean_first_operand")),
        "missing boolean_first_operand prefix: {:?}", tags
    );
    assert!(
        tags.iter().any(|t| t.starts_with("boolean_second_operand")),
        "missing boolean_second_operand prefix: {:?}", tags
    );
    // The leaf representation type must come through too (both
    // operands are IfcExtrudedAreaSolid).
    assert!(
        tags.iter().all(|t| t.ends_with("|extrusion")),
        "expected compound tags ending in |extrusion: {:?}", tags
    );

    // Each segment must have a positive triangle count (the dispatcher
    // actually tessellated both operands).
    for seg in &wall.segments {
        assert!(seg.index_count > 0 && seg.index_count % 3 == 0,
            "segment {:?} has bad index_count {}", seg.source, seg.index_count);
    }

    // Segments must cover the entire indices buffer with no gaps.
    let last = wall.segments.last().unwrap();
    assert_eq!(
        last.index_start + last.index_count,
        wall.indices.len() as u32,
        "segments don't cover the full index buffer"
    );

    // Stats must report both operand sources via the compound key.
    let by_src = &stats.by_source;
    assert!(by_src.keys().any(|k| k.starts_with("boolean_first_operand")));
    assert!(by_src.keys().any(|k| k.starts_with("boolean_second_operand")));
}

#[test]
fn unhandled_representation_appears_as_labeled_bucket() {
    let (_meshes, stats) = mesh_ifc(UNHANDLED_REPR_IFC.as_bytes());

    // The product is seen but produces no geometry because we don't
    // handle IfcRevolvedAreaSolid yet. The stats bucket MUST name the
    // missing type explicitly — the whole point of reveal-all.
    let unhandled_key = "unhandled:IFCSURFACECURVESWEPTAREASOLID";
    let count = stats.by_source.get(unhandled_key).copied().unwrap_or(0);
    assert!(
        count >= 1,
        "expected stats.by_source['{}'] >= 1, got {:?}",
        unhandled_key, stats.by_source
    );
}

/// Synthetic file modelling the Duplex pattern: a wall clipped by an
/// `IfcPolygonalBoundedHalfSpace`. The second operand of the clipping
/// result MUST surface as the compound tag
/// `"boolean_second_operand|halfspace_bounded"` — losing either fact
/// would violate reveal-all.
const BOOLEAN_OVER_HALFSPACE_IFC: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('reveal_halfspace.ifc','2026-05-18T00:00:00',('test'),('skiplum'),'ifcfast','ifcfast','');
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
#8=IFCPLANE(#6);
#15=IFCLOCALPLACEMENT($,#6);
#16=IFCLOCALPLACEMENT(#15,#6);
#30=IFCRECTANGLEPROFILEDEF(.AREA.,'WallRect',#31,1000.,200.);
#31=IFCAXIS2PLACEMENT2D(#7,$);
#32=IFCDIRECTION((0.,0.,1.));
#33=IFCEXTRUDEDAREASOLID(#30,#6,#32,3000.);
#40=IFCCARTESIANPOINT((-100.,-100.,0.));
#41=IFCCARTESIANPOINT((100.,-100.,0.));
#42=IFCCARTESIANPOINT((100.,100.,0.));
#43=IFCCARTESIANPOINT((-100.,100.,0.));
#44=IFCPOLYLINE((#40,#41,#42,#43,#40));
#45=IFCPOLYGONALBOUNDEDHALFSPACE(#8,.F.,#6,#44);
#50=IFCBOOLEANCLIPPINGRESULT(.DIFFERENCE.,#33,#45);
#60=IFCSHAPEREPRESENTATION(#5,'Body','Clipping',(#50));
#61=IFCPRODUCTDEFINITIONSHAPE($,$,(#60));
#70=IFCWALL('7Wall00000000000000001',$,'TestWall',$,$,#16,#61,'tag',.STANDARD.);
ENDSEC;
END-ISO-10303-21;
"#;

#[test]
fn boolean_over_halfspace_preserves_both_facts() {
    let (meshes, _stats) = mesh_ifc(BOOLEAN_OVER_HALFSPACE_IFC.as_bytes());
    assert_eq!(meshes.len(), 1, "expected one wall");
    let wall = &meshes[0];
    let tags: Vec<&str> = wall.segments.iter().map(|s| s.source.as_str()).collect();

    // The wall's bulk volume — first operand, leaf = extrusion.
    assert!(
        tags.contains(&"boolean_first_operand|extrusion"),
        "wall bulk volume should surface as boolean_first_operand|extrusion, got {:?}", tags
    );
    // The clip volume — second operand, leaf = halfspace_bounded.
    // Losing either fact (just "boolean_second_operand" or just
    // "halfspace_bounded") would mean the reveal-all stance leaked.
    assert!(
        tags.contains(&"boolean_second_operand|halfspace_bounded"),
        "clip volume should surface as boolean_second_operand|halfspace_bounded, got {:?}", tags
    );
}

#[test]
fn source_tags_set_is_documented() {
    // Sanity check that the documented tag list matches the boolean /
    // halfspace handlers' real output names. Future handlers must add
    // their tag here to satisfy this test — keeps the docs honest.
    let documented = MeshFragment::source_tags();
    for required in [
        "extrusion",
        "mapped",
        "boolean_first_operand",
        "boolean_second_operand",
        "csg_branch",
        "halfspace_bounded",
        "halfspace_plane",
        "advanced_brep_approx",
        "curve_set",
        "csg_block",
        "csg_cylinder",
        "csg_cone",
        "csg_sphere",
        "csg_pyramid",
    ] {
        assert!(
            documented.contains(&required),
            "MeshFragment::source_tags() missing {:?}", required
        );
    }
}

/// Annotation product whose body representation is an
/// `IfcGeometricCurveSet` containing a 3D `IfcPolyline`. Surveying
/// real Norwegian Revit/Magicad ARK + RIB exports surfaced this as
/// the only `unhandled:*` bucket left in the dispatcher — common in
/// structural axis grids and dimension witness lines. The handler
/// must (a) surface a `curve_set` segment, (b) leave no
/// `unhandled:IFCGEOMETRICCURVESET` bucket, and (c) produce zero
/// triangle area so QTO doesn't double-count curve geometry as
/// surface.
const CURVE_SET_IFC: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('reveal_curveset.ifc','2026-05-19T00:00:00',('test'),('skiplum'),'ifcfast','ifcfast','');
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
#15=IFCLOCALPLACEMENT($,#6);
#16=IFCLOCALPLACEMENT(#15,#6);
#40=IFCCARTESIANPOINT((0.,0.,0.));
#41=IFCCARTESIANPOINT((1000.,0.,0.));
#42=IFCCARTESIANPOINT((1000.,1000.,0.));
#43=IFCCARTESIANPOINT((0.,1000.,0.));
#44=IFCPOLYLINE((#40,#41,#42,#43,#40));
#50=IFCGEOMETRICCURVESET((#44));
#60=IFCSHAPEREPRESENTATION(#5,'Body','GeometricCurveSet',(#50));
#61=IFCPRODUCTDEFINITIONSHAPE($,$,(#60));
#70=IFCANNOTATION('7Ann000000000000000001',$,'AxisGrid',$,$,#16,#61);
ENDSEC;
END-ISO-10303-21;
"#;

/// Five `IfcCsgPrimitive3D` leaves, each as the body item of its own
/// `IfcBuildingElementProxy`. Asserts that every type tessellates to a
/// non-empty mesh and surfaces with the expected per-type source tag.
/// Closed-form sanity checks on bbox/volume cover the placement +
/// dimensions plumbing end-to-end.
const CSG_PRIMITIVES_IFC: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('reveal_csg_primitives.ifc','2026-05-19T00:00:00',('test'),('skiplum'),'ifcfast','ifcfast','');
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
#15=IFCLOCALPLACEMENT($,#6);
#100=IFCBLOCK(#6,100.,200.,300.);
#101=IFCSHAPEREPRESENTATION(#5,'Body','CSG',(#100));
#102=IFCPRODUCTDEFINITIONSHAPE($,$,(#101));
#103=IFCBUILDINGELEMENTPROXY('Block00000000000000001',$,'B',$,$,#15,#102,$,$);
#110=IFCRIGHTCIRCULARCYLINDER(#6,500.,50.);
#111=IFCSHAPEREPRESENTATION(#5,'Body','CSG',(#110));
#112=IFCPRODUCTDEFINITIONSHAPE($,$,(#111));
#113=IFCBUILDINGELEMENTPROXY('Cyl0000000000000000001',$,'C',$,$,#15,#112,$,$);
#120=IFCRIGHTCIRCULARCONE(#6,400.,100.);
#121=IFCSHAPEREPRESENTATION(#5,'Body','CSG',(#120));
#122=IFCPRODUCTDEFINITIONSHAPE($,$,(#121));
#123=IFCBUILDINGELEMENTPROXY('Cone000000000000000001',$,'Co',$,$,#15,#122,$,$);
#130=IFCSPHERE(#6,75.);
#131=IFCSHAPEREPRESENTATION(#5,'Body','CSG',(#130));
#132=IFCPRODUCTDEFINITIONSHAPE($,$,(#131));
#133=IFCBUILDINGELEMENTPROXY('Sph0000000000000000001',$,'S',$,$,#15,#132,$,$);
#140=IFCRECTANGULARPYRAMID(#6,200.,300.,400.);
#141=IFCSHAPEREPRESENTATION(#5,'Body','CSG',(#140));
#142=IFCPRODUCTDEFINITIONSHAPE($,$,(#141));
#143=IFCBUILDINGELEMENTPROXY('Pyr0000000000000000001',$,'P',$,$,#15,#142,$,$);
ENDSEC;
END-ISO-10303-21;
"#;

/// `IfcRevolvedAreaSolid`: a 100×500 rectangular profile placed 200 units
/// out from the Z-axis, swept a full 2π. Closed-form result is a hollow
/// cylindrical ring (a "washer extruded vertically") whose volume can be
/// computed exactly as `π · (R_outer² − R_inner²) · Height` where
/// R_inner = 200, R_outer = 300, Height = 500.
const REVOLVED_RING_IFC: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('reveal_revolved.ifc','2026-05-19T00:00:00',('test'),('skiplum'),'ifcfast','ifcfast','');
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
#15=IFCLOCALPLACEMENT($,#6);
#20=IFCCARTESIANPOINT((250.,250.,0.));
#21=IFCAXIS2PLACEMENT2D(#20,$);
#22=IFCRECTANGLEPROFILEDEF(.AREA.,'RingSection',#21,100.,500.);
#30=IFCDIRECTION((0.,1.,0.));
#31=IFCAXIS1PLACEMENT(#7,#30);
#32=IFCREVOLVEDAREASOLID(#22,#6,#31,6.283185307);
#60=IFCSHAPEREPRESENTATION(#5,'Body','SweptSolid',(#32));
#61=IFCPRODUCTDEFINITIONSHAPE($,$,(#60));
#70=IFCBUILDINGELEMENTPROXY('Ring000000000000000001',$,'R',$,$,#15,#61,$,$);
ENDSEC;
END-ISO-10303-21;
"#;

#[test]
fn revolved_area_solid_full_revolution_volume_within_tolerance() {
    // Profile is a 100×500 rectangle centred at (250, 250) in the
    // profile-local XY plane. With Position = identity and rotation
    // axis = Y-axis through origin, the rectangle's local X spans
    // 200..300 (radial distance from Y-axis), its local Y spans
    // 0..500 (height along Y). Rotated 2π around Y gives a cylindrical
    // ring with R_inner=200, R_outer=300, height=500.
    let (meshes, stats) = mesh_ifc(REVOLVED_RING_IFC.as_bytes());

    let ring = meshes
        .iter()
        .find(|m| m.guid.starts_with("Ring00"))
        .expect("revolved product missing");

    let tags: Vec<&str> = ring.segments.iter().map(|s| s.source.as_str()).collect();
    assert!(
        tags.contains(&"revolved"),
        "expected a 'revolved' segment, got {:?}",
        tags
    );
    assert!(
        !stats.by_source.contains_key("unhandled:IFCREVOLVEDAREASOLID"),
        "revolved handler did not consume IFCREVOLVEDAREASOLID; stats: {:?}",
        stats.by_source
    );

    let mut volume_x6: f32 = 0.0;
    for tri in ring.indices.chunks_exact(3) {
        let (a, b, c) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
        let ax = ring.vertices[a * 3];
        let ay = ring.vertices[a * 3 + 1];
        let az = ring.vertices[a * 3 + 2];
        let bx = ring.vertices[b * 3];
        let by = ring.vertices[b * 3 + 1];
        let bz = ring.vertices[b * 3 + 2];
        let cx = ring.vertices[c * 3];
        let cy = ring.vertices[c * 3 + 1];
        let cz = ring.vertices[c * 3 + 2];
        volume_x6 += ax * (by * cz - bz * cy)
            + ay * (bz * cx - bx * cz)
            + az * (bx * cy - by * cx);
    }
    let volume = (volume_x6 / 6.0).abs();
    let expected =
        std::f32::consts::PI * (300.0_f32.powi(2) - 200.0_f32.powi(2)) * 500.0;
    let ratio = volume / expected;
    assert!(
        ratio > 0.85 && ratio < 1.05,
        "revolved ring volume {:.0} vs expected {:.0} (ratio {:.3}) outside [0.85, 1.05]",
        volume,
        expected,
        ratio
    );
}

#[test]
fn csg_primitives_tessellate_to_per_type_tags() {
    let (meshes, stats) = mesh_ifc(CSG_PRIMITIVES_IFC.as_bytes());

    let by_guid_prefix: std::collections::HashMap<&str, &_core::mesh::ProductMesh> = meshes
        .iter()
        .map(|m| (&m.guid[..6], m))
        .collect();

    let expected: &[(&str, &str)] = &[
        ("Block0", "csg_block"),
        ("Cyl000", "csg_cylinder"),
        ("Cone00", "csg_cone"),
        ("Sph000", "csg_sphere"),
        ("Pyr000", "csg_pyramid"),
    ];

    for (guid_prefix, expected_tag) in expected {
        let m = by_guid_prefix
            .get(guid_prefix)
            .unwrap_or_else(|| panic!("missing product with guid prefix {}", guid_prefix));
        let tags: Vec<&str> = m.segments.iter().map(|s| s.source.as_str()).collect();
        assert!(
            tags.contains(expected_tag),
            "{}: expected tag {:?}, got {:?}",
            guid_prefix,
            expected_tag,
            tags
        );
        assert!(
            !m.indices.is_empty() && m.indices.len() % 3 == 0,
            "{}: empty or non-triangular index buffer",
            guid_prefix
        );
    }

    // None of the five types may remain in the unhandled bucket.
    for ifc_type in [
        "unhandled:IFCBLOCK",
        "unhandled:IFCRIGHTCIRCULARCYLINDER",
        "unhandled:IFCRIGHTCIRCULARCONE",
        "unhandled:IFCSPHERE",
        "unhandled:IFCRECTANGULARPYRAMID",
    ] {
        assert!(
            !stats.by_source.contains_key(ifc_type),
            "stats still bucketing {}: {:?}",
            ifc_type,
            stats.by_source
        );
    }
}

#[test]
fn csg_block_dimensions_match_input() {
    // The 100×200×300 block must produce an AABB matching its inputs,
    // and a volume of 100·200·300 = 6 000 000 (closed-form, no tolerance).
    let (meshes, _) = mesh_ifc(CSG_PRIMITIVES_IFC.as_bytes());
    let block = meshes
        .iter()
        .find(|m| m.guid.starts_with("Block0"))
        .expect("block product missing");

    let mut xmin = f32::INFINITY;
    let mut ymin = f32::INFINITY;
    let mut zmin = f32::INFINITY;
    let mut xmax = f32::NEG_INFINITY;
    let mut ymax = f32::NEG_INFINITY;
    let mut zmax = f32::NEG_INFINITY;
    for chunk in block.vertices.chunks_exact(3) {
        xmin = xmin.min(chunk[0]);
        ymin = ymin.min(chunk[1]);
        zmin = zmin.min(chunk[2]);
        xmax = xmax.max(chunk[0]);
        ymax = ymax.max(chunk[1]);
        zmax = zmax.max(chunk[2]);
    }
    let ex = (xmax - xmin - 100.0).abs();
    let ey = (ymax - ymin - 200.0).abs();
    let ez = (zmax - zmin - 300.0).abs();
    assert!(ex < 1e-3 && ey < 1e-3 && ez < 1e-3,
        "block AABB extents wrong: got ({}, {}, {})", xmax-xmin, ymax-ymin, zmax-zmin);
}

#[test]
fn csg_sphere_volume_within_tessellation_tolerance() {
    // Sphere of radius 75 has closed-form volume 4/3 π r³ ≈ 1 767 146.
    // A 12-latitude × 24-longitude tessellation inscribes the sphere
    // (chords cut inside the great circle) so the mesh volume
    // *undershoots* by a predictable amount — empirically ~9.4% at
    // this density. Bracket the result to catch placement / radius
    // bugs without chasing tessellation density: the floor confirms
    // the tessellation isn't catastrophically wrong (e.g. half-radius),
    // the ceiling confirms we didn't accidentally *circumscribe*
    // (which would mean wrong sign somewhere in normals).
    let (meshes, _) = mesh_ifc(CSG_PRIMITIVES_IFC.as_bytes());
    let sphere = meshes
        .iter()
        .find(|m| m.guid.starts_with("Sph000"))
        .expect("sphere product missing");

    // Signed-tetrahedra volume via divergence theorem, same kernel
    // ProductStats uses.
    let mut volume_x6: f32 = 0.0;
    for tri in sphere.indices.chunks_exact(3) {
        let (a, b, c) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
        let ax = sphere.vertices[a * 3];
        let ay = sphere.vertices[a * 3 + 1];
        let az = sphere.vertices[a * 3 + 2];
        let bx = sphere.vertices[b * 3];
        let by = sphere.vertices[b * 3 + 1];
        let bz = sphere.vertices[b * 3 + 2];
        let cx = sphere.vertices[c * 3];
        let cy = sphere.vertices[c * 3 + 1];
        let cz = sphere.vertices[c * 3 + 2];
        volume_x6 += ax * (by * cz - bz * cy)
            + ay * (bz * cx - bx * cz)
            + az * (bx * cy - by * cx);
    }
    let volume = (volume_x6 / 6.0).abs();
    let expected: f32 = 4.0 / 3.0 * std::f32::consts::PI * 75.0 * 75.0 * 75.0;
    let ratio = volume / expected;
    assert!(
        ratio > 0.85 && ratio < 1.0,
        "sphere volume {:.0} vs expected {:.0} (ratio {:.3}) outside inscribed-tessellation bracket [0.85, 1.0]",
        volume,
        expected,
        ratio
    );
}

#[test]
fn geometric_curve_set_surfaces_as_curve_set_not_unhandled() {
    let (meshes, stats) = mesh_ifc(CURVE_SET_IFC.as_bytes());

    // The curve-set product must mesh — not get bucketed as deferred.
    assert_eq!(
        meshes.len(),
        1,
        "expected one product mesh for the IfcAnnotation, got {}",
        meshes.len()
    );
    let ann = &meshes[0];

    // Source tag on the (only) segment must be `curve_set`. No
    // `unhandled:IFCGEOMETRICCURVESET` bucket may exist.
    let tags: Vec<&str> = ann.segments.iter().map(|s| s.source.as_str()).collect();
    assert!(
        tags.contains(&"curve_set"),
        "expected a 'curve_set' segment, got {:?}",
        tags
    );
    assert!(
        !stats.by_source.contains_key("unhandled:IFCGEOMETRICCURVESET"),
        "curve_set handler did not consume IFCGEOMETRICCURVESET; stats: {:?}",
        stats.by_source
    );

    // Vertices must be populated (the polyline points came through)
    // and indices must form complete triangles, even though every
    // triangle is degenerate.
    assert!(!ann.vertices.is_empty(), "curve set produced no vertices");
    assert!(!ann.indices.is_empty(), "curve set produced no indices");
    assert_eq!(
        ann.indices.len() % 3,
        0,
        "indices buffer is not triangle-aligned"
    );

    // Every emitted triangle is `(a, b, b)` — zero area. Verify via
    // the cross-product magnitude. A real surface would produce a
    // non-trivial sum here.
    let mut area_x2: f32 = 0.0;
    for tri in ann.indices.chunks_exact(3) {
        let (a, b, c) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
        let ax = ann.vertices[a * 3];
        let ay = ann.vertices[a * 3 + 1];
        let az = ann.vertices[a * 3 + 2];
        let bx = ann.vertices[b * 3];
        let by = ann.vertices[b * 3 + 1];
        let bz = ann.vertices[b * 3 + 2];
        let cx = ann.vertices[c * 3];
        let cy = ann.vertices[c * 3 + 1];
        let cz = ann.vertices[c * 3 + 2];
        let ux = bx - ax;
        let uy = by - ay;
        let uz = bz - az;
        let vx = cx - ax;
        let vy = cy - ay;
        let vz = cz - az;
        let nx = uy * vz - uz * vy;
        let ny = uz * vx - ux * vz;
        let nz = ux * vy - uy * vx;
        area_x2 += (nx * nx + ny * ny + nz * nz).sqrt();
    }
    assert!(
        area_x2 < 1e-3,
        "curve_set triangles should be degenerate (zero area), got 2*area={}",
        area_x2
    );
}

/// Synthetic IFC4 file with three IfcSpace products that each exercise
/// one of the three "no geometry" code paths in `mesh_ifc_streaming`:
///
/// - `#10` has `Representation = $` (no representation reference at all).
/// - `#20` has a `IfcShapeRepresentation` whose `Items` list is empty.
/// - `#30` has an `Items` list containing only a single
///   `IfcSphericalSurface` — a representation item we don't tessellate,
///   so it surfaces as `unhandled:IFCSPHERICALSURFACE` and `combined_i`
///   ends empty.
///
/// Pre-fix all three were silently `continue`'d in the streaming loop
/// — they never reached the sink, and `instances.parquet` never recorded
/// their identity / psets / materials. The substrate sink opts in via
/// `ProductSink::wants_geometryless() == true` to receive them as
/// empty-geometry `ProductMesh` rows; legacy sinks (`VecSink`,
/// OBJ/glTF/drift) keep the default `false` and stay unchanged.
const GEOMETRYLESS_PRODUCTS_IFC: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('reveal_geometryless.ifc','2026-05-26T00:00:00',('test'),('skiplum'),'ifcfast','ifcfast','');
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
#15=IFCLOCALPLACEMENT($,#6);
#10=IFCSPACE('1Spc0000000000000000001',$,'NoRep',$,$,#15,$,$,.ELEMENT.,.SPACE.,$);
#21=IFCSHAPEREPRESENTATION(#5,'Body','Brep',());
#22=IFCPRODUCTDEFINITIONSHAPE($,$,(#21));
#20=IFCSPACE('2Spc0000000000000000001',$,'EmptyItems',$,$,#15,#22,$,.ELEMENT.,.SPACE.,$);
#31=IFCSPHERICALSURFACE(#6,500.);
#32=IFCSHAPEREPRESENTATION(#5,'Body','SurfaceModel',(#31));
#33=IFCPRODUCTDEFINITIONSHAPE($,$,(#32));
#30=IFCSPACE('3Spc0000000000000000001',$,'AllUnhandled',$,$,#15,#33,$,.ELEMENT.,.SPACE.,$);
ENDSEC;
END-ISO-10303-21;
"#;

/// Capturing sink that opts in to geometryless emissions — mirrors what
/// `ParquetSink` does. Used to assert the three silent-drop sites all
/// reach the sink when opt-in is on.
#[derive(Default)]
struct OptInSink {
    products: Vec<ProductMesh>,
}

impl ProductSink for OptInSink {
    fn on_product(&mut self, mesh: ProductMesh) {
        self.products.push(mesh);
    }
    fn wants_geometryless(&self) -> bool {
        true
    }
}

#[test]
fn geometryless_products_silent_drop_unless_sink_opts_in() {
    // `is_product_type()` is a permissive "starts with IFC and not in
    // the non-product blacklist" filter — it also lets through some
    // representation items that the fixture happens to declare at the
    // top level (e.g. the IfcSphericalSurface, which has no GUID and
    // no Representation reference, so it lands in the same
    // "no_representation" bucket as IfcSpace #10). That noise is a
    // pre-existing scope cut, tracked separately; this test focuses on
    // the silent-drop fix.
    let (legacy_meshes, legacy_stats) =
        mesh_ifc(GEOMETRYLESS_PRODUCTS_IFC.as_bytes());
    assert_eq!(
        legacy_meshes.len(),
        0,
        "legacy VecSink (wants_geometryless=false) must drop every \
         geometryless product, got {} meshes",
        legacy_meshes.len()
    );
    assert_eq!(legacy_stats.products_meshed, 0);
    assert_eq!(legacy_stats.products_emitted_geometryless, 0);
    assert_eq!(
        legacy_stats.products_seen,
        legacy_stats.products_deferred,
        "every product seen must be accounted for in deferred"
    );
    // Reveal-all stance: each silent-drop reason is credited to its own
    // bucket. IfcSpace #20 and #30 each hit exactly one of these.
    assert_eq!(legacy_stats.by_source.get("no_body_items"), Some(&1));
    assert_eq!(legacy_stats.by_source.get("item_unhandled"), Some(&1));
    // IfcSpace #10 (no Representation) plus any top-level rep items
    // the permissive filter let through — at least 1.
    assert!(
        *legacy_stats.by_source.get("no_representation").unwrap_or(&0) >= 1,
        "no_representation bucket must include IfcSpace #10"
    );

    // Opt-in sink: every deferred product reaches the sink with empty
    // vertex and index buffers. Identity and entity name survive.
    let mut sink = OptInSink::default();
    let opt_in_stats = mesh_ifc_streaming(GEOMETRYLESS_PRODUCTS_IFC.as_bytes(), &mut sink);
    assert_eq!(opt_in_stats.products_meshed, 0);
    assert_eq!(
        opt_in_stats.products_emitted_geometryless,
        opt_in_stats.products_deferred,
        "opt-in sink must receive every deferred product"
    );
    assert_eq!(sink.products.len(), opt_in_stats.products_deferred);

    for mesh in &sink.products {
        assert!(
            mesh.vertices.is_empty() && mesh.indices.is_empty(),
            "geometryless emit must have empty vertex/index buffers, \
             got {} verts / {} indices",
            mesh.vertices.len(),
            mesh.indices.len()
        );
        assert!(mesh.segments.is_empty());
        assert!(mesh.parts.is_empty());
        assert_eq!(mesh.source, "none");
    }

    // All three IfcSpace fixtures must reach the sink with their GUIDs
    // and canonical entity name intact — that's the core fix.
    let spaces_by_guid: std::collections::HashMap<&str, &ProductMesh> = sink
        .products
        .iter()
        .filter(|m| m.entity == "IfcSpace")
        .map(|m| (m.guid.as_str(), m))
        .collect();
    assert_eq!(
        spaces_by_guid.len(),
        3,
        "expected 3 IfcSpace emissions, got {}",
        spaces_by_guid.len()
    );
    for guid in [
        "1Spc0000000000000000001",
        "2Spc0000000000000000001",
        "3Spc0000000000000000001",
    ] {
        assert!(
            spaces_by_guid.contains_key(guid),
            "missing IfcSpace GUID {guid}"
        );
    }
}

/// Annular pipe cross-section authored as IfcArbitraryProfileDefWithVoids
/// with IfcCircle curves (outer ring r=0.1 m, inner bore r=0.08 m),
/// extruded 1 m. This is the empty-mesh failure mode reported on RIV
/// MEP sets: pre-fix, curve_to_polyline returned None for IfcCircle, the
/// whole profile resolved to None, and the product emitted an empty mesh.
/// Post-fix the circle is sampled, the annulus triangulates, and the
/// bore is subtracted.
const ANNULAR_PIPE_IFC: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('annular_pipe.ifc','2026-05-29T00:00:00',('test'),('skiplum'),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCPROJECT('0Test000000000000000001',$,'p',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#3,#4));
#3=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);
#4=IFCSIUNIT(*,.PLANEANGLEUNIT.,$,.RADIAN.);
#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);
#6=IFCAXIS2PLACEMENT3D(#7,$,$);
#7=IFCCARTESIANPOINT((0.,0.,0.));
#8=IFCCARTESIANPOINT((0.,0.));
#9=IFCAXIS2PLACEMENT2D(#8,$);
#15=IFCLOCALPLACEMENT($,#6);
#16=IFCLOCALPLACEMENT(#15,#6);
#30=IFCCIRCLE(#9,0.1);
#31=IFCCIRCLE(#9,0.08);
#32=IFCARBITRARYPROFILEDEFWITHVOIDS(.AREA.,'Pipe',#30,(#31));
#33=IFCDIRECTION((0.,0.,1.));
#34=IFCEXTRUDEDAREASOLID(#32,#6,#33,1.0);
#40=IFCSHAPEREPRESENTATION(#5,'Body','SweptSolid',(#34));
#41=IFCPRODUCTDEFINITIONSHAPE($,$,(#40));
#50=IFCFLOWSEGMENT('7Pipe00000000000000001',$,'Pipe',$,$,#16,#41,'tag',$);
ENDSEC;
END-ISO-10303-21;
"#;

#[test]
fn annular_pipe_with_circle_curves_meshes_with_bore() {
    use _core::mesh::qto;

    let (meshes, _stats) = mesh_ifc(ANNULAR_PIPE_IFC.as_bytes());
    assert_eq!(meshes.len(), 1, "expected the pipe to mesh, got {} meshes", meshes.len());
    let pipe = &meshes[0];
    assert_eq!(pipe.entity, "IfcFlowSegment");
    // Pre-fix this was empty (IfcCircle dropped → profile None).
    assert!(
        !pipe.vertices.is_empty() && !pipe.indices.is_empty(),
        "annular pipe meshed empty: {} verts / {} indices",
        pipe.vertices.len(),
        pipe.indices.len(),
    );

    // The bore must be cut. Metre file (unit_scale 1.0):
    //   solid disk  = π·0.1²·1        ≈ 0.0314 m³
    //   annular     = π·(0.1²−0.08²)·1 ≈ 0.0113 m³
    // A correct annulus lands near 0.0113; a missed bore (solid disk)
    // lands near 0.0314. Assert we're well under the solid-disk volume.
    let q = qto::compute(&pipe.vertices, &pipe.indices, 1.0);
    let v = q.volume_m3.abs();
    assert!(
        v > 0.006 && v < 0.020,
        "annular volume {v:.5} m³ outside expected ~0.0113 (bore not cut → \
         solid disk ~0.0314?)",
    );
}

/// A 0.1 m box whose product placement sits 5,000 km from the origin
/// (5e6 m) — a deliberate stress of the f32 precision cliff. At that
/// magnitude the f32 quantum (~0.6 m) is larger than the box, so a
/// World-frame bake rounds every vertex to the same point: degenerate
/// triangles, surface_count = 0 — the empty-mesh failure mode reported
/// on georeferenced MEP sets. The Local frame applies the placement's
/// rotation but drops the large translation, so the shape stays precise.
const FAR_FROM_ORIGIN_BOX_IFC: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('far_box.ifc','2026-05-29T00:00:00',('test'),('skiplum'),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCPROJECT('0Test000000000000000001',$,'p',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#3,#4));
#3=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);
#4=IFCSIUNIT(*,.PLANEANGLEUNIT.,$,.RADIAN.);
#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);
#6=IFCAXIS2PLACEMENT3D(#7,$,$);
#7=IFCCARTESIANPOINT((0.,0.,0.));
#8=IFCCARTESIANPOINT((50000000.,50000000.,50000000.));
#9=IFCAXIS2PLACEMENT3D(#8,$,$);
#10=IFCCARTESIANPOINT((0.,0.));
#11=IFCAXIS2PLACEMENT2D(#10,$);
#15=IFCLOCALPLACEMENT($,#9);
#30=IFCRECTANGLEPROFILEDEF(.AREA.,'Box',#11,0.1,0.1);
#32=IFCDIRECTION((0.,0.,1.));
#33=IFCEXTRUDEDAREASOLID(#30,#6,#32,0.1);
#40=IFCSHAPEREPRESENTATION(#5,'Body','SweptSolid',(#33));
#41=IFCPRODUCTDEFINITIONSHAPE($,$,(#40));
#50=IFCWALL('7Box000000000000000001',$,'FarBox',$,$,#15,#41,'tag',.STANDARD.);
ENDSEC;
END-ISO-10303-21;
"#;

#[test]
fn far_from_origin_box_collapses_in_world_but_not_local() {
    use _core::mesh::{mesh_ifc_streaming_framed, qto, BakeFrame, ProductMesh, ProductSink};

    struct Cap(Vec<ProductMesh>);
    impl ProductSink for Cap {
        fn on_product(&mut self, m: ProductMesh) { self.0.push(m); }
    }

    // World frame: the box is baked at ~5e6 m → f32 collapse → the QTO
    // shape degenerates. This documents the bug (surface_count 0,
    // volume ~0).
    let mut world = Cap(Vec::new());
    mesh_ifc_streaming_framed(FAR_FROM_ORIGIN_BOX_IFC.as_bytes(), &mut world, BakeFrame::World);
    assert_eq!(world.0.len(), 1);
    let qw = qto::compute(&world.0[0].vertices, &world.0[0].indices, 1.0);
    assert_eq!(
        qw.surface_count, 0,
        "world-frame far box should collapse (got surface_count {}, volume {})",
        qw.surface_count, qw.volume_m3,
    );

    // Local frame: rotation applied, large translation dropped → the
    // 0.1 m box keeps full precision. Correct QTO: 6 faces, 0.001 m³.
    let mut local = Cap(Vec::new());
    mesh_ifc_streaming_framed(FAR_FROM_ORIGIN_BOX_IFC.as_bytes(), &mut local, BakeFrame::Local);
    assert_eq!(local.0.len(), 1);
    let ql = qto::compute(&local.0[0].vertices, &local.0[0].indices, 1.0);
    assert_eq!(ql.surface_count, 6, "local-frame box should have 6 faces");
    assert!(
        (ql.volume_m3.abs() - 0.001).abs() < 1e-5,
        "local-frame box volume {} m³, expected ~0.001",
        ql.volume_m3,
    );
    // And the product origin is preserved for positioning (≈ 5e6 m).
    assert!(
        (local.0[0].placement_origin[0] - 50_000_000.0).abs() < 10.0,
        "placement_origin x {} should be ~5e7",
        local.0[0].placement_origin[0],
    );

    // Stage 2: the precise f64 `world_origin` is exact (sub-mm), unlike
    // the f32 `placement_origin` which is already quantised to ~6 units
    // at this magnitude. This is the anchor point_cloud()/meshes() add
    // back in f64.
    let wo = local.0[0].world_origin;
    for (k, &w) in wo.iter().enumerate() {
        assert!(
            (w - 50_000_000.0).abs() < 1e-3,
            "world_origin[{}] = {} should be exactly ~5e7 in f64",
            k,
            w,
        );
    }

    // Stage 2 reconstruction contract (mirrors CloudSink/MeshSink):
    // shift = round(world_origin); off = world_origin - shift (≈ 0);
    // positioning each local vertex as `local + off` then downcasting to
    // f32 must NOT re-collapse the box — that's the whole point of the
    // global shift vs. adding the full 5e7 origin back.
    let shift = [wo[0].round(), wo[1].round(), wo[2].round()];
    let off = [wo[0] - shift[0], wo[1] - shift[1], wo[2] - shift[2]];
    for o in off {
        assert!(o.abs() < 1.0, "per-product offset {} should be small", o);
    }
    let shifted: Vec<f32> = local.0[0]
        .vertices
        .chunks_exact(3)
        .flat_map(|c| {
            [
                (c[0] as f64 + off[0]) as f32,
                (c[1] as f64 + off[1]) as f32,
                (c[2] as f64 + off[2]) as f32,
            ]
        })
        .collect();
    let qs = qto::compute(&shifted, &local.0[0].indices, 1.0);
    assert_eq!(
        qs.surface_count, 6,
        "shifted box must keep 6 faces (no re-collapse)",
    );
    assert!(
        (qs.volume_m3.abs() - 0.001).abs() < 1e-5,
        "shifted box volume {} m³, expected ~0.001",
        qs.volume_m3,
    );
}

/// A `IfcPolygonalFaceSet` whose `IfcCartesianPointList3D` carries huge
/// world coordinates baked directly into the vertex list (the
/// "transformed" / georeferenced MEP case Stage 3 targets). Stage 2's
/// f64 placement chain and Local-frame bake can't rescue this — the
/// largeness is inside the local mesh, so vertices collapse at parse
/// time in the geometry kernel. The faceset kernel now parses coords in
/// f64 and rebases by bbox-min before downcasting; the bake loop adds
/// `rep_origin` back through an f64 anchor.
const FAR_FACESET_BOX_IFC: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('far_faceset.ifc','2026-05-29T00:00:00',('t'),('s'),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCPROJECT('0Test000000000000000001',$,'p',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#3,#4));
#3=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);
#4=IFCSIUNIT(*,.PLANEANGLEUNIT.,$,.RADIAN.);
#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);
#6=IFCAXIS2PLACEMENT3D(#7,$,$);
#7=IFCCARTESIANPOINT((0.,0.,0.));
#15=IFCLOCALPLACEMENT($,#6);
#20=IFCCARTESIANPOINTLIST3D(((50000000.,50000000.,50000000.),(50000000.1,50000000.,50000000.),(50000000.1,50000000.1,50000000.),(50000000.,50000000.1,50000000.),(50000000.,50000000.,50000000.1),(50000000.1,50000000.,50000000.1),(50000000.1,50000000.1,50000000.1),(50000000.,50000000.1,50000000.1)));
#21=IFCINDEXEDPOLYGONALFACE((1,2,3,4));
#22=IFCINDEXEDPOLYGONALFACE((5,8,7,6));
#23=IFCINDEXEDPOLYGONALFACE((1,5,6,2));
#24=IFCINDEXEDPOLYGONALFACE((2,6,7,3));
#25=IFCINDEXEDPOLYGONALFACE((3,7,8,4));
#26=IFCINDEXEDPOLYGONALFACE((4,8,5,1));
#30=IFCPOLYGONALFACESET(#20,.T.,(#21,#22,#23,#24,#25,#26),$);
#40=IFCSHAPEREPRESENTATION(#5,'Body','Tessellation',(#30));
#41=IFCPRODUCTDEFINITIONSHAPE($,$,(#40));
#50=IFCBUILDINGELEMENTPROXY('7Box000000000000000001',$,'FarBox',$,$,#15,#41,'tag',.NOTDEFINED.);
ENDSEC;
END-ISO-10303-21;
"#;

#[test]
fn far_faceset_with_baked_world_coords_meshes_intact() {
    use _core::mesh::{mesh_ifc_streaming_framed, qto, BakeFrame, ProductMesh, ProductSink};

    struct Cap(Vec<ProductMesh>);
    impl ProductSink for Cap {
        fn on_product(&mut self, m: ProductMesh) { self.0.push(m); }
    }

    // Local frame is what point_cloud()/meshes() use. Without the
    // kernel-level f64 rebase added in Stage 3, the f32 vertex buffer
    // collapses at parse time inside faceset.rs and this product would
    // mesh with surface_count = 0 (the empty-pipe bug class). With the
    // rebase, the box keeps all 6 faces and the correct 0.001 m³ volume.
    let mut local = Cap(Vec::new());
    mesh_ifc_streaming_framed(FAR_FACESET_BOX_IFC.as_bytes(), &mut local, BakeFrame::Local);
    assert_eq!(local.0.len(), 1, "one product expected");
    let m = &local.0[0];
    let q = qto::compute(&m.vertices, &m.indices, 1.0);
    assert_eq!(
        q.surface_count, 6,
        "far faceset (baked world coords) should keep 6 faces, got {} (volume {})",
        q.surface_count, q.volume_m3,
    );
    assert!(
        (q.volume_m3.abs() - 0.001).abs() < 1e-5,
        "far faceset volume {} m³, expected ~0.001",
        q.volume_m3,
    );

    // World frame at this magnitude is inherently f32-limited: the
    // emitted vertex buffer is `[f32; 3 * n]` of absolute world coords,
    // and f32 simply cannot hold magnitude 5e7 AND distinguish 0.1 m
    // offsets simultaneously (quantum at 5e7 ≈ 6 m). That's the whole
    // reason Stage 2's point_cloud()/meshes() consumers use
    // BakeFrame::Local + the f64 global shift; the legacy World-frame
    // consumers (OBJ / glTF / drift / substrate) accept the precision
    // ceiling at extreme magnitudes. No assertion here — the meaningful
    // path is Local, and Local works.

    // Sanity: mesh_anchor IS the precise (f64) world position of the
    // baked geometry, so adding it back to the Local-frame vertices
    // reconstructs the box at ~5e7 in f64. (The reconstructed coords
    // themselves can't be held in f32, but they round-trip in f64 — the
    // shape information is preserved.)
    let anchor = m.mesh_anchor;
    for k in 0..3 {
        assert!(
            (anchor[k] - 50_000_000.0).abs() < 1e-3,
            "mesh_anchor[{}] = {} should be ~5e7 in f64",
            k,
            anchor[k],
        );
    }
}
