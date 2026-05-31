//! Apply CSG opening cuts to a `ProductMesh` in place.
//!
//! Closes the viewer-integrator's P0 #1 (GH #20): today every wall
//! whose IFC representation is `IfcBooleanClippingResult(host, void)`
//! ships BOTH operands as visible triangles (per the reveal-all
//! stance), so viewers render solid walls with the door / window
//! geometry sitting on top as a separate volume. This module gives
//! the consumer an opt-in path that subtracts the cutter volumes
//! from the host and emits a single, holes-cut net mesh.
//!
//! Implementation note: ifcfast's extractor already tags every
//! triangle's provenance in `ProductMesh.segments` —
//! `"boolean_first_operand|..."` for the host, `"boolean_second_operand|..."`
//! for the cutter. We don't need to re-walk the IFC tree or consult
//! `IfcRelVoidsElement` for this in-product case; the segment tags
//! tell us which triangles to subtract. (Cross-product
//! `IfcRelVoidsElement` openings — host wall + separately-modelled
//! IfcOpeningElement linked by `IfcRelVoidsElement`, where the
//! wall's representation is a solid extrusion with no boolean — are
//! a separate path that this module does NOT handle yet. See the
//! `[[viewer-feedback-2026-05-30]]` memory for follow-on.)
//!
//! Reveal-all stance preserved: this is opt-in per call. The
//! substrate writer never invokes it, so `instances.parquet` /
//! `representations.parquet` keep their operand-by-operand fidelity.

use std::collections::HashMap;

use crate::geom::csg::{self, CsgKernelError};
use crate::mesh::{MeshSegment, ProductMesh};

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
    let host = match assemble_submesh(&mesh.vertices, &mesh.indices, &host_segs) {
        Some(h) => h,
        // No host triangles at all — nothing meaningful to cut.
        // Treat as passthrough so the caller doesn't lose the cutter
        // geometry. (Pathological case: shouldn't fire on real files.)
        None => return Outcome::Passthrough,
    };

    let cutter_meshes: Vec<(Vec<f32>, Vec<u32>)> = cutter_segs
        .iter()
        .filter_map(|s| assemble_submesh(&mesh.vertices, &mesh.indices, &[*s]))
        .collect();

    if cutter_meshes.is_empty() {
        return Outcome::Passthrough;
    }

    let cutter_refs: Vec<(&[f32], &[u32])> = cutter_meshes
        .iter()
        .map(|(v, i)| (v.as_slice(), i.as_slice()))
        .collect();

    match csg::subtract_many(&host.0, &host.1, &cutter_refs) {
        Ok((verts, idx)) => {
            let triangle_count = (idx.len() / 3) as u32;
            mesh.vertices = verts;
            mesh.indices = idx;
            mesh.segments = vec![MeshSegment {
                index_start: 0,
                index_count: triangle_count * 3,
                source: "cut_openings".to_string(),
            }];
            // The per-fragment instance dedup payload is no longer
            // valid after a cut — clear it. Substrate writers don't
            // invoke this path; consumers that care about parts
            // (only ParquetSink today) don't see cut meshes.
            mesh.parts.clear();
            Outcome::Cut
        }
        Err(_e) => Outcome::Fallback,
    }
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
