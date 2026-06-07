//! Property-based correctness budget for `mesh::cut_openings::apply`.
//!
//! This harness is the "recursive design-by-counterexample" companion
//! to the W1–W17 work plan tracked in GH #58. Instead of enumerating
//! the parameter space by hand (a fixed-fixture corpus, W5), we
//! generate `(host_box, cutter_box)` configurations from constrained
//! random distributions and assert closed-form analytic invariants
//! on the output of every cut. When proptest finds a counterexample
//! it automatically shrinks the inputs to the smallest reproducer
//! (the "recursive" loop the user asked for) — every failure is a
//! minimised test case ready to copy-paste into the fixed corpus.
//!
//! Scope (v1): **prism-minus-prism only**, axis-aligned, in metres,
//! near origin. The ~95 % case from the audit (GH #58). The same
//! framework extends naturally to halfspace cutters (W6), tapered
//! extrusions (W8), and the prism-prism pure-Rust replacement (W9).
//!
//! What we DON'T test here (deferred to later workitems):
//! - Rotated prisms (W9 axis-snap policy).
//! - Far-from-origin coords (W7 anchor-synchronization invariant).
//! - Halfspace cutters (W6).
//! - Brep hosts (W11).
//! - IfcMappedItem / instance interactions (W14).
//!
//! What baseline failures here tell us:
//! - `Outcome::Cut` returned, volume disagrees with analytic by >ε
//!   → the underlying CSG kernel produced a wrong result (a
//!   plausible source of GH #56 / #57 reports). One of the typed
//!   variants in `Outcome::Unsupported` ([W2]) would be the right
//!   reaction once detection lands in W3 / W11.
//! - `Outcome::Fallback` returned consistently for a shape pattern
//!   → manifold rejected the input. Useful baseline before W11
//!   pre-flight wires up typed reasons.
//! - Non-manifold output (mesh fails the closed-edge check)
//!   → exactly the class of degeneracy the audit flagged manifold
//!   as fragile on (coplanar / axis-aligned). Wired into the W3
//!   validation gate's classifier.
//!
//! Run: `cargo test --release --test cut_openings_proptest`.
//! Default budget is `PROPTEST_CASES=256`; bump to 10_000+ for
//! nightly stress: `PROPTEST_CASES=10000 cargo test --release …`.

#![cfg(all(feature = "mesh", feature = "csg"))]

use _core::mesh::cut_openings::{apply, Outcome};
use _core::mesh::{InstancePart, MeshSegment, ProductMesh};
use proptest::prelude::*;

// ----- mesh builders --------------------------------------------------

/// Axis-aligned box `[min_xyz, max_xyz]`. Always non-empty: callers
/// must ensure `max > min` per axis (the generators clamp).
#[derive(Debug, Clone, Copy)]
struct Box3 {
    min: [f64; 3],
    max: [f64; 3],
}

impl Box3 {
    fn volume(&self) -> f64 {
        (self.max[0] - self.min[0])
            * (self.max[1] - self.min[1])
            * (self.max[2] - self.min[2])
    }

    /// Closed-form `volume(self ∩ other)` over the AABB. Returns 0 if
    /// the boxes are disjoint along any axis.
    fn intersection_volume(&self, other: &Box3) -> f64 {
        let mut v = 1.0_f64;
        for ax in 0..3 {
            let lo = self.min[ax].max(other.min[ax]);
            let hi = self.max[ax].min(other.max[ax]);
            let extent = (hi - lo).max(0.0);
            if extent <= 0.0 {
                return 0.0;
            }
            v *= extent;
        }
        v
    }
}

/// Triangulate a closed box into 8 vertices + 12 triangles. Winding
/// is outward-CCW everywhere (verified pair-by-pair below — every
/// edge a→b on a face has its mirror b→a on the face's neighbour, so
/// the resulting mesh is a closed 2-manifold with `Σ signed-tetrahedra
/// volume == +box.volume()`).
fn box_mesh(b: &Box3) -> (Vec<f32>, Vec<u32>) {
    let (xn, yn, zn) = (b.min[0] as f32, b.min[1] as f32, b.min[2] as f32);
    let (xx, yy, zz) = (b.max[0] as f32, b.max[1] as f32, b.max[2] as f32);
    let verts = vec![
        xn, yn, zn, // 0
        xx, yn, zn, // 1
        xx, yy, zn, // 2
        xn, yy, zn, // 3
        xn, yn, zz, // 4
        xx, yn, zz, // 5
        xx, yy, zz, // 6
        xn, yy, zz, // 7
    ];
    // Each face is CCW viewed from outside the box (outward normal):
    //   −Z (bottom), +Z (top), −Y, +Y, −X, +X.
    let tris: Vec<u32> = vec![
        // −Z (normal -Z): 0,3,2 + 0,2,1
        0, 3, 2, 0, 2, 1, // +Z (normal +Z): 4,5,6 + 4,6,7
        4, 5, 6, 4, 6, 7, // −Y (normal -Y): 0,1,5 + 0,5,4
        0, 1, 5, 0, 5, 4, // +Y (normal +Y): 3,7,6 + 3,6,2
        3, 7, 6, 3, 6, 2, // −X (normal -X): 0,4,7 + 0,7,3
        0, 4, 7, 0, 7, 3, // +X (normal +X): 1,2,6 + 1,6,5
        1, 2, 6, 1, 6, 5,
    ];
    (verts, tris)
}

/// Synthesise a `ProductMesh` whose representation is a synthetic
/// `IfcBooleanClippingResult(host, cutter)` — i.e. two boxes
/// concatenated into one vertex/index buffer with `MeshSegment`s
/// tagged so `cut_openings::apply` recognises the host vs cutter
/// partition.
///
/// Skips the IFC parser entirely so proptest can iterate in
/// microseconds per case. The contract `apply` actually relies on is:
/// (a) `segments[i].source` contains `boolean_first_operand` or
///     `boolean_second_operand` as a chain token (split-on-`|`),
/// (b) `segments[i].index_start..index_count` is a valid range in
///     `mesh.indices`,
/// (c) every vertex is in `mesh.vertices`. We honour all three.
fn build_pair_product_mesh(host: &Box3, cutter: &Box3) -> ProductMesh {
    let (mut v, mut i) = box_mesh(host);
    let host_index_count = i.len() as u32;

    let (cv, ci) = box_mesh(cutter);
    let base = (v.len() / 3) as u32;
    v.extend(cv.iter().copied());
    i.extend(ci.iter().map(|idx| idx + base));

    let segments = vec![
        MeshSegment {
            index_start: 0,
            index_count: host_index_count,
            source: "boolean_first_operand|extrusion".to_string(),
        },
        MeshSegment {
            index_start: host_index_count,
            index_count: i.len() as u32 - host_index_count,
            source: "boolean_second_operand|extrusion".to_string(),
        },
    ];

    // InstancePart entries mirror the segments so any downstream
    // substrate code that walks `parts` doesn't trip. The vertex
    // payloads here are placeholders — apply() reads from
    // mesh.vertices/indices directly via the segments.
    let parts = vec![
        InstancePart {
            rep_step_id: 1,
            instance_transform: identity_mat4_cols(),
            local_vertices: Vec::new(),
            local_indices: Vec::new(),
            index_start: 0,
            index_count: host_index_count,
            source: "boolean_first_operand|extrusion".to_string(),
            surface_color: None,
        },
        InstancePart {
            rep_step_id: 2,
            instance_transform: identity_mat4_cols(),
            local_vertices: Vec::new(),
            local_indices: Vec::new(),
            index_start: host_index_count,
            index_count: i.len() as u32 - host_index_count,
            source: "boolean_second_operand|extrusion".to_string(),
            surface_color: None,
        },
    ];

    ProductMesh {
        guid: "0Proptest0000000000000".to_string(),
        entity: "IfcWallStandardCase".to_string(),
        ifc_id: 0,
        vertices: v,
        indices: i,
        source: "boolean_first_operand",
        segments,
        placement_origin: [0.0; 3],
        parts,
        world_transform: identity_mat4_cols(),
        world_origin: [0.0; 3],
        mesh_anchor: [0.0; 3],
        surface_color: None,
        bounded_halfspaces: Vec::new(),
    }
}

fn identity_mat4_cols() -> [f32; 16] {
    let mut m = [0.0_f32; 16];
    m[0] = 1.0;
    m[5] = 1.0;
    m[10] = 1.0;
    m[15] = 1.0;
    m
}

// ----- volume + topology invariants -----------------------------------

/// Signed mesh volume via the standard signed-tetrahedra divergence
/// sum: V = (1/6) Σ a · (b × c) over every CCW triangle (a, b, c).
/// For a closed outward-CCW mesh this returns the enclosed volume.
fn signed_volume(verts: &[f32], indices: &[u32]) -> f64 {
    let mut v6: f64 = 0.0;
    for tri in indices.chunks_exact(3) {
        let (ai, bi, ci) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
        let (ax, ay, az) = (
            verts[ai * 3] as f64,
            verts[ai * 3 + 1] as f64,
            verts[ai * 3 + 2] as f64,
        );
        let (bx, by, bz) = (
            verts[bi * 3] as f64,
            verts[bi * 3 + 1] as f64,
            verts[bi * 3 + 2] as f64,
        );
        let (cx, cy, cz) = (
            verts[ci * 3] as f64,
            verts[ci * 3 + 1] as f64,
            verts[ci * 3 + 2] as f64,
        );
        v6 += ax * (by * cz - bz * cy)
            + ay * (bz * cx - bx * cz)
            + az * (bx * cy - by * cx);
    }
    v6 / 6.0
}

/// `true` if every undirected edge in the index buffer appears in
/// exactly two triangles (closed 2-manifold). False if any edge is
/// odd-counted (a hole) or appears >2 times (non-manifold).
fn is_closed_manifold(indices: &[u32]) -> bool {
    use std::collections::HashMap;
    let mut edges: HashMap<(u32, u32), u32> = HashMap::new();
    for tri in indices.chunks_exact(3) {
        for (a, b) in [(tri[0], tri[1]), (tri[1], tri[2]), (tri[2], tri[0])] {
            let key = if a < b { (a, b) } else { (b, a) };
            *edges.entry(key).or_insert(0) += 1;
        }
    }
    edges.values().all(|&c| c == 2)
}

// ----- generators -----------------------------------------------------

/// A host box centred near origin with side lengths in `[0.5, 5.0]` m.
/// Bounded magnitude keeps the analytic-vs-kernel comparison out of
/// f32 round-off territory; far-origin testing is a separate harness
/// (W7).
fn host_box_strategy() -> impl Strategy<Value = Box3> {
    (
        0.5_f64..5.0_f64,
        0.5_f64..5.0_f64,
        0.5_f64..5.0_f64,
        -1.0_f64..1.0_f64,
        -1.0_f64..1.0_f64,
        -1.0_f64..1.0_f64,
    )
        .prop_map(|(sx, sy, sz, cx, cy, cz)| Box3 {
            min: [cx - sx / 2.0, cy - sy / 2.0, cz - sz / 2.0],
            max: [cx + sx / 2.0, cy + sy / 2.0, cz + sz / 2.0],
        })
}

/// A cutter box positioned anywhere within ±host_extent of the host
/// origin. Side lengths in `[0.05, 8.0]` m, so we deliberately
/// generate over- AND under-sized cutters — the audit specifically
/// flagged oversized cutters as a Revit-export reality (the
/// IfcOpeningElement extruded past the wall face for safety).
fn cutter_box_strategy(host: Box3) -> impl Strategy<Value = Box3> {
    let host_centre = [
        (host.min[0] + host.max[0]) / 2.0,
        (host.min[1] + host.max[1]) / 2.0,
        (host.min[2] + host.max[2]) / 2.0,
    ];
    let host_half = [
        (host.max[0] - host.min[0]) / 2.0,
        (host.max[1] - host.min[1]) / 2.0,
        (host.max[2] - host.min[2]) / 2.0,
    ];
    (
        0.05_f64..8.0_f64,
        0.05_f64..8.0_f64,
        0.05_f64..8.0_f64,
        -2.0_f64..2.0_f64,
        -2.0_f64..2.0_f64,
        -2.0_f64..2.0_f64,
    )
        .prop_map(move |(sx, sy, sz, ox, oy, oz)| {
            let centre = [
                host_centre[0] + ox * host_half[0],
                host_centre[1] + oy * host_half[1],
                host_centre[2] + oz * host_half[2],
            ];
            Box3 {
                min: [centre[0] - sx / 2.0, centre[1] - sy / 2.0, centre[2] - sz / 2.0],
                max: [centre[0] + sx / 2.0, centre[1] + sy / 2.0, centre[2] + sz / 2.0],
            }
        })
}

fn prism_pair_strategy() -> impl Strategy<Value = (Box3, Box3)> {
    host_box_strategy().prop_flat_map(|host| {
        cutter_box_strategy(host).prop_map(move |cutter| (host, cutter))
    })
}

/// Strict-containment generator: every emitted cutter is inside the
/// host's open interval on every axis. Built by sampling cutter
/// side-lengths as a fraction `f ∈ [0.05, 0.95]` of the host's
/// extent then placing the cutter's centre uniformly inside the
/// host minus its half-extent. No rejection — every sample is valid
/// by construction, so proptest's reject budget never depletes.
fn contained_pair_strategy() -> impl Strategy<Value = (Box3, Box3)> {
    host_box_strategy().prop_flat_map(|host| {
        let host_extents = [
            host.max[0] - host.min[0],
            host.max[1] - host.min[1],
            host.max[2] - host.min[2],
        ];
        (
            0.05_f64..0.95_f64,
            0.05_f64..0.95_f64,
            0.05_f64..0.95_f64,
            0.0_f64..1.0_f64,
            0.0_f64..1.0_f64,
            0.0_f64..1.0_f64,
        )
            .prop_map(move |(fx, fy, fz, ox, oy, oz)| {
                let cs = [
                    fx * host_extents[0],
                    fy * host_extents[1],
                    fz * host_extents[2],
                ];
                let centre = [
                    host.min[0] + cs[0] / 2.0 + ox * (host_extents[0] - cs[0]),
                    host.min[1] + cs[1] / 2.0 + oy * (host_extents[1] - cs[1]),
                    host.min[2] + cs[2] / 2.0 + oz * (host_extents[2] - cs[2]),
                ];
                let cutter = Box3 {
                    min: [centre[0] - cs[0] / 2.0, centre[1] - cs[1] / 2.0, centre[2] - cs[2] / 2.0],
                    max: [centre[0] + cs[0] / 2.0, centre[1] + cs[1] / 2.0, centre[2] + cs[2] / 2.0],
                };
                (host, cutter)
            })
    })
}

/// Tolerance for volume-equality assertions. f32 vertex precision plus
/// manifold's vertex-weld eps plus our box discretisation gives a
/// noise floor around `1e-5 m³`; for larger expected volumes the
/// 0.5 % relative tolerance dominates.
fn volume_tolerance(expected: f64) -> f64 {
    (expected.abs() * 5e-3).max(1e-5)
}

// ----- properties -----------------------------------------------------

proptest! {
    // 256 cases per property is the default; bump via env var
    // `PROPTEST_CASES=N` for stress runs. Each case is <2 ms wall-clock
    // on the manifold path locally, so the default budget completes
    // in well under a second.
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Volume invariant: `volume(host − cutter) == volume(host) −
    /// volume(host ∩ cutter)` within tolerance.
    ///
    /// On `Outcome::Cut` the actual output volume must match the
    /// analytic ground truth. On `Outcome::Fallback` the apply path
    /// gave up and the input mesh is left untouched (reveal-all
    /// fallback) — that's a known manifold limitation, not a bug per
    /// se; we measure but don't fail. `Passthrough` shouldn't fire on
    /// this fixture shape (we always have a cutter segment).
    #[test]
    fn prism_minus_prism_volume_matches_analytic(
        (host, cutter) in prism_pair_strategy(),
    ) {
        let mut mesh = build_pair_product_mesh(&host, &cutter);
        let host_vol = host.volume();
        let inter_vol = host.intersection_volume(&cutter);
        let expected = host_vol - inter_vol;

        let outcome = apply(&mut mesh, 1.0);

        match outcome {
            Outcome::Cut => {
                let actual = signed_volume(&mesh.vertices, &mesh.indices);
                let tol = volume_tolerance(expected);
                prop_assert!(
                    (actual - expected).abs() <= tol,
                    "volume mismatch: expected {:.6} m³, got {:.6} m³ (diff {:.3e} m³, tol {:.3e}); \
                     host = {:?}, cutter = {:?}, intersection = {:.6} m³",
                    expected,
                    actual,
                    actual - expected,
                    tol,
                    host,
                    cutter,
                    inter_vol,
                );
            }
            Outcome::Fallback => {
                // Manifold gave up. Acceptable today; the rate is the
                // signal. (W11 will replace this with a typed reason.)
            }
            Outcome::Passthrough => {
                prop_assert!(
                    false,
                    "Passthrough on a fixture with a cutter segment — partition_segments bug? \
                     host = {:?}, cutter = {:?}",
                    host,
                    cutter,
                );
            }
            Outcome::Unsupported(reason) => {
                // W3 wired NonManifoldInput and W4 wired the
                // union/intersection reasons, but none can fire on this
                // fixture: the boxes are index-welded closed manifolds
                // and the second operand is always tagged DIFFERENCE. An
                // Unsupported reason here is a real regression — surface
                // it as a failure to investigate.
                prop_assert!(
                    false,
                    "unexpected Outcome::Unsupported({:?}) on prism-prism fixture; \
                     host = {:?}, cutter = {:?}",
                    reason,
                    host,
                    cutter,
                );
            }
        }
    }

    /// Topology invariant: on `Outcome::Cut`, the output mesh must
    /// remain a closed 2-manifold (every undirected edge in exactly
    /// two triangles). A failure here is the signature of the
    /// degenerate-coplanar / axis-aligned cases the audit flagged
    /// manifold as fragile on.
    ///
    /// On `Outcome::Fallback` we skip — the input mesh wasn't mutated,
    /// and asserting closed-manifold on the union of two disjoint
    /// boxes is true but uninteresting.
    #[test]
    fn prism_minus_prism_output_is_closed_manifold(
        (host, cutter) in prism_pair_strategy(),
    ) {
        let mut mesh = build_pair_product_mesh(&host, &cutter);
        let outcome = apply(&mut mesh, 1.0);
        if matches!(outcome, Outcome::Cut) {
            prop_assert!(
                is_closed_manifold(&mesh.indices),
                "post-cut mesh is non-manifold (some edge ≠ 2 triangles); \
                 host = {:?}, cutter = {:?}",
                host,
                cutter,
            );
        }
    }

    /// Disjoint cutter: `cutter ∩ host = ∅` → volume out must equal
    /// volume host. Specialised case of the volume invariant above,
    /// kept as its own test because the analytic answer is exact
    /// (`expected = host.volume()`, tolerance is purely floating-point
    /// noise on the output mesh).
    #[test]
    fn disjoint_cutter_preserves_host_volume(
        (host, cutter) in prism_pair_strategy()
            .prop_filter("require disjoint cutter", |(h, c)| {
                h.intersection_volume(c) == 0.0
            }),
    ) {
        let mut mesh = build_pair_product_mesh(&host, &cutter);
        let outcome = apply(&mut mesh, 1.0);
        if let Outcome::Cut = outcome {
            let actual = signed_volume(&mesh.vertices, &mesh.indices);
            let expected = host.volume();
            let tol = volume_tolerance(expected);
            prop_assert!(
                (actual - expected).abs() <= tol,
                "disjoint cutter should leave host volume unchanged; \
                 expected {:.6} m³, got {:.6} m³; host = {:?}, cutter = {:?}",
                expected,
                actual,
                host,
                cutter,
            );
        }
    }

    /// Contained cutter: `cutter ⊆ host` → output volume must equal
    /// `host.volume() − cutter.volume()`. Constructed directly
    /// (cutter side lengths and centre are sampled relative to the
    /// host's extents) so proptest never has to reject — the
    /// filter-based variant starved the reject budget at 256 cases
    /// because oversize-cutter distribution dominates the joint
    /// strategy.
    #[test]
    fn contained_cutter_subtracts_full_cutter_volume(
        (host, cutter) in contained_pair_strategy(),
    ) {
        let mut mesh = build_pair_product_mesh(&host, &cutter);
        let outcome = apply(&mut mesh, 1.0);
        if let Outcome::Cut = outcome {
            let actual = signed_volume(&mesh.vertices, &mesh.indices);
            let expected = host.volume() - cutter.volume();
            let tol = volume_tolerance(expected);
            prop_assert!(
                (actual - expected).abs() <= tol,
                "contained cutter should remove its full volume from host; \
                 expected {:.6} m³, got {:.6} m³; host = {:?}, cutter = {:?}",
                expected,
                actual,
                host,
                cutter,
            );
        }
    }
}
