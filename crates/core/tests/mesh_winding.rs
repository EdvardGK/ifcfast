//! Profile-winding invariance for swept solids (GH #62).
//!
//! IFC does not mandate a winding direction for profile curves, and Revit
//! authors both `IfcPolyline` outers and voids clockwise. `profile::extract`
//! must normalise every authored combination to the `Polygon2D` invariant
//! (outer CCW, holes CW) so the extruded mesh is a closed manifold whose
//! divergence-theorem volume equals the true swept volume. Before the fix a
//! CW-authored void had its wall normals inverted: the volume *added* the
//! hole instead of subtracting it (ring below: 4.33 instead of 3.0) and
//! edge-pairing flagged the mesh open — the exact signature of the G55_ARK
//! 208-window `prism_fallback` +482% residue (GH #62 / #121).

#![cfg(feature = "mesh")]

use std::collections::HashMap;

use _core::mesh::mesh_ifc;

/// Synthetic proxy whose Body is a 2×2 outer / 1×1 void ring extruded to
/// depth 1 — true volume 3.0. `{OUTER}` / `{HOLE}` are point-ref lists so
/// each test picks its own authored winding.
const RING_IFC_TEMPLATE: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION((''),'2;1');
FILE_NAME('winding.ifc','2026-07-03T00:00:00',('test'),('skiplum'),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC2X3'));
ENDSEC;
DATA;
#1=IFCPROJECT('0Test000000000000000001',$,'p',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#3));
#3=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);
#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);
#6=IFCAXIS2PLACEMENT3D(#7,$,$);
#7=IFCCARTESIANPOINT((0.,0.,0.));
#10=IFCSITE('1Site000000000000000001',$,'s',$,$,#15,$,$,.ELEMENT.,$,$,$,$,$);
#15=IFCLOCALPLACEMENT($,#6);
#20=IFCRELAGGREGATES('4Agg000000000000000001',$,$,$,#1,(#10));
#30=IFCARBITRARYPROFILEDEFWITHVOIDS(.AREA.,$,#31,(#32));
#31=IFCPOLYLINE({OUTER});
#32=IFCPOLYLINE({HOLE});
#40=IFCCARTESIANPOINT((0.,0.));
#41=IFCCARTESIANPOINT((2.,0.));
#42=IFCCARTESIANPOINT((2.,2.));
#43=IFCCARTESIANPOINT((0.,2.));
#50=IFCCARTESIANPOINT((0.5,0.5));
#51=IFCCARTESIANPOINT((1.5,0.5));
#52=IFCCARTESIANPOINT((1.5,1.5));
#53=IFCCARTESIANPOINT((0.5,1.5));
#60=IFCDIRECTION((0.,0.,1.));
#61=IFCEXTRUDEDAREASOLID(#30,#6,#60,1.);
#62=IFCSHAPEREPRESENTATION(#5,'Body','SweptSolid',(#61));
#63=IFCPRODUCTDEFINITIONSHAPE($,$,(#62));
#70=IFCBUILDINGELEMENTPROXY('7Ring00000000000000001',$,'ring',$,$,#15,#63,$);
#71=IFCRELCONTAINEDINSPATIALSTRUCTURE('8Rel00000000000000001',$,$,$,(#70),#10);
ENDSEC;
END-ISO-10303-21;
"#;

const OUTER_CCW: &str = "(#40,#41,#42,#43,#40)";
const OUTER_CW: &str = "(#40,#43,#42,#41,#40)";
const HOLE_CCW: &str = "(#50,#51,#52,#53,#50)";
const HOLE_CW: &str = "(#50,#53,#52,#51,#50)";

/// Signed-tetra divergence volume over the whole mesh, f64 accumulation.
fn signed_volume(vertices: &[f32], faces: &[u32]) -> f64 {
    let mut acc = 0.0_f64;
    for tri in faces.chunks_exact(3) {
        let p = |i: u32| {
            let b = i as usize * 3;
            (
                vertices[b] as f64,
                vertices[b + 1] as f64,
                vertices[b + 2] as f64,
            )
        };
        let (ax, ay, az) = p(tri[0]);
        let (bx, by, bz) = p(tri[1]);
        let (cx, cy, cz) = p(tri[2]);
        acc += ax * (by * cz - bz * cy) - ay * (bx * cz - bz * cx) + az * (bx * cy - by * cx);
    }
    acc / 6.0
}

/// Closed manifold iff every undirected edge is used exactly twice, once
/// in each direction (mirrors the qto edge-pairing classifier).
fn is_closed(faces: &[u32]) -> bool {
    let mut edges: HashMap<(u32, u32), i32> = HashMap::new();
    for tri in faces.chunks_exact(3) {
        for (a, b) in [(tri[0], tri[1]), (tri[1], tri[2]), (tri[2], tri[0])] {
            let key = (a.min(b), a.max(b));
            let dir = if a < b { 1 } else { -1 };
            *edges.entry(key).or_insert(0) += dir;
        }
    }
    !edges.is_empty() && edges.values().all(|&v| v == 0)
}

fn ring_case(outer: &str, hole: &str, label: &str) {
    let src = RING_IFC_TEMPLATE
        .replace("{OUTER}", outer)
        .replace("{HOLE}", hole);
    let (meshes, _stats) = mesh_ifc(src.as_bytes());
    assert_eq!(meshes.len(), 1, "{label}: expected exactly one product mesh");
    let m = &meshes[0];
    let vol = signed_volume(&m.vertices, &m.indices).abs();
    assert!(
        (vol - 3.0).abs() < 1e-4,
        "{label}: ring volume {vol} != 3.0 — hole wall winding inverted"
    );
    assert!(
        is_closed(&m.indices),
        "{label}: extruded ring is not a closed manifold"
    );
}

#[test]
fn ring_outer_ccw_hole_ccw() {
    ring_case(OUTER_CCW, HOLE_CCW, "outer CCW / hole CCW");
}

#[test]
fn ring_outer_ccw_hole_cw() {
    // The Revit-authored form — all 208 G55_ARK windows (GH #62).
    ring_case(OUTER_CCW, HOLE_CW, "outer CCW / hole CW");
}

#[test]
fn ring_outer_cw_hole_ccw() {
    ring_case(OUTER_CW, HOLE_CCW, "outer CW / hole CCW");
}

#[test]
fn ring_outer_cw_hole_cw() {
    ring_case(OUTER_CW, HOLE_CW, "outer CW / hole CW");
}
