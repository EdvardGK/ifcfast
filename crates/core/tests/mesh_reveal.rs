//! Reveal-all dispatch tests for the mesh pipeline.
//!
//! These verify the dispatcher's stance: every representation type the
//! file contains either produces real geometry (with a per-segment
//! source tag) or surfaces as an explicit `unhandled:IFCXXX` bucket in
//! stats. Nothing silently disappears.

#![cfg(feature = "mesh")]

use _core::mesh::{mesh_ifc, MeshFragment};

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

/// A second wall using `IfcRevolvedAreaSolid` — we don't handle this
/// representation type yet, so it MUST appear as an `unhandled:` bucket
/// in stats rather than disappearing.
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
#33=IFCAXIS1PLACEMENT(#7,#32);
#34=IFCREVOLVEDAREASOLID(#30,#6,#33,1.5708);
#60=IFCSHAPEREPRESENTATION(#5,'Body','SweptSolid',(#34));
#61=IFCPRODUCTDEFINITIONSHAPE($,$,(#60));
#70=IFCBUILDINGELEMENTPROXY('7Prox00000000000000001',$,'Spinner',$,$,#16,#61,'tag',$);
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
    let unhandled_key = "unhandled:IFCREVOLVEDAREASOLID";
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
    ] {
        assert!(
            documented.contains(&required),
            "MeshFragment::source_tags() missing {:?}", required
        );
    }
}
