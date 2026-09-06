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

use super::source::{self, InstanceRow, RepresentationRow, SubstrateReadError};

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

/// Semantic category for a clash pair. Engine *categorises* (never
/// drops) so consumers can triage a real-world MEP run where ~90% of
/// raw hits are joints / insulation / non-physical class involvement.
/// See GH #49 for the production data behind the rules.
///
/// Precedence (first match wins): `NonPhysical` > `Insulation` >
/// `Connection` > `Clash`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClashCategory {
    /// Actionable cross-system clash — the default bucket.
    Clash,
    /// At least one side is `IfcCovering` (typically insulation
    /// overlapping its host pipe/duct, or neighbouring insulation
    /// segments).
    Insulation,
    /// Same-medium MEP joint: one side is `<Family>Fitting`, the other
    /// is `<Family>Segment`, with the same family prefix (Pipe, Duct,
    /// CableCarrier, …). Captures fittings meeting their own run —
    /// expected geometry, not a real clash.
    Connection,
    /// At least one side is a non-physical class
    /// (`Grid`, `Annotation`, `Space`, `OpeningElement`,
    /// `VirtualElement`) — never an actionable clash, regardless of
    /// the other side.
    NonPhysical,
}

impl ClashCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Clash => "clash",
            Self::Insulation => "insulation",
            Self::Connection => "connection",
            Self::NonPhysical => "non_physical",
        }
    }
}

/// Classes the engine treats as non-physical. Substrate stores
/// classes with the `Ifc` prefix stripped (e.g. `"Grid"` for
/// `IfcGrid`).
const NON_PHYSICAL_CLASSES: &[&str] = &[
    "Grid",
    "Annotation",
    "Space",
    "OpeningElement",
    "VirtualElement",
];

fn is_non_physical(class: &str) -> bool {
    NON_PHYSICAL_CLASSES.contains(&class)
}

/// Detect a same-family MEP joint: one side ends with `"Fitting"`,
/// the other with `"Segment"`, and the prefix before that suffix
/// matches on both sides. Generic so future MEP families (e.g.
/// `CableCarrier`, `Cable`) are picked up without a hardcoded list.
fn is_same_family_joint(class_a: &str, class_b: &str) -> bool {
    let (a_prefix, a_is_fitting) = split_mep_class(class_a);
    let (b_prefix, b_is_fitting) = split_mep_class(class_b);
    matches!((a_prefix, b_prefix), (Some(pa), Some(pb)) if pa == pb && a_is_fitting != b_is_fitting)
}

/// Splits an MEP class into `(family_prefix, is_fitting)`. Returns
/// `None` prefix if the class is neither `*Fitting` nor `*Segment`.
fn split_mep_class(class: &str) -> (Option<&str>, bool) {
    if let Some(prefix) = class.strip_suffix("Fitting") {
        if !prefix.is_empty() {
            return (Some(prefix), true);
        }
    }
    if let Some(prefix) = class.strip_suffix("Segment") {
        if !prefix.is_empty() {
            return (Some(prefix), false);
        }
    }
    (None, false)
}

/// Categorise a clash pair from the substrate classes alone. Pure
/// function of the two class strings — no geometry, no IFC parse —
/// so it's cheap and easy to test.
pub fn categorise(class_a: &str, class_b: &str) -> ClashCategory {
    if is_non_physical(class_a) || is_non_physical(class_b) {
        return ClashCategory::NonPhysical;
    }
    if class_a == "Covering" || class_b == "Covering" {
        return ClashCategory::Insulation;
    }
    if is_same_family_joint(class_a, class_b) {
        return ClashCategory::Connection;
    }
    ClashCategory::Clash
}

/// One clash fact between two instances. Identity is by `ifc_id` and
/// the substrate `guid` — agents join this back to `instances.parquet`
/// for storey / type / pset enrichment. In a FEDERATED bundle (GH #50)
/// bare `ifc_id` / `guid` can collide across constituent models; the
/// unique join key there is `(ifc_id, source_model)` /
/// `(guid, source_model)`.
#[derive(Debug, Clone)]
pub struct ClashPair {
    pub ifc_id_a: u64,
    pub ifc_id_b: u64,
    pub guid_a: String,
    pub guid_b: String,
    pub class_a: String,
    pub class_b: String,
    /// `source_model` of each side (empty string on pre-v29 bundles).
    /// `source_model_a != source_model_b` is the cross-model clash
    /// predicate on a federated substrate.
    pub source_model_a: String,
    pub source_model_b: String,
    pub kind: ClashKind,
    pub category: ClashCategory,
    pub min_distance_m: f32,
}

/// Engine configuration.
#[derive(Debug, Clone)]
pub struct ClashOptions {
    /// Soft-clash band, in metres. `0.0` means "hard clashes only";
    /// positive values also emit `Clearance` pairs whose meshes are
    /// within that distance of each other. Must be finite and >= 0 —
    /// [`run`] rejects anything else rather than quietly shrinking the
    /// broad phase (GH #161).
    pub tolerance_m: f32,
    /// If set, only emit pairs where at least one side matches one of
    /// these classes (after the substrate's `class` normalisation —
    /// e.g. `"Pipe"`, not `"IfcPipe"`). Empty = no class filter.
    pub include_classes: Vec<String>,
    /// Classes that should never clash against themselves. Useful for
    /// suppressing "wall-vs-wall" noise where the user only cares about
    /// cross-discipline clashes. Empty = no self-class filter.
    pub exclude_self_class: Vec<String>,
    /// `source_model` values acting as pure REFERENCE geometry (GH #50):
    /// pairs where BOTH sides' `source_model` is in this set are
    /// dropped before narrow-phase — reference models clash against
    /// active models, never among themselves. Enforced here rather
    /// than at federation time so one federated bundle serves every
    /// reference-set choice. Empty = no reference filter.
    pub reference_only: Vec<String>,
}

impl Default for ClashOptions {
    fn default() -> Self {
        Self {
            tolerance_m: 0.0,
            include_classes: Vec::new(),
            exclude_self_class: Vec::new(),
            reference_only: Vec::new(),
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
    ///
    /// Always equals `narrow_phase_residual_details.len()`; kept as its
    /// own field because it is the long-standing stats key.
    pub narrow_phase_residuals: usize,
    /// One entry per residual, naming BOTH sides and why the pair never
    /// reached the narrow phase (GH #161). An anonymous count told an
    /// agent chasing GH #145 (MEP terminal / damper / silencer misses)
    /// nothing about which elements went missing; these rows do.
    /// Ordered by candidate-pair order, so the list is deterministic
    /// across runs and thread counts.
    pub narrow_phase_residual_details: Vec<NarrowPhaseResidual>,
}

/// A candidate pair that broad-phase admitted but narrow-phase could
/// never test, with the identity of both sides and the cause.
#[derive(Debug, Clone)]
pub struct NarrowPhaseResidual {
    pub ifc_id_a: u64,
    pub ifc_id_b: u64,
    pub guid_a: String,
    pub guid_b: String,
    pub class_a: String,
    pub class_b: String,
    pub source_model_a: String,
    pub source_model_b: String,
    /// Which side failed to produce a world mesh: `"a"`, `"b"`,
    /// `"both"`, or `"unknown"` (a cache miss, which should not happen).
    pub side: &'static str,
    /// Human-readable cause, e.g.
    /// `"a: mesh build (rep #4711, 0 tris): index buffer empty…"`.
    pub reason: String,
}

/// Build the residual record for a pair whose mesh cache lookup failed
/// on one or both sides.
fn residual_for(
    a: &InstanceRow,
    b: &InstanceRow,
    reason_a: Option<&str>,
    reason_b: Option<&str>,
) -> NarrowPhaseResidual {
    let side = match (reason_a.is_some(), reason_b.is_some()) {
        (true, true) => "both",
        (true, false) => "a",
        (false, true) => "b",
        (false, false) => "unknown",
    };
    let reason = match (reason_a, reason_b) {
        (Some(ra), Some(rb)) => format!("a: {ra}; b: {rb}"),
        (Some(ra), None) => format!("a: {ra}"),
        (None, Some(rb)) => format!("b: {rb}"),
        (None, None) => "world mesh missing from the cache".to_string(),
    };
    NarrowPhaseResidual {
        ifc_id_a: a.ifc_id,
        ifc_id_b: b.ifc_id,
        guid_a: a.guid.clone(),
        guid_b: b.guid.clone(),
        class_a: a.class.clone(),
        class_b: b.class.clone(),
        source_model_a: a.source_model.clone(),
        source_model_b: b.source_model.clone(),
        side,
        reason,
    }
}

#[derive(Debug)]
pub enum ClashError {
    Read(SubstrateReadError),
    NarrowPhase(geom::NarrowPhaseError),
    /// A caller-supplied option the engine refuses to guess about
    /// (GH #161) — e.g. a negative or NaN `tolerance_m`, which silently
    /// SHRINKS every broad-phase AABB and drops genuine hard clashes.
    InvalidOptions(String),
}

impl std::fmt::Display for ClashError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read(e) => write!(f, "substrate read: {e}"),
            Self::NarrowPhase(e) => write!(f, "narrow phase: {e}"),
            Self::InvalidOptions(m) => write!(f, "invalid options: {m}"),
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
    // Fail loudly on a tolerance the pipeline cannot honour (GH #161).
    // A negative value SHRINKS every broad-phase AABB, so genuine hard
    // clashes vanish before narrow phase while `tolerance_m > 0.0`
    // stays false and the hard path still runs — a plausible, silently
    // incomplete report. NaN poisons every expanded AABB instead:
    // zero pairs, zero warnings.
    if !options.tolerance_m.is_finite() || options.tolerance_m < 0.0 {
        return Err(ClashError::InvalidOptions(format!(
            "tolerance_m must be finite and >= 0, got {}",
            options.tolerance_m
        )));
    }

    // One anchor for the whole run (GH #156): every mesh AND every
    // broad-phase box is expressed relative to it, so the f32 mantissa
    // spends its bits on the model's extent instead of on its distance
    // from the survey origin. Per-pair anchors would be tighter still,
    // but they would force a re-bake per pair — pairs vastly outnumber
    // instances, and the bake is the narrow phase's fixed cost — so the
    // scene anchor is what keeps ONE bake per instance.
    let anchor = scene_anchor(instances);

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
            min: rebase_point(inst.bbox_min, anchor),
            max: rebase_point(inst.bbox_max, anchor),
        });
    }

    let candidate_pairs = geom::pairs_overlapping(&boxes, options.tolerance_m);

    // Class filters are cheap string checks — apply them before any
    // geometry work so filtered candidates never cost a mesh build.
    let candidate_pairs: Vec<(u32, u32)> = candidate_pairs
        .into_iter()
        .filter(|&(a, b)| class_filter_ok(&instances[a as usize], &instances[b as usize], options))
        .collect();

    // Materialise world-coord TriMeshes once per instance that shows up
    // in at least one surviving candidate pair — built in parallel, the
    // narrow phase's fixed cost. We bake-world per instance rather than
    // sharing per-rep BVHs across instances — see module docs for the
    // rationale. Arc so both sides of a pair borrow concurrently.
    use rayon::prelude::*;
    let mut ids: Vec<u32> = candidate_pairs.iter().flat_map(|&(a, b)| [a, b]).collect();
    ids.sort_unstable();
    ids.dedup();
    let mesh_cache: HashMap<u32, Result<Arc<parry3d::shape::TriMesh>, String>> = ids
        .par_iter()
        .map(|&idx| {
            (
                idx,
                build_world_trimesh(&instances[idx as usize], reps, anchor).map(Arc::new),
            )
        })
        .collect();

    // Narrow phase, in parallel over candidate pairs (order-preserving
    // collect keeps the output deterministic). At tolerance 0,
    // `intersection_test` early-outs on first BVH contact and no
    // distance traversal is ever paid (federation-scale runs were
    // spending hours in global distance traversals — GH #141 finding).
    // At tolerance > 0, a band-capped probe rejects beyond-band pairs
    // (the dominant band cost) near the BVH root; survivors pay the
    // exhaustive `distance` query, whose value is what we report —
    // the probe's value is schedule-dependent at the last ulps and is
    // never emitted (GH #143, see `min_distance_within` docs). The
    // probe cap is padded so its `None` provably agrees with the
    // exact query's band verdict despite that jitter.
    enum Outcome {
        Pair(ClashPair),
        Residual(NarrowPhaseResidual),
        Skip,
    }
    let outcomes: Vec<Result<Outcome, ClashError>> = candidate_pairs
        .par_iter()
        .map(|&(id_a, id_b)| {
            let (mesh_a, mesh_b) = match (mesh_cache.get(&id_a), mesh_cache.get(&id_b)) {
                (Some(Ok(a)), Some(Ok(b))) => (a, b),
                // At least one side has no world mesh. Record WHO and
                // WHY rather than incrementing an anonymous counter
                // (GH #161).
                (ra, rb) => {
                    return Ok(Outcome::Residual(residual_for(
                        &instances[id_a as usize],
                        &instances[id_b as usize],
                        ra.and_then(|r| r.as_ref().err()).map(|s| s.as_str()),
                        rb.and_then(|r| r.as_ref().err()).map(|s| s.as_str()),
                    )));
                }
            };

            let distance = if options.tolerance_m > 0.0 {
                // 5 mm absolute / 5 % relative pad — orders of
                // magnitude above the f32 bound-rounding slack at
                // building-scale coords, negligible extra pass-through.
                let pad = (options.tolerance_m * 0.05).max(0.005);
                if geom::min_distance_within(mesh_a, mesh_b, options.tolerance_m + pad).is_none() {
                    return Ok(Outcome::Skip);
                }
                geom::min_distance(mesh_a, mesh_b)?
            } else if geom::intersects(mesh_a, mesh_b)? {
                0.0
            } else {
                return Ok(Outcome::Skip);
            };

            let kind = if distance == 0.0 {
                ClashKind::Hard
            } else if distance <= options.tolerance_m {
                ClashKind::Clearance
            } else {
                // Broad phase admitted them via expanded AABBs but the
                // actual mesh distance is outside the tolerance band.
                return Ok(Outcome::Skip);
            };

            let a = &instances[id_a as usize];
            let b = &instances[id_b as usize];
            Ok(Outcome::Pair(ClashPair {
                ifc_id_a: a.ifc_id,
                ifc_id_b: b.ifc_id,
                guid_a: a.guid.clone(),
                guid_b: b.guid.clone(),
                class_a: a.class.clone(),
                class_b: b.class.clone(),
                source_model_a: a.source_model.clone(),
                source_model_b: b.source_model.clone(),
                kind,
                category: categorise(&a.class, &b.class),
                min_distance_m: distance,
            }))
        })
        .collect();

    let mut residuals: Vec<NarrowPhaseResidual> = Vec::new();
    let mut pairs: Vec<ClashPair> = Vec::new();
    for outcome in outcomes {
        match outcome? {
            Outcome::Pair(p) => pairs.push(p),
            Outcome::Residual(r) => residuals.push(r),
            Outcome::Skip => {}
        }
    }

    Ok(ClashReport {
        pairs,
        geometryless_skipped,
        narrow_phase_residuals: residuals.len(),
        narrow_phase_residual_details: residuals,
    })
}

/// Component-wise min of every geometry-carrying instance's world AABB,
/// in f64 — the run's rebase anchor (GH #156). Falls back to the origin
/// when nothing has geometry. Instance bboxes are f32, hence exact in
/// f64, so the rebase subtraction itself introduces no error.
///
/// A model spanning millions of metres is still beyond f32's reach in
/// ANY single-anchor scheme; this fixes the common case, where the whole
/// model sits far from the origin but is itself only hundreds of metres
/// across.
fn scene_anchor(instances: &[InstanceRow]) -> [f64; 3] {
    let mut anchor = [f64::INFINITY; 3];
    for inst in instances {
        if inst.rep_id.is_none() {
            continue;
        }
        for (slot, &b) in anchor.iter_mut().zip(inst.bbox_min.iter()) {
            let v = b as f64;
            if v.is_finite() && v < *slot {
                *slot = v;
            }
        }
    }
    for a in anchor.iter_mut() {
        if !a.is_finite() {
            *a = 0.0;
        }
    }
    anchor
}

/// Rebase one f32 point onto the run anchor, subtracting in f64.
fn rebase_point(p: [f32; 3], anchor: [f64; 3]) -> [f32; 3] {
    [
        (p[0] as f64 - anchor[0]) as f32,
        (p[1] as f64 - anchor[1]) as f32,
        (p[2] as f64 - anchor[2]) as f32,
    ]
}

fn class_filter_ok(a: &InstanceRow, b: &InstanceRow, options: &ClashOptions) -> bool {
    if !options.include_classes.is_empty() {
        let hit = options
            .include_classes
            .iter()
            .any(|c| c == &a.class || c == &b.class);
        if !hit {
            return false;
        }
    }
    if a.class == b.class && options.exclude_self_class.iter().any(|c| c == &a.class) {
        return false;
    }
    if !options.reference_only.is_empty()
        && options.reference_only.iter().any(|m| m == &a.source_model)
        && options.reference_only.iter().any(|m| m == &b.source_model)
    {
        return false;
    }
    true
}

/// Build the anchored world-coord `TriMesh` for one instance. Returns
/// the reason as an `Err` when the rep is missing from the
/// representations map or the mesh won't build — the caller turns that
/// into a named residual instead of an anonymous count (GH #161).
fn build_world_trimesh(
    inst: &InstanceRow,
    reps: &HashMap<u64, RepresentationRow>,
    anchor: [f64; 3],
) -> Result<parry3d::shape::TriMesh, String> {
    let rep_id = inst
        .rep_id
        .ok_or_else(|| "instance carries no rep_id".to_string())?;
    let rep = reps
        .get(&rep_id)
        .ok_or_else(|| format!("rep #{rep_id} missing from representations.parquet"))?;

    let world_vertices: Vec<f32> = if rep.source_kind == "composite" {
        // Composite reps already carry world-baked vertices and the
        // instance transform is identity — only the anchor rebase
        // applies.
        geom::rebase_world(&rep.vertices, anchor)
    } else {
        // shared_or_direct: rep vertices are local-frame. Apply the
        // instance's column-major 4×4 transform per-vertex, in f64,
        // and land the result in the anchored frame. Allocates a fresh
        // vertex buffer — fine for v1; a parry isometry-aware
        // narrow-phase API can lift this allocation later.
        geom::bake_world(&rep.vertices, &inst.transform, anchor)
    };

    geom::build_trimesh(&world_vertices, &rep.indices).map_err(|e| {
        format!(
            "mesh build (rep #{rep_id}, {} verts, {} tris): {e}",
            world_vertices.len() / 3,
            rep.indices.len() / 3
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit_cube_local() -> (Vec<f32>, Vec<u32>) {
        let v: Vec<f32> = vec![
            0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 1.0, 0.0,
            1.0, 1.0, 1.0, 1.0, 0.0, 1.0, 1.0,
        ];
        let i: Vec<u32> = vec![
            0, 2, 1, 0, 3, 2, 4, 5, 6, 4, 6, 7, 0, 1, 5, 0, 5, 4, 2, 3, 7, 2, 7, 6, 1, 2, 6, 1, 6,
            5, 0, 4, 7, 0, 7, 3,
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

    fn make_instance(
        idx: u64,
        rep_id: u64,
        transform: [f32; 16],
        bbox_origin: [f32; 3],
    ) -> InstanceRow {
        InstanceRow {
            ifc_id: idx,
            guid: format!("g{idx}"),
            class: "Wall".to_string(),
            source_model: String::new(),
            rep_id: Some(rep_id),
            transform,
            bbox_min: bbox_origin,
            bbox_max: [
                bbox_origin[0] + 1.0,
                bbox_origin[1] + 1.0,
                bbox_origin[2] + 1.0,
            ],
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
    fn categorise_default_is_clash() {
        assert_eq!(categorise("Wall", "Slab"), ClashCategory::Clash);
        assert_eq!(categorise("Pipe", "Beam"), ClashCategory::Clash);
    }

    #[test]
    fn categorise_covering_either_side_is_insulation() {
        assert_eq!(
            categorise("Covering", "PipeSegment"),
            ClashCategory::Insulation
        );
        assert_eq!(categorise("Wall", "Covering"), ClashCategory::Insulation);
        assert_eq!(
            categorise("Covering", "Covering"),
            ClashCategory::Insulation
        );
    }

    #[test]
    fn categorise_same_family_fitting_segment_is_connection() {
        assert_eq!(
            categorise("PipeFitting", "PipeSegment"),
            ClashCategory::Connection
        );
        assert_eq!(
            categorise("PipeSegment", "PipeFitting"),
            ClashCategory::Connection
        );
        assert_eq!(
            categorise("DuctFitting", "DuctSegment"),
            ClashCategory::Connection
        );
        assert_eq!(
            categorise("CableCarrierFitting", "CableCarrierSegment"),
            ClashCategory::Connection,
        );
    }

    #[test]
    fn categorise_cross_family_fitting_segment_is_clash() {
        // Different MEP families colliding is a real clash, not a joint.
        assert_eq!(
            categorise("PipeFitting", "DuctSegment"),
            ClashCategory::Clash
        );
        assert_eq!(
            categorise("DuctFitting", "PipeSegment"),
            ClashCategory::Clash
        );
    }

    #[test]
    fn categorise_fitting_fitting_or_segment_segment_is_clash() {
        // Two fittings (or two segments) of the same family meeting is
        // NOT a complementary joint — leave as default clash.
        assert_eq!(
            categorise("PipeFitting", "PipeFitting"),
            ClashCategory::Clash
        );
        assert_eq!(
            categorise("PipeSegment", "PipeSegment"),
            ClashCategory::Clash
        );
    }

    #[test]
    fn categorise_non_physical_takes_precedence_over_insulation() {
        // Annotation + Covering should be non_physical, not insulation —
        // the annotation involvement is the dominant non-actionable
        // signal.
        assert_eq!(
            categorise("Annotation", "Covering"),
            ClashCategory::NonPhysical
        );
        assert_eq!(categorise("Grid", "Wall"), ClashCategory::NonPhysical);
        assert_eq!(
            categorise("Space", "PipeSegment"),
            ClashCategory::NonPhysical
        );
        assert_eq!(
            categorise("OpeningElement", "Wall"),
            ClashCategory::NonPhysical,
        );
        assert_eq!(
            categorise("VirtualElement", "Door"),
            ClashCategory::NonPhysical,
        );
    }

    #[test]
    fn categorise_bare_fitting_or_segment_without_family_is_clash() {
        // Defensive: empty family prefix should not match the joint rule.
        assert_eq!(categorise("Fitting", "Segment"), ClashCategory::Clash);
    }

    #[test]
    fn run_emits_categorised_pairs() {
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
        a.class = "PipeFitting".to_string();
        let mut b = make_instance(2, 100, translate(0.5, 0.0, 0.0), [0.5, 0.0, 0.0]);
        b.class = "PipeSegment".to_string();
        let instances = vec![a, b];

        let report = run(&instances, &reps, &ClashOptions::default()).unwrap();
        assert_eq!(report.pairs.len(), 1);
        assert_eq!(report.pairs[0].category, ClashCategory::Connection);
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

    #[test]
    fn reference_only_drops_pairs_only_when_both_sides_are_reference() {
        let (v, i) = unit_cube_local();
        let rep = RepresentationRow {
            rep_id: 100,
            source_kind: "shared_or_direct".to_string(),
            vertices: v,
            indices: i,
        };
        let mut reps = HashMap::new();
        reps.insert(100u64, rep);

        // Three overlapping instances: two from a reference model, one
        // from an active model. ref-vs-ref is dropped; ref-vs-active
        // pairs survive.
        let mut r1 = make_instance(1, 100, identity(), [0.0, 0.0, 0.0]);
        r1.source_model = "REF".to_string();
        let mut r2 = make_instance(2, 100, translate(0.25, 0.0, 0.0), [0.25, 0.0, 0.0]);
        r2.source_model = "REF".to_string();
        let mut act = make_instance(3, 100, translate(0.5, 0.0, 0.0), [0.5, 0.0, 0.0]);
        act.source_model = "ACT".to_string();
        let instances = vec![r1, r2, act];

        let unfiltered = run(&instances, &reps, &ClashOptions::default()).unwrap();
        assert_eq!(unfiltered.pairs.len(), 3);

        let report = run(
            &instances,
            &reps,
            &ClashOptions {
                reference_only: vec!["REF".to_string()],
                ..ClashOptions::default()
            },
        )
        .unwrap();
        assert_eq!(report.pairs.len(), 2, "ref-vs-ref pair must be dropped");
        for p in &report.pairs {
            assert!(
                p.source_model_a == "ACT" || p.source_model_b == "ACT",
                "surviving pairs must involve the active model"
            );
        }
    }

    // ---- GH #161: option validation + named residuals ------------------

    #[test]
    fn non_finite_or_negative_tolerance_is_rejected() {
        let (v, i) = unit_cube_local();
        let mut reps = HashMap::new();
        reps.insert(
            100u64,
            RepresentationRow {
                rep_id: 100,
                source_kind: "shared_or_direct".to_string(),
                vertices: v,
                indices: i,
            },
        );
        let instances = vec![
            make_instance(1, 100, identity(), [0.0, 0.0, 0.0]),
            make_instance(2, 100, translate(0.5, 0.0, 0.0), [0.5, 0.0, 0.0]),
        ];
        for bad in [-0.01f32, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let err = run(
                &instances,
                &reps,
                &ClashOptions {
                    tolerance_m: bad,
                    ..ClashOptions::default()
                },
            )
            .expect_err("a tolerance the pipeline cannot honour must fail loudly");
            assert!(
                matches!(err, ClashError::InvalidOptions(_)),
                "expected InvalidOptions, got {err}"
            );
        }
    }

    #[test]
    fn narrow_phase_residuals_name_both_sides_and_the_reason() {
        let (v, i) = unit_cube_local();
        let mut reps = HashMap::new();
        reps.insert(
            100u64,
            RepresentationRow {
                rep_id: 100,
                source_kind: "shared_or_direct".to_string(),
                vertices: v.clone(),
                indices: i,
            },
        );
        // A rep with no triangles — the kernel cannot build it.
        reps.insert(
            101u64,
            RepresentationRow {
                rep_id: 101,
                source_kind: "shared_or_direct".to_string(),
                vertices: v,
                indices: Vec::new(),
            },
        );

        let mut a = make_instance(1, 100, identity(), [0.0, 0.0, 0.0]);
        a.class = "Wall".to_string();
        let mut b = make_instance(2, 101, translate(0.5, 0.0, 0.0), [0.5, 0.0, 0.0]);
        b.class = "AirTerminal".to_string();

        let report = run(&[a, b], &reps, &ClashOptions::default()).unwrap();
        assert!(report.pairs.is_empty());
        assert_eq!(report.narrow_phase_residuals, 1);
        assert_eq!(
            report.narrow_phase_residual_details.len(),
            report.narrow_phase_residuals
        );
        let r = &report.narrow_phase_residual_details[0];
        assert_eq!((r.ifc_id_a, r.ifc_id_b), (1, 2));
        assert_eq!(r.class_a, "Wall");
        assert_eq!(r.class_b, "AirTerminal");
        assert_eq!(r.side, "b", "only side B failed to build");
        assert!(
            r.reason.contains("rep #101"),
            "the reason must name the offending rep: {}",
            r.reason
        );
    }

    #[test]
    fn missing_representation_is_a_named_residual() {
        let (v, i) = unit_cube_local();
        let mut reps = HashMap::new();
        reps.insert(
            100u64,
            RepresentationRow {
                rep_id: 100,
                source_kind: "shared_or_direct".to_string(),
                vertices: v,
                indices: i,
            },
        );
        // Instance b points at a rep_id the substrate never wrote.
        let a = make_instance(1, 100, identity(), [0.0, 0.0, 0.0]);
        let b = make_instance(2, 777, translate(0.5, 0.0, 0.0), [0.5, 0.0, 0.0]);

        let report = run(&[a, b], &reps, &ClashOptions::default()).unwrap();
        assert_eq!(report.narrow_phase_residuals, 1);
        let r = &report.narrow_phase_residual_details[0];
        assert_eq!(r.side, "b");
        assert!(
            r.reason.contains("rep #777"),
            "the reason must name the missing rep: {}",
            r.reason
        );
    }

    // ---- GH #156: far-origin f32 quantisation --------------------------

    /// The unit cube scaled to `size` and shifted `offset_x` along X, in
    /// the REP-LOCAL frame.
    fn cube_scaled(size: f32, offset_x: f32) -> (Vec<f32>, Vec<u32>) {
        let (v, i) = unit_cube_local();
        let scaled: Vec<f32> = v
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|c| [c[0] * size + offset_x, c[1] * size, c[2] * size])
            .collect();
        (scaled, i)
    }

    fn make_instance_bbox(
        idx: u64,
        rep_id: u64,
        transform: [f32; 16],
        bbox_min: [f32; 3],
        bbox_max: [f32; 3],
    ) -> InstanceRow {
        InstanceRow {
            ifc_id: idx,
            guid: format!("g{idx}"),
            class: "Wall".to_string(),
            source_model: String::new(),
            rep_id: Some(rep_id),
            transform,
            bbox_min,
            bbox_max,
        }
    }

    #[test]
    fn clearance_distance_is_invariant_under_a_site_coordinate_shift() {
        // Two 100 mm boxes, 25 mm apart. The gap lives in the rep-local
        // frame — exactly how a far-origin model is authored: one large
        // placement translation, small local geometry — so the f32
        // transform is bit-identical between the near and far runs and
        // only the bake can lose the millimetres. In absolute f32 world
        // coordinates one ULP at 6.7e6 m is ~0.5 m, so the pre-#156
        // engine collapsed both boxes onto each other.
        let (va, ia) = cube_scaled(0.1, 0.0);
        let (vb, ib) = cube_scaled(0.1, 0.125);
        let mut reps = HashMap::new();
        reps.insert(
            300u64,
            RepresentationRow {
                rep_id: 300,
                source_kind: "shared_or_direct".to_string(),
                vertices: va,
                indices: ia,
            },
        );
        reps.insert(
            301u64,
            RepresentationRow {
                rep_id: 301,
                source_kind: "shared_or_direct".to_string(),
                vertices: vb,
                indices: ib,
            },
        );
        let opts = ClashOptions {
            tolerance_m: 0.05,
            ..ClashOptions::default()
        };

        let near = vec![
            make_instance_bbox(1, 300, identity(), [0.0, 0.0, 0.0], [0.1, 0.1, 0.1]),
            make_instance_bbox(2, 301, identity(), [0.125, 0.0, 0.0], [0.225, 0.1, 0.1]),
        ];
        let near_report = run(&near, &reps, &opts).unwrap();
        assert_eq!(near_report.pairs.len(), 1, "near pair must be found");
        let d_near = near_report.pairs[0].min_distance_m;
        assert!(
            (d_near - 0.025).abs() < 1e-5,
            "25 mm gap at the origin, got {d_near}"
        );

        // Same geometry, moved to (6.7e6, 5e5, 100) m.
        let o = [6.7e6f32, 5.0e5, 100.0];
        let m = translate(o[0], o[1], o[2]);
        let far = vec![
            make_instance_bbox(1, 300, m, o, [o[0] + 0.1, o[1] + 0.1, o[2] + 0.1]),
            make_instance_bbox(
                2,
                301,
                m,
                [o[0] + 0.125, o[1], o[2]],
                [o[0] + 0.225, o[1] + 0.1, o[2] + 0.1],
            ),
        ];
        let far_report = run(&far, &reps, &opts).unwrap();
        assert_eq!(far_report.pairs.len(), 1, "far pair must still be found");
        let d_far = far_report.pairs[0].min_distance_m;
        assert!(
            (d_far - d_near).abs() < 1e-6,
            "distance must be translation-invariant: near {d_near}, far {d_far}"
        );
    }

    #[test]
    fn scene_anchor_is_the_min_of_geometry_carrying_bboxes() {
        let mut geometryless =
            make_instance_bbox(9, 100, identity(), [-1.0e6, 0.0, 0.0], [-1.0e6, 0.0, 0.0]);
        geometryless.rep_id = None;
        let instances = vec![
            make_instance_bbox(1, 100, identity(), [10.0, 20.0, 30.0], [11.0, 21.0, 31.0]),
            make_instance_bbox(2, 100, identity(), [5.0, 25.0, 35.0], [6.0, 26.0, 36.0]),
            geometryless,
        ];
        assert_eq!(scene_anchor(&instances), [5.0, 20.0, 30.0]);
        assert_eq!(scene_anchor(&[]), [0.0, 0.0, 0.0]);
    }
}
