//! Net booleans via the Manifold CSG library.
//!
//! Bridges ifcfast's flat-buffer mesh form into `manifold-csg`'s
//! `Manifold` type so we can subtract `IfcOpeningElement` geometry
//! from its host product (wall, slab, …). The viewer-grade output
//! path needs this: today every wall renders solid because the
//! reveal-all stance emits both operands without subtracting, so
//! doors and windows are invisible holes-in-name-only.
//!
//! Behind the `csg` Cargo feature, which also pulls in the C++
//! Manifold library as a `cmake`-built dep (~1.5 minutes of
//! one-time compile). Off by default in the published Python wheel
//! pending wheel-build verification across all platforms.
//!
//! Engine vs policy: this module is the *engine* layer — given a
//! host and a list of cutters, return the net solid. Picking which
//! products are hosts, which are cutters, and whether to cut at all
//! is the caller's job (see the mesh extractor's `cut_openings` flag
//! once it's wired).

use manifold_csg::{CsgError, Manifold};

/// Reasons a CSG subtraction can fail. Wraps `manifold-csg`'s own
/// error type with one extra variant for our flat-buffer validation
/// — we reject input that doesn't match the `[x, y, z, x, y, z, …]`
/// + `[i0, i1, i2, …]` shape before handing it to manifold.
#[derive(Debug)]
pub enum CsgKernelError {
    /// Vertex buffer length isn't a multiple of 3.
    InvalidVertexBuffer,
    /// Index buffer length isn't a multiple of 3 or has zero triangles.
    InvalidIndexBuffer,
    /// One or more indices point past the end of the vertex buffer.
    OutOfBoundsIndex,
    /// manifold rejected the input or the operation. Common causes:
    /// non-manifold input topology (open shells, self-intersecting
    /// triangles, inconsistent winding). The reveal-all stance
    /// applies here too — surface the error rather than silently
    /// substitute a fallback shape.
    ManifoldRejected(CsgError),
}

impl std::fmt::Display for CsgKernelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidVertexBuffer => f.write_str("vertex buffer length not a multiple of 3"),
            Self::InvalidIndexBuffer => f.write_str("index buffer empty or not a multiple of 3"),
            Self::OutOfBoundsIndex => f.write_str("triangle index references out-of-range vertex"),
            Self::ManifoldRejected(e) => write!(f, "manifold rejected operation: {e:?}"),
        }
    }
}

impl std::error::Error for CsgKernelError {}

impl From<CsgError> for CsgKernelError {
    fn from(e: CsgError) -> Self {
        Self::ManifoldRejected(e)
    }
}

fn validate(vertices: &[f32], indices: &[u32]) -> Result<(), CsgKernelError> {
    if vertices.is_empty() || vertices.len() % 3 != 0 {
        return Err(CsgKernelError::InvalidVertexBuffer);
    }
    if indices.is_empty() || indices.len() % 3 != 0 {
        return Err(CsgKernelError::InvalidIndexBuffer);
    }
    let n_verts = (vertices.len() / 3) as u32;
    for &i in indices {
        if i >= n_verts {
            return Err(CsgKernelError::OutOfBoundsIndex);
        }
    }
    Ok(())
}

/// Bring a flat-buffer mesh into manifold-csg's `Manifold`. Validates
/// shape (multiple-of-3 buffers, in-range indices) before handing off
/// to the C++ kernel — manifold has its own validity check on top of
/// that (closed surface, consistent winding, no self-intersection)
/// which can still reject input that passes our shape check.
pub fn build_manifold(vertices: &[f32], indices: &[u32]) -> Result<Manifold, CsgKernelError> {
    validate(vertices, indices)?;
    Manifold::from_mesh_f32(vertices, 3, indices).map_err(CsgKernelError::ManifoldRejected)
}

/// Decode a `Manifold` back into ifcfast's flat-buffer form. The
/// `n_props` value from `to_mesh_f32` is always `3` for the meshes
/// we produce — we never request normals or other per-vertex
/// attributes, so the output buffer is contiguous `[x, y, z, x, y, z, …]`.
pub fn manifold_to_buffers(m: &Manifold) -> (Vec<f32>, Vec<u32>) {
    let (verts, _n_props, idx) = m.to_mesh_f32();
    (verts, idx)
}

/// Subtract each cutter from the host and return the resulting mesh.
/// Uses manifold's `batch_difference` so all cutters are folded into
/// a single CSG operation rather than N sequential subtractions —
/// faster and produces a cleaner topology.
///
/// All meshes must be in the same coordinate frame (world or local,
/// the kernel doesn't care, but they must agree). All values are
/// treated as `f32`; manifold internally promotes to `f64` for the
/// CSG and we decode back to `f32` on the way out.
///
/// Returns `Ok((vertices, indices))` on success; the result may be
/// empty (zero triangles) if the cutters fully consume the host.
pub fn subtract_many(
    host_vertices: &[f32],
    host_indices: &[u32],
    cutters: &[(&[f32], &[u32])],
) -> Result<(Vec<f32>, Vec<u32>), CsgKernelError> {
    let host = build_manifold(host_vertices, host_indices)?;

    if cutters.is_empty() {
        // No cutters → echo the host through. Round-trip via manifold
        // anyway so the output topology is consistent with the
        // "subtracted" path (manifold may collapse coincident verts
        // or otherwise normalise on conversion).
        return Ok(manifold_to_buffers(&host));
    }

    // Build all cutters first so any per-cutter validation failure
    // surfaces before the operation, not partway through.
    let mut all: Vec<Manifold> = Vec::with_capacity(cutters.len() + 1);
    all.push(host);
    for (v, i) in cutters {
        all.push(build_manifold(v, i)?);
    }

    let result = Manifold::batch_difference(&all);
    Ok(manifold_to_buffers(&result))
}

/// Convenience wrapper for the common single-cutter case
/// (one host, one opening to subtract).
pub fn subtract(
    host_vertices: &[f32],
    host_indices: &[u32],
    cutter_vertices: &[f32],
    cutter_indices: &[u32],
) -> Result<(Vec<f32>, Vec<u32>), CsgKernelError> {
    subtract_many(
        host_vertices,
        host_indices,
        &[(cutter_vertices, cutter_indices)],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1×1×1 axis-aligned cube at the given origin. Closed manifold
    /// — 8 verts, 12 triangles, outward-facing winding.
    fn unit_cube_at(origin: [f32; 3]) -> (Vec<f32>, Vec<u32>) {
        let [ox, oy, oz] = origin;
        let v: Vec<f32> = vec![
            ox, oy, oz,
            ox + 1.0, oy, oz,
            ox + 1.0, oy + 1.0, oz,
            ox, oy + 1.0, oz,
            ox, oy, oz + 1.0,
            ox + 1.0, oy, oz + 1.0,
            ox + 1.0, oy + 1.0, oz + 1.0,
            ox, oy + 1.0, oz + 1.0,
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

    #[test]
    fn build_manifold_succeeds_on_unit_cube() {
        let (v, i) = unit_cube_at([0.0, 0.0, 0.0]);
        let m = build_manifold(&v, &i).expect("unit cube is manifold");
        assert!(!m.is_empty());
        assert!((m.volume() - 1.0).abs() < 1e-4);
    }

    #[test]
    fn rejects_empty_buffers() {
        assert!(matches!(
            build_manifold(&[], &[]),
            Err(CsgKernelError::InvalidVertexBuffer)
        ));
    }

    #[test]
    fn rejects_misaligned_index_buffer() {
        let (v, _) = unit_cube_at([0.0, 0.0, 0.0]);
        let i = vec![0, 1];
        assert!(matches!(
            build_manifold(&v, &i),
            Err(CsgKernelError::InvalidIndexBuffer)
        ));
    }

    #[test]
    fn rejects_out_of_bounds_index() {
        let (v, _) = unit_cube_at([0.0, 0.0, 0.0]);
        let i = vec![0, 1, 99];
        assert!(matches!(
            build_manifold(&v, &i),
            Err(CsgKernelError::OutOfBoundsIndex)
        ));
    }

    /// Subtract a smaller cube (the "opening") wholly contained in
    /// a larger host. Expected volume: host − opening = 1.0 − 0.125
    /// = 0.875 m³ (a unit-cube host with a 0.5-cube hole).
    #[test]
    fn subtract_contained_opening_drops_volume_by_opening_volume() {
        let host = box_at([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        let opening = box_at([0.25, 0.25, 0.25], [0.75, 0.75, 0.75]);
        let (verts, idx) = subtract(&host.0, &host.1, &opening.0, &opening.1)
            .expect("contained opening subtracts cleanly");

        // Result has triangles (host shell + inner hole walls).
        assert!(!verts.is_empty(), "expected non-empty result mesh");
        assert!(!idx.is_empty(), "expected non-empty index buffer");

        // Round-trip through manifold to verify the volume.
        let m = build_manifold(&verts, &idx).expect("result is manifold");
        let expected = 1.0_f64 - 0.5_f64.powi(3); // 0.875
        assert!(
            (m.volume() - expected).abs() < 1e-3,
            "expected volume ≈ {expected}, got {}",
            m.volume()
        );
    }

    /// The opening straddles the host's boundary. Manifold should
    /// still return a valid net solid (the cut intersects the host
    /// surface). Volume = host − (overlap with opening).
    #[test]
    fn subtract_boundary_straddling_opening_returns_net_solid() {
        let host = box_at([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        // Opening covers x ∈ [0.5, 1.5]. Overlap with host: x ∈ [0.5, 1.0].
        let opening = box_at([0.5, 0.25, 0.25], [1.5, 0.75, 0.75]);
        let (verts, idx) = subtract(&host.0, &host.1, &opening.0, &opening.1)
            .expect("boundary straddling subtracts");
        let m = build_manifold(&verts, &idx).expect("result is manifold");
        // Overlap volume: 0.5 × 0.5 × 0.5 = 0.125.
        let expected = 1.0_f64 - 0.125;
        assert!(
            (m.volume() - expected).abs() < 1e-3,
            "expected ≈ {expected}, got {}",
            m.volume()
        );
    }

    /// Subtract two disjoint openings from the host in a single
    /// batch_difference call.
    #[test]
    fn subtract_many_with_two_openings() {
        let host = box_at([0.0, 0.0, 0.0], [2.0, 1.0, 1.0]);
        let o1 = box_at([0.25, 0.25, 0.25], [0.75, 0.75, 0.75]);
        let o2 = box_at([1.25, 0.25, 0.25], [1.75, 0.75, 0.75]);
        let (verts, idx) = subtract_many(
            &host.0,
            &host.1,
            &[(&o1.0, &o1.1), (&o2.0, &o2.1)],
        )
        .expect("multi-opening subtract");
        let m = build_manifold(&verts, &idx).expect("result is manifold");
        // 2.0 − 2 × 0.125 = 1.75
        let expected = 2.0_f64 - 2.0 * 0.125;
        assert!(
            (m.volume() - expected).abs() < 1e-3,
            "expected ≈ {expected}, got {}",
            m.volume()
        );
    }

    /// Empty cutter list → echo the host through (round-tripped).
    #[test]
    fn subtract_with_no_cutters_returns_host_volume() {
        let host = box_at([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        let (verts, idx) =
            subtract_many(&host.0, &host.1, &[]).expect("no-cutter subtract");
        let m = build_manifold(&verts, &idx).expect("result is manifold");
        assert!((m.volume() - 1.0).abs() < 1e-3, "expected ≈ 1.0, got {}", m.volume());
    }
}
