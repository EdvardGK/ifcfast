//! Clash orchestration: substrate → broad-phase → narrow-phase →
//! `ClashPair` records.
//!
//! Single entry point: [`clash`]. Reads `instances.parquet` and
//! `representations.parquet` from the given bundle directory, runs the
//! broad / narrow pipeline, and returns a [`ClashReport`].
//!
//! Writing `clashes.parquet` is the caller's responsibility — see
//! [`super::write_clashes_parquet`].

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use crate::geom::{self, AabbF32};

use super::source::{
    self, InstanceRow, RepresentationRow, SubstrateReadError,
};

/// Per-pair clash classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClashKind {
    /// Solids actually intersect (zero minimum distance).
    Hard,
    /// Solids don't intersect, but the minimum distance between them
    /// is `<= options.tolerance_m`. Only emitted when `tolerance_m > 0`.
    Clearance,
}

impl ClashKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Hard => "hard",
            Self::Clearance => "clearance",
        }
    }
}

/// One clash fact between two instances. Identity is by `ifc_id` and
/// the substrate `guid` — agents join this back to `instances.parquet`
/// for storey / type / pset enrichment.
#[derive(Debug, Clone)]
pub struct ClashPair {
    pub ifc_id_a: u64,
    pub ifc_id_b: u64,
    pub guid_a: String,
    pub guid_b: String,
    pub class_a: String,
    pub class_b: String,
    pub kind: ClashKind,
    pub min_distance_m: f32,
}

/// Engine configuration.
#[derive(Debug, Clone)]
pub struct ClashOptions {
    /// Soft-clash band, in metres. `0.0` means "hard clashes only";
    /// positive values also emit `Clearance` pairs whose meshes are
    /// within that distance of each other.
    pub tolerance_m: f32,
    /// If set, only emit pairs where at least one side matches one of
    /// these classes (after the substrate's `class` normalisation —
    /// e.g. `"Pipe"`, not `"IfcPipe"`). Empty = no class filter.
    pub include_classes: Vec<String>,
    /// Classes that should never clash against themselves. Useful for
    /// suppressing "wall-vs-wall" noise where the user only cares about
    /// cross-discipline clashes. Empty = no self-class filter.
    pub exclude_self_class: Vec<String>,
}

impl Default for ClashOptions {
    fn default() -> Self {
        Self {
            tolerance_m: 0.0,
            include_classes: Vec::new(),
            exclude_self_class: Vec::new(),
        }
    }
}

/// Aggregate output of a single clash run.
#[derive(Debug, Clone)]
pub struct ClashReport {
    pub pairs: Vec<ClashPair>,
    /// Instances skipped because they were geometryless (`rep_id` =
    /// NULL on the substrate). Reported so agents can audit
    /// completeness — these aren't silent drops.
    pub geometryless_skipped: usize,
    /// Candidate pairs from broad-phase that were dropped because at
    /// least one side's mesh wouldn't build (e.g. degenerate
    /// representation). Surfaced rather than swallowed.
    pub narrow_phase_residuals: usize,
}

#[derive(Debug)]
pub enum ClashError {
    Read(SubstrateReadError),
    MeshBuild(geom::MeshBuildError),
    NarrowPhase(geom::NarrowPhaseError),
}

impl std::fmt::Display for ClashError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read(e) => write!(f, "substrate read: {e}"),
            Self::MeshBuild(e) => write!(f, "mesh build: {e}"),
            Self::NarrowPhase(e) => write!(f, "narrow phase: {e}"),
        }
    }
}

impl std::error::Error for ClashError {}

impl From<SubstrateReadError> for ClashError {
    fn from(e: SubstrateReadError) -> Self {
        Self::Read(e)
    }
}
impl From<geom::NarrowPhaseError> for ClashError {
    fn from(e: geom::NarrowPhaseError) -> Self {
        Self::NarrowPhase(e)
    }
}

/// Run the clash pipeline over the substrate at `bundle_dir`. Looks
/// for `instances.parquet` and `representations.parquet` inside that
/// directory.
pub fn clash(bundle_dir: &Path, options: &ClashOptions) -> Result<ClashReport, ClashError> {
    let instances = source::read_instances(&bundle_dir.join("instances.parquet"))?;
    let reps = source::read_representations(&bundle_dir.join("representations.parquet"))?;

    run(&instances, &reps, options)
}

/// Pure-Rust entry that takes already-decoded substrate rows. Useful
/// for tests and for callers who want to short-circuit the parquet
/// read (e.g. a future in-memory bundle).
pub fn run(
    instances: &[InstanceRow],
    reps: &HashMap<u64, RepresentationRow>,
    options: &ClashOptions,
) -> Result<ClashReport, ClashError> {
    // Build the broad-phase input. Skip geometryless products — they
    // have no rep to narrow-phase against. The broad-phase id is the
    // index into `instances`, so the narrow-phase loop can re-lookup
    // semantics by index.
    let mut boxes: Vec<AabbF32> = Vec::with_capacity(instances.len());
    let mut geometryless_skipped = 0usize;
    for (idx, inst) in instances.iter().enumerate() {
        if inst.rep_id.is_none() {
            geometryless_skipped += 1;
            continue;
        }
        boxes.push(AabbF32 {
            id: idx as u32,
            min: inst.bbox_min,
            max: inst.bbox_max,
        });
    }

    let candidate_pairs = geom::pairs_overlapping(&boxes, options.tolerance_m);

    // Materialise world-coord TriMeshes lazily — once per instance that
    // shows up in at least one candidate pair. Many models have most
    // instances disjoint, so building a mesh for every one upfront
    // would be wasted. We bake-world per instance rather than sharing
    // per-rep BVHs across instances — see module docs for the rationale.
    // Arc keeps the cache borrow checker happy when we need both a and
    // b out simultaneously; cloning the Arc is constant-time.
    let mut mesh_cache: HashMap<u32, Option<Arc<parry3d::shape::TriMesh>>> = HashMap::new();
    let mut narrow_phase_residuals = 0usize;
    let mut pairs: Vec<ClashPair> = Vec::new();

    for (id_a, id_b) in candidate_pairs {
        if !class_filter_ok(&instances[id_a as usize], &instances[id_b as usize], options) {
            continue;
        }

        let mesh_a = ensure_mesh(&mut mesh_cache, instances, reps, id_a);
        let mesh_b = ensure_mesh(&mut mesh_cache, instances, reps, id_b);

        let (mesh_a, mesh_b) = match (mesh_a, mesh_b) {
            (Some(a), Some(b)) => (a, b),
            _ => {
                narrow_phase_residuals += 1;
                continue;
            }
        };

        // Distance subsumes intersection: 0.0 == hard clash, positive
        // == clearance. One parry call, both facts.
        let distance = geom::min_distance(&mesh_a, &mesh_b)?;

        let kind = if distance == 0.0 {
            ClashKind::Hard
        } else if distance <= options.tolerance_m {
            ClashKind::Clearance
        } else {
            // Broad phase admitted them via expanded AABBs but the
            // actual mesh distance is outside the tolerance band.
            continue;
        };

        let a = &instances[id_a as usize];
        let b = &instances[id_b as usize];
        pairs.push(ClashPair {
            ifc_id_a: a.ifc_id,
            ifc_id_b: b.ifc_id,
            guid_a: a.guid.clone(),
            guid_b: b.guid.clone(),
            class_a: a.class.clone(),
            class_b: b.class.clone(),
            kind,
            min_distance_m: distance,
        });
    }

    Ok(ClashReport {
        pairs,
        geometryless_skipped,
        narrow_phase_residuals,
    })
}

fn class_filter_ok(a: &InstanceRow, b: &InstanceRow, options: &ClashOptions) -> bool {
    if !options.include_classes.is_empty() {
        let hit = options.include_classes.iter().any(|c| c == &a.class || c == &b.class);
        if !hit {
            return false;
        }
    }
    if a.class == b.class && options.exclude_self_class.iter().any(|c| c == &a.class) {
        return false;
    }
    true
}

/// Build (and cache) the world-coord `TriMesh` for `instances[idx]`.
/// Returns `None` if the rep is missing from the representations map
/// or if the mesh failed to build — caller treats both as residuals.
fn ensure_mesh(
    cache: &mut HashMap<u32, Option<Arc<parry3d::shape::TriMesh>>>,
    instances: &[InstanceRow],
    reps: &HashMap<u64, RepresentationRow>,
    idx: u32,
) -> Option<Arc<parry3d::shape::TriMesh>> {
    if !cache.contains_key(&idx) {
        let built = build_world_trimesh(&instances[idx as usize], reps).map(Arc::new);
        cache.insert(idx, built);
    }
    cache.get(&idx).and_then(|m| m.clone())
}

fn build_world_trimesh(
    inst: &InstanceRow,
    reps: &HashMap<u64, RepresentationRow>,
) -> Option<parry3d::shape::TriMesh> {
    let rep_id = inst.rep_id?;
    let rep = reps.get(&rep_id)?;

    let world_vertices: Vec<f32> = if rep.source_kind == "composite" {
        // Composite reps already carry world-baked vertices and the
        // instance transform is identity. Pass through unchanged.
        rep.vertices.clone()
    } else {
        // shared_or_direct: rep vertices are local-frame. Apply the
        // instance's column-major 4×4 transform per-vertex to bake
        // world coordinates. Allocates a fresh vertex buffer — fine
        // for v1; a parry isometry-aware narrow-phase API can lift
        // this allocation later.
        bake_world(&rep.vertices, &inst.transform)
    };

    geom::build_trimesh(&world_vertices, &rep.indices).ok()
}

/// Apply a column-major 4×4 affine matrix to a `[x, y, z, x, y, z, …]`
/// vertex buffer. Treats the matrix as a true 4×4 (includes any scale
/// the placement chain carried) — IFC placements are normally pure
/// rotation+translation but we don't assume that here.
fn bake_world(local: &[f32], m: &[f32; 16]) -> Vec<f32> {
    let mut out = Vec::with_capacity(local.len());
    for v in local.chunks_exact(3) {
        let x = v[0];
        let y = v[1];
        let z = v[2];
        // Column-major indexing: m[col * 4 + row]
        let wx = m[0] * x + m[4] * y + m[8] * z + m[12];
        let wy = m[1] * x + m[5] * y + m[9] * z + m[13];
        let wz = m[2] * x + m[6] * y + m[10] * z + m[14];
        out.push(wx);
        out.push(wy);
        out.push(wz);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit_cube_local() -> (Vec<f32>, Vec<u32>) {
        let v: Vec<f32> = vec![
            0.0, 0.0, 0.0,
            1.0, 0.0, 0.0,
            1.0, 1.0, 0.0,
            0.0, 1.0, 0.0,
            0.0, 0.0, 1.0,
            1.0, 0.0, 1.0,
            1.0, 1.0, 1.0,
            0.0, 1.0, 1.0,
        ];
        let i: Vec<u32> = vec![
            0, 2, 1, 0, 3, 2,
            4, 5, 6, 4, 6, 7,
            0, 1, 5, 0, 5, 4,
            2, 3, 7, 2, 7, 6,
            1, 2, 6, 1, 6, 5,
            0, 4, 7, 0, 7, 3,
        ];
        (v, i)
    }

    fn identity() -> [f32; 16] {
        let mut m = [0.0f32; 16];
        m[0] = 1.0;
        m[5] = 1.0;
        m[10] = 1.0;
        m[15] = 1.0;
        m
    }

    fn translate(x: f32, y: f32, z: f32) -> [f32; 16] {
        let mut m = identity();
        m[12] = x;
        m[13] = y;
        m[14] = z;
        m
    }

    fn make_instance(idx: u64, rep_id: u64, transform: [f32; 16], bbox_origin: [f32; 3]) -> InstanceRow {
        InstanceRow {
            ifc_id: idx,
            guid: format!("g{idx}"),
            class: "Wall".to_string(),
            rep_id: Some(rep_id),
            transform,
            bbox_min: bbox_origin,
            bbox_max: [bbox_origin[0] + 1.0, bbox_origin[1] + 1.0, bbox_origin[2] + 1.0],
        }
    }

    #[test]
    fn shared_rep_with_two_instances_overlapping_clashes_hard() {
        let (v, i) = unit_cube_local();
        let rep = RepresentationRow {
            rep_id: 100,
            source_kind: "shared_or_direct".to_string(),
            vertices: v,
            indices: i,
        };
        let mut reps = HashMap::new();
        reps.insert(100u64, rep);

        // Two instances of the same rep, offset by 0.5 along X — they
        // overlap.
        let instances = vec![
            make_instance(1, 100, identity(), [0.0, 0.0, 0.0]),
            make_instance(2, 100, translate(0.5, 0.0, 0.0), [0.5, 0.0, 0.0]),
        ];
        let report = run(&instances, &reps, &ClashOptions::default()).unwrap();
        assert_eq!(report.pairs.len(), 1);
        assert_eq!(report.pairs[0].kind, ClashKind::Hard);
        assert_eq!(report.pairs[0].min_distance_m, 0.0);
        assert_eq!(report.geometryless_skipped, 0);
    }

    #[test]
    fn separated_instances_do_not_clash() {
        let (v, i) = unit_cube_local();
        let rep = RepresentationRow {
            rep_id: 100,
            source_kind: "shared_or_direct".to_string(),
            vertices: v,
            indices: i,
        };
        let mut reps = HashMap::new();
        reps.insert(100u64, rep);

        let instances = vec![
            make_instance(1, 100, identity(), [0.0, 0.0, 0.0]),
            // 2 m apart along X — broad-phase already discards them at
            // tolerance 0.
            make_instance(2, 100, translate(2.0, 0.0, 0.0), [2.0, 0.0, 0.0]),
        ];
        let report = run(&instances, &reps, &ClashOptions::default()).unwrap();
        assert!(report.pairs.is_empty());
    }

    #[test]
    fn tolerance_emits_clearance_pair() {
        let (v, i) = unit_cube_local();
        let rep = RepresentationRow {
            rep_id: 100,
            source_kind: "shared_or_direct".to_string(),
            vertices: v,
            indices: i,
        };
        let mut reps = HashMap::new();
        reps.insert(100u64, rep);

        // Cubes 0.1 m apart along X. Hard-only: no pair. With 0.2 m
        // tolerance: one clearance pair.
        let instances = vec![
            make_instance(1, 100, identity(), [0.0, 0.0, 0.0]),
            make_instance(2, 100, translate(1.1, 0.0, 0.0), [1.1, 0.0, 0.0]),
        ];
        let hard_only = run(&instances, &reps, &ClashOptions::default()).unwrap();
        assert!(hard_only.pairs.is_empty());

        let with_tol = run(
            &instances,
            &reps,
            &ClashOptions {
                tolerance_m: 0.2,
                ..ClashOptions::default()
            },
        )
        .unwrap();
        assert_eq!(with_tol.pairs.len(), 1);
        assert_eq!(with_tol.pairs[0].kind, ClashKind::Clearance);
        assert!((with_tol.pairs[0].min_distance_m - 0.1).abs() < 1e-4);
    }

    #[test]
    fn composite_rep_uses_world_vertices_directly() {
        // For composite reps, the rep vertex buffer is already in world
        // coords and the instance transform is identity. Confirm the
        // engine doesn't double-transform.
        let (v, i) = unit_cube_local();
        let rep = RepresentationRow {
            rep_id: 200,
            source_kind: "composite".to_string(),
            vertices: v.clone(),
            indices: i,
        };
        let mut reps = HashMap::new();
        reps.insert(200u64, rep);

        // Even with a non-identity transform on the instance, the
        // composite path should ignore it (rep verts are world).
        let instances = vec![
            make_instance(1, 200, translate(999.0, 0.0, 0.0), [0.0, 0.0, 0.0]),
            make_instance(2, 200, translate(999.0, 0.0, 0.0), [0.5, 0.0, 0.0]),
        ];
        let report = run(&instances, &reps, &ClashOptions::default()).unwrap();
        assert_eq!(report.pairs.len(), 1);
        assert_eq!(report.pairs[0].kind, ClashKind::Hard);
    }

    #[test]
    fn geometryless_instances_are_skipped_and_reported() {
        let (v, i) = unit_cube_local();
        let rep = RepresentationRow {
            rep_id: 100,
            source_kind: "shared_or_direct".to_string(),
            vertices: v,
            indices: i,
        };
        let mut reps = HashMap::new();
        reps.insert(100u64, rep);

        let mut geometryless = make_instance(99, 100, identity(), [0.0, 0.0, 0.0]);
        geometryless.rep_id = None;

        let instances = vec![
            make_instance(1, 100, identity(), [0.0, 0.0, 0.0]),
            make_instance(2, 100, translate(0.5, 0.0, 0.0), [0.5, 0.0, 0.0]),
            geometryless,
        ];
        let report = run(&instances, &reps, &ClashOptions::default()).unwrap();
        assert_eq!(report.pairs.len(), 1);
        assert_eq!(report.geometryless_skipped, 1);
    }

    #[test]
    fn include_classes_filters_pairs() {
        let (v, i) = unit_cube_local();
        let rep = RepresentationRow {
            rep_id: 100,
            source_kind: "shared_or_direct".to_string(),
            vertices: v,
            indices: i,
        };
        let mut reps = HashMap::new();
        reps.insert(100u64, rep);

        let mut a = make_instance(1, 100, identity(), [0.0, 0.0, 0.0]);
        a.class = "Wall".to_string();
        let mut b = make_instance(2, 100, translate(0.5, 0.0, 0.0), [0.5, 0.0, 0.0]);
        b.class = "Slab".to_string();
        let instances = vec![a, b];

        // include_classes = ["Pipe"] — no Pipe instance present, so
        // no pair survives the filter even though the meshes overlap.
        let report = run(
            &instances,
            &reps,
            &ClashOptions {
                include_classes: vec!["Pipe".to_string()],
                ..ClashOptions::default()
            },
        )
        .unwrap();
        assert!(report.pairs.is_empty());

        // include_classes = ["Wall"] — passes (one side matches).
        let report = run(
            &instances,
            &reps,
            &ClashOptions {
                include_classes: vec!["Wall".to_string()],
                ..ClashOptions::default()
            },
        )
        .unwrap();
        assert_eq!(report.pairs.len(), 1);
    }

    #[test]
    fn exclude_self_class_suppresses_homogeneous_pairs() {
        let (v, i) = unit_cube_local();
        let rep = RepresentationRow {
            rep_id: 100,
            source_kind: "shared_or_direct".to_string(),
            vertices: v,
            indices: i,
        };
        let mut reps = HashMap::new();
        reps.insert(100u64, rep);

        // Two walls overlapping — suppressed when Wall is in
        // exclude_self_class.
        let instances = vec![
            make_instance(1, 100, identity(), [0.0, 0.0, 0.0]),
            make_instance(2, 100, translate(0.5, 0.0, 0.0), [0.5, 0.0, 0.0]),
        ];
        let report = run(
            &instances,
            &reps,
            &ClashOptions {
                exclude_self_class: vec!["Wall".to_string()],
                ..ClashOptions::default()
            },
        )
        .unwrap();
        assert!(report.pairs.is_empty());
    }
}
