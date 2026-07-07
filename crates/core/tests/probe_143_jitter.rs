//! Diagnostic probe for GH #143: shows where a band-capped (seeded)
//! distance traversal numerically diverges from `query::distance` on
//! a real mesh pair. Findings it documents (measured on G55 TMK13):
//! parry's exact query is orientation-asymmetric at the last ulps
//! (`distance(a,b) != distance(b,a)`), and seeding the best-first
//! traversal changes which of two near-tied leaf pairs wins — which
//! is why the engine treats `min_distance_within` as reject-only and
//! re-runs the exact query for every emitted distance.
//! Ignored by default; run with:
//!
//! ```sh
//! PROBE_BUNDLE=... PROBE_GUID_A=... PROBE_GUID_B=... PROBE_CAP=0.1 \
//!   cargo test -p ifcfast-core --test probe_143_jitter -- --ignored --nocapture
//! ```
#![cfg(feature = "clash")]

use std::path::Path;

use _core::clash::source::{read_instances, read_representations};
use _core::geom;
use parry3d::math::Isometry;
use parry3d::query::details::CompositeShapeAgainstAnyDistanceVisitor;
use parry3d::query::DefaultQueryDispatcher;
use parry3d::shape::TriMesh;

fn bake_world(local: &[f32], m: &[f32; 16]) -> Vec<f32> {
    let mut out = Vec::with_capacity(local.len());
    for v in local.chunks_exact(3) {
        let (x, y, z) = (v[0], v[1], v[2]);
        out.push(m[0] * x + m[4] * y + m[8] * z + m[12]);
        out.push(m[1] * x + m[5] * y + m[9] * z + m[13]);
        out.push(m[2] * x + m[6] * y + m[10] * z + m[14]);
    }
    out
}

fn seeded(a: &TriMesh, b: &TriMesh, init: f32) -> Option<f32> {
    let pos = Isometry::identity();
    let mut visitor =
        CompositeShapeAgainstAnyDistanceVisitor::new(&DefaultQueryDispatcher, &pos, a, b);
    a.qbvh()
        .traverse_best_first_node(&mut visitor, 0, init)
        .map(|(_, (_, d))| d)
}

#[test]
#[ignore]
fn probe_real_pair() {
    let bundle = std::env::var("PROBE_BUNDLE").unwrap();
    let guid_a = std::env::var("PROBE_GUID_A").unwrap();
    let guid_b = std::env::var("PROBE_GUID_B").unwrap();
    let cap: f32 = std::env::var("PROBE_CAP").unwrap().parse().unwrap();

    let dir = Path::new(&bundle);
    let instances = read_instances(&dir.join("instances.parquet")).unwrap();
    let reps = read_representations(&dir.join("representations.parquet")).unwrap();

    let mut meshes = Vec::new();
    for guid in [&guid_a, &guid_b] {
        let inst = instances.iter().find(|i| &i.guid == guid).unwrap();
        let rep = &reps[&inst.rep_id.unwrap()];
        let world = if rep.source_kind == "composite" {
            rep.vertices.clone()
        } else {
            bake_world(&rep.vertices, &inst.transform)
        };
        let mesh = geom::build_trimesh(&world, &rep.indices).unwrap();
        println!("{guid}: {} tris", rep.indices.len() / 3);
        meshes.push(mesh);
    }
    let (a, b) = (&meshes[0], &meshes[1]);

    let exact_ab = geom::min_distance(a, b).unwrap();
    let exact_ba = geom::min_distance(b, a).unwrap();
    let vis_max = seeded(a, b, f32::MAX);
    let vis_big = seeded(a, b, 1000.0);
    let vis_cap = seeded(a, b, cap.next_up());
    let within = geom::min_distance_within(a, b, cap);

    println!("query::distance(a,b)        = {exact_ab:.9}");
    println!("query::distance(b,a)        = {exact_ba:.9}");
    println!("visitor init=MAX            = {vis_max:?}");
    println!("visitor init=1000           = {vis_big:?}");
    println!("visitor init=cap.next_up()  = {vis_cap:?}");
    println!("min_distance_within(cap)    = {within:?}");
}
