//! GH #188 review item 4 — the glTF instanced path must place
//! `parts[0]`'s geometry from `parts[0]`'s own anchor.
//!
//! `mesh_anchor` is pinned by the product's FIRST fragment, because that
//! is what defines the `Local` bake frame — it cannot be chosen by any
//! later criterion without moving every vertex. But a fragment that
//! carries vertices and no triangles never reaches the
//! `seg_index_count > 0` guard, so it pushes no `InstancePart`. Such a
//! product has `parts[0]` anchored somewhere other than `mesh_anchor`,
//! and the instanced glTF node — which positions `parts[0]`'s shared
//! mesh — would sit displaced from the baked path for the same geometry.
//! `InstancePart.anchor` is what closes that.
//!
//! The file below is exactly that shape: a Body of two items, the first
//! an `IfcGeometricCurveSet` of standalone points at the product origin
//! (vertices, zero triangles), the second an `IfcPolygonalFaceSet` whose
//! coordinates are baked 5 km east — so the faceset kernel rebases it and
//! `parts[0]` carries a `rep_origin` of (5000, 0, 0). One part, two
//! anchors, 5 km apart. That pairing is exactly what a Revit MEP export
//! with a stray annotation curve and transformed geometry produces.

use _core::mesh::{self, BakeFrame};

/// Points-then-solid, with the solid's coordinates baked 5 km east.
const IFC: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [CoordinationView]'),'2;1');
FILE_NAME('anchor.ifc','2026-09-21T00:00:00',(''),(''),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCPROJECT('0AnchorProject000000__',$,'Anchor',$,$,$,$,(#10),#5);
#2=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);
#3=IFCSIUNIT(*,.AREAUNIT.,$,.SQUARE_METRE.);
#4=IFCSIUNIT(*,.VOLUMEUNIT.,$,.CUBIC_METRE.);
#5=IFCUNITASSIGNMENT((#2,#3,#4));
#6=IFCCARTESIANPOINT((0.,0.,0.));
#7=IFCDIRECTION((0.,0.,1.));
#8=IFCDIRECTION((1.,0.,0.));
#9=IFCAXIS2PLACEMENT3D(#6,#7,#8);
#10=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.E-05,#9,$);
#11=IFCGEOMETRICREPRESENTATIONSUBCONTEXT('Body','Model',*,*,*,*,#10,$,.MODEL_VIEW.,$);
#12=IFCSITE('0AnchorSite000000000__',$,'Site',$,$,#20,$,$,.ELEMENT.,$,$,$,$,$);
#13=IFCBUILDING('0AnchorBuilding00000__',$,'Building',$,$,#21,$,$,.ELEMENT.,$,$,$);
#14=IFCBUILDINGSTOREY('0AnchorStorey0000000__',$,'L0',$,$,#22,$,$,.ELEMENT.,0.);
#15=IFCRELAGGREGATES('0AnchorAggProjSite00__',$,$,$,#1,(#12));
#16=IFCRELAGGREGATES('0AnchorAggSiteBldg00__',$,$,$,#12,(#13));
#17=IFCRELAGGREGATES('0AnchorAggBldgStry00__',$,$,$,#13,(#14));
#20=IFCLOCALPLACEMENT($,#9);
#21=IFCLOCALPLACEMENT(#20,#9);
#22=IFCLOCALPLACEMENT(#21,#9);
/* the triangle-less first fragment: two standalone points at the origin */
#30=IFCCARTESIANPOINT((0.,0.,0.));
#31=IFCCARTESIANPOINT((1.,0.,0.));
#32=IFCGEOMETRICCURVESET((#30,#31));
/* the solid that carries the triangles \u2014 a tetrahedron whose coordinates
   are baked 5 km east, so the faceset kernel rebases it by its bbox-min
   and parts[0] lands with rep_origin = (5000, 0, 0) */
#40=IFCCARTESIANPOINTLIST3D(((5000.,0.,0.),(5001.,0.,0.),(5000.,1.,0.),(5000.,0.,1.)));
#41=IFCINDEXEDPOLYGONALFACE((1,2,3));
#42=IFCINDEXEDPOLYGONALFACE((1,2,4));
#43=IFCINDEXEDPOLYGONALFACE((1,3,4));
#46=IFCINDEXEDPOLYGONALFACE((2,3,4));
#47=IFCPOLYGONALFACESET(#40,.T.,(#41,#42,#43,#46),$);
#44=IFCSHAPEREPRESENTATION(#11,'Body','Tessellation',(#32,#47));
#45=IFCPRODUCTDEFINITIONSHAPE($,$,(#44));
#50=IFCBUILDINGELEMENTPROXY('0AnchorProduct000000__',$,'Points then solid',$,$,#22,#45,$,$);
#51=IFCRELCONTAINEDINSPATIALSTRUCTURE('0AnchorContained0000__',$,$,$,(#50),#14);
ENDSEC;
END-ISO-10303-21;
"#;

/// World position of `parts[i]`'s local origin, from the transform the
/// substrate and the glTF instancer both compose:
/// `world_transform * parts[i].instance_transform * [0,0,0,1]`.
fn part_anchor(m: &mesh::ProductMesh, i: usize) -> [f64; 3] {
    let world = glam::Mat4::from_cols_array(&m.world_transform);
    let inst = glam::Mat4::from_cols_array(&m.parts[i].instance_transform);
    let p = (world * inst) * glam::Vec4::new(0.0, 0.0, 0.0, 1.0);
    [p.x as f64, p.y as f64, p.z as f64]
}

#[test]
fn part_anchor_places_the_part_even_when_mesh_anchor_does_not() {
    let (meshes, _stats) = mesh::mesh_ifc_framed(IFC.as_bytes(), BakeFrame::Local);
    let m = meshes
        .iter()
        .find(|m| m.guid == "0AnchorProduct000000__")
        .expect("the proxy meshes");

    // The points contributed vertices but no triangles, so there is one
    // part — the solid. That is the premise of the whole test; if the
    // mesher ever starts emitting a part for the curve set, this test is
    // measuring something else and should be rewritten, not relaxed.
    assert_eq!(
        m.parts.len(),
        1,
        "expected one InstancePart (the solid); got {}",
        m.parts.len()
    );
    assert!(m.parts[0].source.contains("faceset"), "{}", m.parts[0].source);

    // `parts[0].anchor` is the f64 twin of the transform every consumer
    // composes for that part — this is the invariant the glTF instanced
    // translation relies on.
    let want = part_anchor(m, 0);
    for k in 0..3 {
        assert!(
            (m.parts[0].anchor[k] - want[k]).abs() < 1.0e-6,
            "axis {k}: parts[0].anchor {:?} vs world_transform * \
             instance_transform {want:?}",
            m.parts[0].anchor
        );
    }

    // …and the hazard is real on this file: `mesh_anchor` is the mapped
    // curve set's 5 m offset, so a consumer using it to place `parts[0]`
    // would be 5 m out. If this ever stops being true the fixture has
    // lost its teeth and the test above stops proving anything.
    assert!(
        m.mesh_anchor[0].abs() < 1.0e-6,
        "expected mesh_anchor pinned by the points at the origin, got {:?}",
        m.mesh_anchor
    );
    assert!(
        (m.parts[0].anchor[0] - 5000.0).abs() < 1.0e-6,
        "expected parts[0] anchored at the faceset rebase, got {:?}",
        m.parts[0].anchor
    );
}

/// The same product in the World frame keeps its vertices where they
/// were — the anchor change must not move geometry, only the label.
#[test]
fn the_anchor_fix_does_not_move_vertices() {
    let (local, _) = mesh::mesh_ifc_framed(IFC.as_bytes(), BakeFrame::Local);
    let (world, _) = mesh::mesh_ifc_framed(IFC.as_bytes(), BakeFrame::World);
    let pick = |v: &[_core::mesh::ProductMesh]| -> usize {
        v.iter()
            .position(|m| m.guid == "0AnchorProduct000000__")
            .expect("product")
    };
    let l = &local[pick(&local)];
    let w = &world[pick(&world)];
    assert_eq!(l.vertices.len(), w.vertices.len());
    // Near-origin file: Local + anchor == World, exactly.
    for (i, (lv, wv)) in l.vertices.iter().zip(w.vertices.iter()).enumerate() {
        let shifted = *lv as f64 + l.mesh_anchor[i % 3];
        assert!(
            (shifted - *wv as f64).abs() < 1.0e-5,
            "vertex {i}: local {lv} + anchor != world {wv}"
        );
    }
}


