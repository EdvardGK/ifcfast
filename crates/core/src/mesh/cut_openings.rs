//! Apply CSG opening cuts to a `ProductMesh` in place.
//!
//! Closes the viewer-integrator's P0 #1 (GH #20). Two patterns are
//! covered, each by its own entry point:
//!
//! * **In-representation booleans** — `IfcBooleanClippingResult(host,
//!   void)`. ifcfast's extractor already tags every triangle's
//!   provenance in `ProductMesh.segments` (`"boolean_first_operand|..."`
//!   for the host, `"boolean_second_operand|..."` for the cutter), so
//!   the cut is purely consumer-side: see [`apply`].
//!
//! * **Cross-product `IfcRelVoidsElement` openings** — a solid
//!   `IfcWall` whose own representation is a plain `IfcExtrudedAreaSolid`
//!   with a separately-modelled `IfcOpeningElement` linked via
//!   `IfcRelVoidsElement`. Both products mesh independently; the cut
//!   needs the indexer's relationship arrays plus a stream-time buffer
//!   so opening meshes can fold into their host once both arrive:
//!   see [`CrossProductCut`].
//!
//! Reveal-all stance preserved: both paths are opt-in per call. The
//! substrate writer never invokes them, so `instances.parquet` /
//! `representations.parquet` keep their operand-by-operand fidelity.

use std::collections::{HashMap, HashSet};

use glam::Vec3;

use crate::geom::csg::{self, CsgKernelError};
use crate::mesh::{halfspace_clip, MeshSegment, ProductMesh};

/// Outcome counters from a cut pass. Aggregate across products in
/// the caller's loop for a top-line "n products had their openings
/// cut, m fell back to reveal-all" report.
#[derive(Debug, Default, Clone, Copy)]
pub struct CutOpeningsStats {
    /// Products where at least one `boolean_second_operand` segment
    /// existed AND the cut succeeded — the output mesh is the net
    /// solid.
    pub cut: usize,
    /// Products that had no cutter segments — the mesh is unchanged,
    /// returned through verbatim.
    pub passthrough: usize,
    /// Products that had cutter segments but the CSG operation
    /// failed (e.g. manifold rejected non-closed host topology).
    /// The original mesh is left in place — reveal-all becomes the
    /// fallback. Surfacing this count is the agent's signal that
    /// the file has authoring issues worth flagging.
    pub fallback: usize,
}

/// Apply opening cuts to `mesh` in place. Returns the per-product
/// outcome so callers can aggregate stats.
///
/// On `Outcome::Cut`, `mesh.vertices` / `mesh.indices` /
/// `mesh.segments` are replaced with the net solid; `mesh.parts` is
/// cleared because the cut destroys the per-fragment instance dedup
/// (a wall-minus-this-specific-window is unique to that wall).
///
/// On `Outcome::Passthrough` or `Outcome::Fallback`, the mesh is
/// untouched.
pub fn apply(mesh: &mut ProductMesh) -> Outcome {
    let (host_segs, cutter_segs) = partition_segments(&mesh.segments);

    if cutter_segs.is_empty() {
        return Outcome::Passthrough;
    }

    // Build the host mesh as the concatenation of all non-cutter
    // segments. For a typical wall with `IfcBooleanClippingResult` of
    // one extrusion and one void, that's just the single
    // `"boolean_first_operand|extrusion"` segment.
    let mut host = match assemble_submesh(&mesh.vertices, &mesh.indices, &host_segs) {
        Some(h) => h,
        // No host triangles at all — nothing meaningful to cut.
        // Treat as passthrough so the caller doesn't lose the cutter
        // geometry. (Pathological case: shouldn't fire on real files.)
        None => return Outcome::Passthrough,
    };

    // Partition cutters into half-space slabs and real solid cutters.
    // Half-spaces don't go through Manifold (GH #39 — Manifold's
    // boolean propagates a bbox-derived tolerance from oversized
    // half-space cutters and collapses the result to empty); instead
    // we clip the host directly against the slab's plane via
    // `mesh::halfspace_clip`. Solid cutters (real opening
    // extrusions, csg primitives) still go through `subtract_many`.
    let mut halfspace_planes: Vec<(Vec3, Vec3)> = Vec::new();
    let mut solid_cutters: Vec<(Vec<f32>, Vec<u32>)> = Vec::new();
    for seg in &cutter_segs {
        let Some(submesh) = assemble_submesh(&mesh.vertices, &mesh.indices, &[*seg]) else {
            continue;
        };
        if is_halfspace_cutter(&seg.source) {
            // Derive the plane (point on plane + agreement-direction
            // normal) from the thin slab geometry. The slab's first
            // triangle is a top-cap triangle whose CCW winding gives
            // a normal in the agreement direction. The slab's
            // centroid is essentially on the plane (the slab is
            // ≤ 1 mm thick).
            if let Some(plane) = derive_plane_from_slab(&submesh.0, &submesh.1) {
                halfspace_planes.push(plane);
            }
        } else {
            solid_cutters.push(submesh);
        }
    }

    if halfspace_planes.is_empty() && solid_cutters.is_empty() {
        return Outcome::Passthrough;
    }

    // Apply half-space clips sequentially. Each clip reduces the
    // host; if the host empties part-way we're done — the remaining
    // half-spaces and solid cutters would just confirm the empty.
    for (plane_point, plane_normal) in &halfspace_planes {
        if host.1.is_empty() {
            break;
        }
        let (new_v, new_i) = halfspace_clip::clip_by_plane(
            &host.0,
            &host.1,
            *plane_point,
            *plane_normal,
        );
        host = (new_v, new_i);
    }

    // Run remaining solid cutters (real opening extrusions, etc.)
    // through Manifold. If the host has been emptied by the
    // half-space clips alone, skip the boolean step entirely.
    let (final_v, final_i) = if solid_cutters.is_empty() {
        host
    } else if host.1.is_empty() {
        host
    } else {
        let cutter_refs: Vec<(&[f32], &[u32])> = solid_cutters
            .iter()
            .map(|(v, i)| (v.as_slice(), i.as_slice()))
            .collect();
        match csg::subtract_many(&host.0, &host.1, &cutter_refs) {
            Ok(out) => out,
            Err(_e) => return Outcome::Fallback,
        }
    };

    let triangle_count = (final_i.len() / 3) as u32;
    mesh.vertices = final_v;
    mesh.indices = final_i;
    mesh.segments = vec![MeshSegment {
        index_start: 0,
        index_count: triangle_count * 3,
        source: "cut_openings".to_string(),
    }];
    // The per-fragment instance dedup payload is no longer valid
    // after a cut — clear it. Substrate writers don't invoke this
    // path; consumers that care about parts (only ParquetSink today)
    // don't see cut meshes.
    mesh.parts.clear();
    Outcome::Cut
}

/// Recognise a half-space cutter segment by its source-tag chain.
/// Tags are emitted by `mesh::boolean::halfspace_solid` and
/// `polygonal_bounded_halfspace` with an `:agree` / `:disagree`
/// suffix; the suffix doesn't affect cut_openings — the plane
/// geometry (slab's first-triangle normal) already encodes the
/// agreement direction in world coords.
fn is_halfspace_cutter(source: &str) -> bool {
    source.split('|').any(|link| {
        link == "halfspace_plane:agree"
            || link == "halfspace_plane:disagree"
            || link == "halfspace_bounded:agree"
            || link == "halfspace_bounded:disagree"
    })
}

/// Derive `(plane_point, plane_normal_into_halfspace)` from a thin
/// half-space slab's geometry. The slab is built by
/// `boolean::halfspace_solid` / `polygonal_bounded_halfspace` as a
/// one-sided extrusion of a polygon, with the first triangle a
/// top-cap CCW triangle whose normal points along the agreement
/// direction. Returns `None` if the slab is degenerate (zero-area
/// first triangle or empty vertex buffer).
fn derive_plane_from_slab(vertices: &[f32], indices: &[u32]) -> Option<(Vec3, Vec3)> {
    if indices.len() < 3 || vertices.len() < 9 {
        return None;
    }
    let i0 = indices[0] as usize;
    let i1 = indices[1] as usize;
    let i2 = indices[2] as usize;
    if (i0 * 3 + 2).max(i1 * 3 + 2).max(i2 * 3 + 2) >= vertices.len() {
        return None;
    }
    let v0 = Vec3::new(
        vertices[i0 * 3],
        vertices[i0 * 3 + 1],
        vertices[i0 * 3 + 2],
    );
    let v1 = Vec3::new(
        vertices[i1 * 3],
        vertices[i1 * 3 + 1],
        vertices[i1 * 3 + 2],
    );
    let v2 = Vec3::new(
        vertices[i2 * 3],
        vertices[i2 * 3 + 1],
        vertices[i2 * 3 + 2],
    );
    let raw_normal = (v1 - v0).cross(v2 - v0);
    if raw_normal.length_squared() < 1e-12 {
        return None;
    }
    let agree = raw_normal.normalize();

    // Plane point: centroid of all slab vertices. The slab is thin
    // (≤ 1 mm in the agree direction), so the centroid is on (or
    // within half a thickness of) the plane — well below
    // building-element tolerance.
    let mut sum = Vec3::ZERO;
    let n = (vertices.len() / 3) as f32;
    if n < 4.0 {
        return None;
    }
    for chunk in vertices.chunks_exact(3) {
        sum += Vec3::new(chunk[0], chunk[1], chunk[2]);
    }
    let plane_point = sum / n;
    Some((plane_point, agree))
}

/// Outcome of one cut attempt on a single product.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Cut,
    Passthrough,
    Fallback,
}

impl Outcome {
    pub fn accumulate(self, stats: &mut CutOpeningsStats) {
        match self {
            Self::Cut => stats.cut += 1,
            Self::Passthrough => stats.passthrough += 1,
            Self::Fallback => stats.fallback += 1,
        }
    }
}

/// Stream-time buffer for the cross-product `IfcRelVoidsElement`
/// case. Walls authored as a solid extrusion plus a separately-
/// modelled `IfcOpeningElement` linked by `IfcRelVoidsElement` mesh
/// independently — the indexer hands us a `(opening, host)` row per
/// relation, but the products surface in entity-table order, not
/// in a topologically sorted way that would let us fold on the fly.
///
/// `CrossProductCut` solves that by buffering both sides and folding
/// at flush time. The caller (typically the `MeshSink` wrapper in
/// `extract_meshes`) routes each incoming `ProductMesh` through
/// `route` — opening products get held as cutters, host products
/// get held as hosts, anything else falls through untouched. After
/// the streaming pass completes, [`flush`] runs the in-rep [`apply`]
/// on each buffered host first (so a host with BOTH an
/// `IfcBooleanClippingResult` and cross-product openings folds
/// cleanly), then subtracts the gathered openings via
/// `geom::csg::subtract_many`.
///
/// Frame contract: all buffered meshes are expected to live in
/// `BakeFrame::Local` form — vertices near origin, true world
/// position carried separately on `ProductMesh.mesh_anchor`. The
/// fold translates each opening's vertices by
/// `(opening.mesh_anchor - host.mesh_anchor)` before passing to the
/// CSG kernel, which keeps everything in the host's local frame and
/// stays f32-safe (host/opening anchors are typically <10 m apart).
pub struct CrossProductCut {
    /// For each host step_id, the set of openings expected per
    /// `IfcRelVoidsElement`. A host may appear with multiple
    /// openings (typical for walls with several doors / windows).
    expected_openings: HashMap<u64, Vec<u64>>,
    /// Membership test for "is this product an opening?" — drives
    /// the suppression decision in `route`.
    openings: HashSet<u64>,
    /// Membership test for "is this product a host?".
    hosts: HashSet<u64>,
    /// Opening meshes that have arrived, keyed by opening step_id.
    /// Stored as `(vertices, indices, mesh_anchor)` — the smallest
    /// payload the fold needs, so the rest of `ProductMesh` can be
    /// dropped immediately.
    held_openings: HashMap<u64, (Vec<f32>, Vec<u32>, [f64; 3])>,
    /// Host meshes waiting for their openings, keyed by host
    /// step_id. The full `ProductMesh` is held so the existing
    /// MeshSink encoding logic (guid, entity, anchor, byte buffers)
    /// can run on the folded result without re-derivation.
    held_hosts: HashMap<u64, ProductMesh>,
}

/// What `CrossProductCut::route` decided for an incoming mesh.
#[derive(Debug)]
pub enum Routed {
    /// Product was identified as an `IfcOpeningElement` cutter and
    /// stashed for later folding. Caller MUST drop the mesh without
    /// forwarding it to the inner sink — in cut mode openings are
    /// not user-visible products.
    Suppressed,
    /// Product was identified as a host with at least one expected
    /// opening and stashed for later folding. Caller MUST drop the
    /// mesh without forwarding it; the eventual folded result comes
    /// out of `flush`.
    Held,
    /// Product is neither an opening nor a host (the common case).
    /// Caller continues with their normal in-rep `apply` pipeline.
    PassThrough(ProductMesh),
}

impl CrossProductCut {
    /// Build the index from the indexer's parallel arrays. Both
    /// slices must be the same length (one row per
    /// `IfcRelVoidsElement`).
    pub fn from_indexer(voids_opening: &[u64], voids_host: &[u64]) -> Self {
        let mut expected_openings: HashMap<u64, Vec<u64>> = HashMap::new();
        let mut openings: HashSet<u64> = HashSet::new();
        let mut hosts: HashSet<u64> = HashSet::new();
        for (opening, host) in voids_opening.iter().zip(voids_host.iter()) {
            // Skip self-voids (pathological but cheap to guard).
            if opening == host {
                continue;
            }
            openings.insert(*opening);
            hosts.insert(*host);
            expected_openings.entry(*host).or_default().push(*opening);
        }
        Self {
            expected_openings,
            openings,
            hosts,
            held_openings: HashMap::new(),
            held_hosts: HashMap::new(),
        }
    }

    /// `true` if the index is empty (no `IfcRelVoidsElement` in the
    /// file). Callers can use this to short-circuit the wrapper and
    /// keep the hot path identical to non-cut behaviour.
    pub fn is_empty(&self) -> bool {
        self.expected_openings.is_empty()
    }

    /// Classify a mesh against the relationship index and stash it
    /// if it's a participant. Returns [`Routed::PassThrough`] for
    /// the common case where the product is neither an opening nor
    /// a host — the caller continues with their normal pipeline.
    pub fn route(&mut self, mesh: ProductMesh) -> Routed {
        let id = mesh.ifc_id;
        if self.openings.contains(&id) {
            self.held_openings
                .insert(id, (mesh.vertices, mesh.indices, mesh.mesh_anchor));
            return Routed::Suppressed;
        }
        if self.hosts.contains(&id) {
            self.held_hosts.insert(id, mesh);
            return Routed::Held;
        }
        Routed::PassThrough(mesh)
    }

    /// Fold every buffered host with its arrived openings and yield
    /// the (mesh, outcome) pairs ready for the caller's emit path.
    ///
    /// Per-host sequence: run the in-rep [`apply`] first (so a host
    /// authored as `IfcBooleanClippingResult` plus a cross-product
    /// opening collapses the in-rep operands before the cross fold),
    /// then translate each opening's vertices by
    /// `(opening.mesh_anchor - host.mesh_anchor)` and call
    /// `geom::csg::subtract_many`. Combined outcome:
    /// * `Cut` — at least one subtraction succeeded on this host.
    /// * `Fallback` — cutters existed but every subtraction failed.
    /// * `Passthrough` — no openings ever arrived (e.g. all expected
    ///   openings were geometryless or otherwise unmeshed).
    pub fn flush(&mut self) -> Vec<(ProductMesh, Outcome)> {
        let mut out = Vec::with_capacity(self.held_hosts.len());
        let held_hosts = std::mem::take(&mut self.held_hosts);
        for (host_id, mut host_mesh) in held_hosts {
            // Run in-rep cut first. Combined outcome accounting is
            // simplified to "did any subtraction happen on this
            // product?" — see the doc above.
            let in_rep = apply(&mut host_mesh);

            // Gather arrived openings for this host.
            let expected = self
                .expected_openings
                .get(&host_id)
                .cloned()
                .unwrap_or_default();
            let cutter_buffers: Vec<(Vec<f32>, Vec<u32>)> = expected
                .iter()
                .filter_map(|oid| {
                    let (v, i, anchor) = self.held_openings.get(oid)?;
                    let off = [
                        (anchor[0] - host_mesh.mesh_anchor[0]) as f32,
                        (anchor[1] - host_mesh.mesh_anchor[1]) as f32,
                        (anchor[2] - host_mesh.mesh_anchor[2]) as f32,
                    ];
                    let translated: Vec<f32> = v
                        .chunks_exact(3)
                        .flat_map(|c| {
                            [c[0] + off[0], c[1] + off[1], c[2] + off[2]]
                        })
                        .collect();
                    Some((translated, i.clone()))
                })
                .collect();

            let combined = if cutter_buffers.is_empty() {
                // No cross-product cutters arrived → outcome stays
                // whatever the in-rep apply said (typically
                // Passthrough for a plain extruded wall).
                in_rep
            } else {
                let cutter_refs: Vec<(&[f32], &[u32])> = cutter_buffers
                    .iter()
                    .map(|(v, i)| (v.as_slice(), i.as_slice()))
                    .collect();
                match csg::subtract_many(
                    &host_mesh.vertices,
                    &host_mesh.indices,
                    &cutter_refs,
                ) {
                    Ok((verts, idx)) => {
                        let triangle_count = (idx.len() / 3) as u32;
                        host_mesh.vertices = verts;
                        host_mesh.indices = idx;
                        host_mesh.segments = vec![MeshSegment {
                            index_start: 0,
                            index_count: triangle_count * 3,
                            source: "cut_openings".to_string(),
                        }];
                        host_mesh.parts.clear();
                        Outcome::Cut
                    }
                    Err(_) => {
                        // If the in-rep cut already succeeded, keep
                        // that as the win and surface the
                        // cross-product failure only via a Fallback
                        // bump when in-rep also passed/failed.
                        if matches!(in_rep, Outcome::Cut) {
                            Outcome::Cut
                        } else {
                            Outcome::Fallback
                        }
                    }
                }
            };

            out.push((host_mesh, combined));
        }
        // Any opening meshes still held belong to hosts that never
        // arrived (geometryless host, host outside the meshable
        // product set). Drop them — in cut mode they're not visible
        // products on their own.
        self.held_openings.clear();
        out
    }
}

/// Classify segments by whether their source tag marks them as a
/// boolean subtractor. A segment's `source` is a compound tag like
/// `"boolean_first_operand|extrusion"` or
/// `"boolean_second_operand|halfspace_bounded"` — see
/// `mesh::boolean::retag` for the construction rule. Any segment
/// whose chain contains `"boolean_second_operand"` is treated as a
/// cutter; everything else is host.
fn partition_segments(segments: &[MeshSegment]) -> (Vec<&MeshSegment>, Vec<&MeshSegment>) {
    let mut hosts = Vec::with_capacity(segments.len());
    let mut cutters = Vec::with_capacity(segments.len());
    for s in segments {
        if is_cutter(&s.source) {
            cutters.push(s);
        } else {
            hosts.push(s);
        }
    }
    (hosts, cutters)
}

fn is_cutter(source: &str) -> bool {
    // `|` is the chain separator in the compound source tag (see
    // `boolean::retag`). A nested boolean produces source tags like
    // `"boolean_first_operand|boolean_first_operand|extrusion"` for
    // the deepest host and `"boolean_first_operand|boolean_second_operand|extrusion"`
    // for a cutter under a wrapping host. Treat any chain link
    // mentioning `boolean_second_operand` as a cutter — i.e. the
    // triangle was structurally on the "subtract" side of some
    // boolean node somewhere up the tree.
    source.split('|').any(|link| link == "boolean_second_operand")
}

/// Pull the triangles addressed by `segments` out of the shared
/// `vertices` / `indices` buffers into a compact (vertices, indices)
/// pair where the indices are remapped to 0..N over a deduplicated
/// vertex set. Returns `None` if the slice is empty.
fn assemble_submesh(
    vertices: &[f32],
    indices: &[u32],
    segments: &[&MeshSegment],
) -> Option<(Vec<f32>, Vec<u32>)> {
    if segments.is_empty() {
        return None;
    }

    let mut remap: HashMap<u32, u32> = HashMap::new();
    let mut out_vertices: Vec<f32> = Vec::new();
    let mut out_indices: Vec<u32> = Vec::new();

    for seg in segments {
        let start = seg.index_start as usize;
        let end = start + seg.index_count as usize;
        if end > indices.len() {
            // Malformed segment range — skip rather than crash.
            continue;
        }
        for &orig_idx in &indices[start..end] {
            let new_idx = *remap.entry(orig_idx).or_insert_with(|| {
                let n = out_vertices.len() / 3;
                let base = (orig_idx as usize) * 3;
                // Guard against truncated vertex buffer just in case.
                if base + 2 < vertices.len() {
                    out_vertices.push(vertices[base]);
                    out_vertices.push(vertices[base + 1]);
                    out_vertices.push(vertices[base + 2]);
                    n as u32
                } else {
                    u32::MAX
                }
            });
            if new_idx == u32::MAX {
                return None;
            }
            out_indices.push(new_idx);
        }
    }

    if out_vertices.is_empty() || out_indices.is_empty() {
        None
    } else {
        Some((out_vertices, out_indices))
    }
}

/// Convenience used by `_core.extract_meshes` when it wraps the
/// `ProductSink` chain — exposes the `(verts, idx)` pair without
/// requiring the caller to also pass `&mut ProductMesh`. Internal.
#[allow(dead_code)]
pub(crate) fn _expose_partition<'a>(
    segments: &'a [MeshSegment],
) -> (Vec<&'a MeshSegment>, Vec<&'a MeshSegment>) {
    partition_segments(segments)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1×1×1 box at given origin, returns (verts, idx) in our flat
    /// buffer form.
    fn box_at(min: [f32; 3], max: [f32; 3]) -> (Vec<f32>, Vec<u32>) {
        let v: Vec<f32> = vec![
            min[0], min[1], min[2],
            max[0], min[1], min[2],
            max[0], max[1], min[2],
            min[0], max[1], min[2],
            min[0], min[1], max[2],
            max[0], min[1], max[2],
            max[0], max[1], max[2],
            min[0], max[1], max[2],
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

    /// Build a synthetic ProductMesh that mimics the extractor's
    /// output for a wall authored as
    /// `IfcBooleanClippingResult(big_box, small_box)`: vertices for
    /// both fragments concatenated, indices for the first fragment
    /// then the second fragment, two segments tagged respectively.
    fn synthetic_wall_with_opening() -> ProductMesh {
        let (host_v, host_i) = box_at([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        let (cut_v, cut_i) = box_at([0.25, 0.25, 0.25], [0.75, 0.75, 0.75]);

        let host_vert_count = (host_v.len() / 3) as u32;
        let cut_idx_shifted: Vec<u32> = cut_i.iter().map(|i| i + host_vert_count).collect();

        let mut vertices = host_v;
        vertices.extend_from_slice(&cut_v);
        let host_index_count = host_i.len() as u32;
        let cut_index_count = cut_idx_shifted.len() as u32;
        let mut indices = host_i;
        indices.extend_from_slice(&cut_idx_shifted);

        ProductMesh {
            guid: "wall-test-guid".into(),
            entity: "IfcWall".into(),
            ifc_id: 1,
            vertices,
            indices,
            source: "boolean_first_operand|extrusion",
            segments: vec![
                MeshSegment {
                    index_start: 0,
                    index_count: host_index_count,
                    source: "boolean_first_operand|extrusion".to_string(),
                },
                MeshSegment {
                    index_start: host_index_count,
                    index_count: cut_index_count,
                    source: "boolean_second_operand|extrusion".to_string(),
                },
            ],
            placement_origin: [0.0, 0.0, 0.0],
            parts: Vec::new(),
            world_transform: [
                1.0, 0.0, 0.0, 0.0,
                0.0, 1.0, 0.0, 0.0,
                0.0, 0.0, 1.0, 0.0,
                0.0, 0.0, 0.0, 1.0,
            ],
            world_origin: [0.0, 0.0, 0.0],
            mesh_anchor: [0.0, 0.0, 0.0],
            surface_color: None,
        }
    }

    #[test]
    fn partition_classifies_segments_by_chain_link() {
        assert!(is_cutter("boolean_second_operand|extrusion"));
        assert!(is_cutter("boolean_first_operand|boolean_second_operand|halfspace_bounded"));
        assert!(!is_cutter("boolean_first_operand|extrusion"));
        assert!(!is_cutter("extrusion"));
        assert!(!is_cutter("mapped"));
    }

    #[test]
    fn apply_passes_through_when_no_cutters() {
        let (v, i) = box_at([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        let idx_count = i.len() as u32;
        let mut mesh = ProductMesh {
            guid: "plain-wall".into(),
            entity: "IfcWall".into(),
            ifc_id: 2,
            vertices: v,
            indices: i,
            source: "extrusion",
            segments: vec![MeshSegment {
                index_start: 0,
                index_count: idx_count,
                source: "extrusion".to_string(),
            }],
            placement_origin: [0.0, 0.0, 0.0],
            parts: Vec::new(),
            world_transform: [
                1.0, 0.0, 0.0, 0.0,
                0.0, 1.0, 0.0, 0.0,
                0.0, 0.0, 1.0, 0.0,
                0.0, 0.0, 0.0, 1.0,
            ],
            world_origin: [0.0, 0.0, 0.0],
            mesh_anchor: [0.0, 0.0, 0.0],
            surface_color: None,
        };
        let before_verts = mesh.vertices.clone();
        assert_eq!(apply(&mut mesh), Outcome::Passthrough);
        assert_eq!(mesh.vertices, before_verts);
        assert_eq!(mesh.segments.len(), 1);
        assert_eq!(mesh.segments[0].source, "extrusion");
    }

    #[test]
    fn apply_cuts_opening_and_rewrites_segments() {
        let mut mesh = synthetic_wall_with_opening();
        // Sanity: input has two segments.
        assert_eq!(mesh.segments.len(), 2);

        let outcome = apply(&mut mesh);
        assert_eq!(outcome, Outcome::Cut);

        // Output has exactly one segment, tagged "cut_openings".
        assert_eq!(mesh.segments.len(), 1);
        assert_eq!(mesh.segments[0].source, "cut_openings");
        assert_eq!(mesh.segments[0].index_start, 0);
        assert_eq!(mesh.segments[0].index_count as usize, mesh.indices.len());

        // Volume should equal host (1.0) minus opening (0.5³ = 0.125).
        let m = csg::build_manifold(&mesh.vertices, &mesh.indices)
            .expect("cut wall is manifold");
        let expected = 1.0_f64 - 0.5_f64.powi(3);
        assert!(
            (m.volume() - expected).abs() < 1e-3,
            "expected volume ≈ {expected}, got {}",
            m.volume()
        );
    }

    #[test]
    fn apply_clears_parts_after_cut() {
        let mut mesh = synthetic_wall_with_opening();
        // Pretend the extractor populated parts for substrate dedup.
        mesh.parts.push(crate::mesh::InstancePart {
            rep_step_id: 42,
            instance_transform: [
                1.0, 0.0, 0.0, 0.0,
                0.0, 1.0, 0.0, 0.0,
                0.0, 0.0, 1.0, 0.0,
                0.0, 0.0, 0.0, 1.0,
            ],
            local_vertices: vec![0.0, 0.0, 0.0],
            local_indices: vec![],
            index_start: 0,
            index_count: 0,
            source: "boolean_first_operand|extrusion".into(),
            surface_color: None,
        });
        let _ = apply(&mut mesh);
        assert!(mesh.parts.is_empty(), "parts must clear post-cut");
    }

    #[test]
    fn stats_accumulator_threads_outcomes() {
        let mut stats = CutOpeningsStats::default();
        Outcome::Cut.accumulate(&mut stats);
        Outcome::Cut.accumulate(&mut stats);
        Outcome::Passthrough.accumulate(&mut stats);
        Outcome::Fallback.accumulate(&mut stats);
        assert_eq!(stats.cut, 2);
        assert_eq!(stats.passthrough, 1);
        assert_eq!(stats.fallback, 1);
    }
}

// Silence unused-import warning on the geom::csg::CsgKernelError type
// in non-test compiles where the error variant naming would otherwise
// be flagged. We keep the alias for callers that want to refer to it.
#[allow(dead_code)]
type _CsgError = CsgKernelError;
