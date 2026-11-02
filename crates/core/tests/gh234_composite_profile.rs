//! `IfcCompositeProfileDef` swept solids (GH #234).
//!
//! Before the fix `profile::extract` had no composite branch: the
//! extrusion failed, and 19 bodied Archicad walls in HI90_ARK_MMI700 were
//! silently unmeshed. Fixture `tests/fixtures/gh234_composite_profile.ifc`
//! (IFC4, mm):
//!
//! * `…D1a` — triangle (0.5 m²) + positioned/rotated rectangle (0.4 m²),
//!   disjoint, depth 3 m → 2.7 m³, two closed prisms.
//! * `…D1b` — two overlapping squares (member sum 2.0 m², union 1.5 m²):
//!   meshes at 6.0 m³ and carries the `overlapping` profile defect.
//! * `…D1c` — one member is an unsupported `IfcCenterLineProfileDef`: the
//!   whole item fails loudly as `unhandled:IFCEXTRUDEDAREASOLID`.

#![cfg(feature = "mesh")]

use std::collections::HashMap;
use std::path::Path;

use _core::mesh::mesh_ifc;
use _core::mesh::profile_check::ProfileDefect;

fn fixture() -> Vec<u8> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gh234_composite_profile.ifc");
    std::fs::read(p).expect("fixture")
}

fn signed_volume(vertices: &[f32], faces: &[u32]) -> f64 {
    let mut acc = 0.0_f64;
    for tri in faces.as_chunks::<3>().0 {
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

fn is_closed(faces: &[u32]) -> bool {
    let mut edges: HashMap<(u32, u32), i32> = HashMap::new();
    for tri in faces.as_chunks::<3>().0 {
        for (a, b) in [(tri[0], tri[1]), (tri[1], tri[2]), (tri[2], tri[0])] {
            let key = (a.min(b), a.max(b));
            *edges.entry(key).or_insert(0) += if a < b { 1 } else { -1 };
        }
    }
    !edges.is_empty() && edges.values().all(|&v| v == 0)
}

#[test]
fn composite_profile_meshes_with_analytic_volume() {
    let buf = fixture();
    let (meshes, stats) = mesh_ifc(&buf);
    let by_guid: HashMap<&str, _> = meshes.iter().map(|m| (m.guid.as_str(), m)).collect();

    let a = by_guid["3Rk2cW9Nt1HfJ4sBpLgD1a"];
    let vol_a = signed_volume(&a.vertices, &a.indices) / 1e9; // mm³ → m³
    assert!((vol_a - 2.7).abs() < 1e-6, "disjoint composite volume {vol_a} != 2.7");
    assert!(is_closed(&a.indices), "disjoint composite must be two closed prisms");
    assert_eq!(a.profile_invalid, None);

    let b = by_guid["3Rk2cW9Nt1HfJ4sBpLgD1b"];
    let vol_b = signed_volume(&b.vertices, &b.indices) / 1e9;
    assert!((vol_b - 6.0).abs() < 1e-6, "overlapping composite mesh volume {vol_b} != 6.0");
    assert_eq!(b.profile_invalid, Some(ProfileDefect::Overlapping));

    assert!(
        !by_guid.contains_key("3Rk2cW9Nt1HfJ4sBpLgD1c"),
        "unsupported member must not produce a partial mesh"
    );
    assert_eq!(stats.by_source.get("unhandled:IFCEXTRUDEDAREASOLID"), Some(&1));
    assert_eq!(stats.products_profile_invalid, 1);
}
