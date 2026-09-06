//! Bridge from ifcfast's flat-buffer mesh representation (the same one
//! `crate::mesh::ProductMesh` and `crate::bundle::RepresentationRecord`
//! use) into `parry3d::shape::TriMesh`, which carries a BVH built at
//! construction.
//!
//! Builds are O(triangles × log triangles) for the BVH construction.
//! Build once per representation; reuse across every clash query
//! against that shape.

use parry3d::math::Point;
use parry3d::shape::TriMesh;

/// Reasons we may refuse to build a `TriMesh`. parry3d's own
/// `TriMeshBuilderError` is collapsed into one variant — the
/// distinction (degenerate vs duplicate triangle vs unconnected) is
/// only useful while iterating the kernel and not exposed to callers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeshBuildError {
    /// Vertex buffer length isn't a multiple of 3.
    InvalidVertexBuffer,
    /// Index buffer length isn't a multiple of 3 or has zero triangles.
    InvalidIndexBuffer,
    /// One or more indices point past the end of the vertex buffer.
    OutOfBoundsIndex,
    /// parry3d's `TriMesh::new` rejected the mesh (e.g. degenerate
    /// triangles, internal validation failure).
    KernelRejected(String),
}

impl std::fmt::Display for MeshBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidVertexBuffer => f.write_str("vertex buffer length not a multiple of 3"),
            Self::InvalidIndexBuffer => f.write_str("index buffer empty or not a multiple of 3"),
            Self::OutOfBoundsIndex => f.write_str("triangle index references out-of-range vertex"),
            Self::KernelRejected(msg) => write!(f, "parry3d rejected mesh: {msg}"),
        }
    }
}

impl std::error::Error for MeshBuildError {}

/// Build a parry3d `TriMesh` (with an internal BVH) from the flat
/// `[x, y, z, x, y, z, …]` + `[i0, i1, i2, …]` form ifcfast already
/// emits.
///
/// Returns an error rather than panicking on invalid input — callers
/// can decide whether to skip the offending product, log it, or surface
/// it as a clash-engine residual ("we couldn't intersection-test this
/// representation because it's degenerate"). The reveal-all stance
/// applies here too: we never silently substitute a fallback shape.
pub fn build_trimesh(vertices: &[f32], indices: &[u32]) -> Result<TriMesh, MeshBuildError> {
    if vertices.is_empty() || !vertices.len().is_multiple_of(3) {
        return Err(MeshBuildError::InvalidVertexBuffer);
    }
    if indices.is_empty() || !indices.len().is_multiple_of(3) {
        return Err(MeshBuildError::InvalidIndexBuffer);
    }

    let n_verts = (vertices.len() / 3) as u32;
    for &i in indices {
        if i >= n_verts {
            return Err(MeshBuildError::OutOfBoundsIndex);
        }
    }

    let points: Vec<Point<f32>> = vertices
        .as_chunks::<3>()
        .0
        .iter()
        .map(|c| Point::new(c[0], c[1], c[2]))
        .collect();
    let tris: Vec<[u32; 3]> = indices
        .as_chunks::<3>()
        .0
        .iter()
        .map(|t| [t[0], t[1], t[2]])
        .collect();

    // parry3d 0.17's `TriMesh::new` is infallible and returns the mesh
    // directly. (Newer versions returned `Result`; the `KernelRejected`
    // variant stays in the error enum for that future-proofing.)
    Ok(TriMesh::new(points, tris))
}

/// Transform a rep-local vertex buffer into an ANCHORED world frame:
/// `world = m * local - anchor`, with the whole multiply-add done in
/// f64 before the result is packed back to f32.
///
/// `m` is a column-major 4x4 affine — treated as a true 4x4 (any scale
/// the placement chain carried is honoured; IFC placements are normally
/// pure rotation+translation but we don't assume it).
///
/// Why the anchor (GH #156): on a site-coordinate model a world
/// northing is ~6.7e6 m, where one f32 ULP is ~0.5 m — a 25 mm
/// clearance verdict computed in absolute f32 world coordinates is
/// quantisation noise, not geometry. Rebasing on an anchor near the
/// data keeps every value the kernel sees small, so the f32 mantissa
/// spends its bits on millimetres instead of megametres. Distance,
/// penetration and intersection are all translation-invariant, so a
/// result in the anchored frame IS the result in the world frame —
/// as long as BOTH sides of a pair share one anchor.
pub fn bake_world(local: &[f32], m: &[f32; 16], anchor: [f64; 3]) -> Vec<f32> {
    let m: [f64; 16] = std::array::from_fn(|i| m[i] as f64);
    let mut out = Vec::with_capacity(local.len());
    for v in local.as_chunks::<3>().0 {
        let (x, y, z) = (v[0] as f64, v[1] as f64, v[2] as f64);
        // Column-major indexing: m[col * 4 + row]
        out.push((m[0] * x + m[4] * y + m[8] * z + m[12] - anchor[0]) as f32);
        out.push((m[1] * x + m[5] * y + m[9] * z + m[13] - anchor[1]) as f32);
        out.push((m[2] * x + m[6] * y + m[10] * z + m[14] - anchor[2]) as f32);
    }
    out
}

/// Rebase an ALREADY world-baked vertex buffer onto `anchor` (the
/// composite-rep path, where the substrate stores world coordinates and
/// the instance transform is identity). Subtraction in f64, same
/// reasoning as [`bake_world`].
pub fn rebase_world(world: &[f32], anchor: [f64; 3]) -> Vec<f32> {
    let mut out = Vec::with_capacity(world.len());
    for v in world.as_chunks::<3>().0 {
        out.push((v[0] as f64 - anchor[0]) as f32);
        out.push((v[1] as f64 - anchor[1]) as f32);
        out.push((v[2] as f64 - anchor[2]) as f32);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One axis-aligned unit cube — closed manifold with 8 verts / 12 tris.
    /// The geom tests reuse this fixture; broad-phase / narrow-phase
    /// tests place two copies at different translations.
    pub(crate) fn unit_cube_at(origin: [f32; 3]) -> (Vec<f32>, Vec<u32>) {
        let [ox, oy, oz] = origin;
        let v: Vec<f32> = vec![
            ox,
            oy,
            oz, // 0  (-,-,-)
            ox + 1.0,
            oy,
            oz, // 1  (+,-,-)
            ox + 1.0,
            oy + 1.0,
            oz, // 2  (+,+,-)
            ox,
            oy + 1.0,
            oz, // 3  (-,+,-)
            ox,
            oy,
            oz + 1.0, // 4  (-,-,+)
            ox + 1.0,
            oy,
            oz + 1.0, // 5  (+,-,+)
            ox + 1.0,
            oy + 1.0,
            oz + 1.0, // 6  (+,+,+)
            ox,
            oy + 1.0,
            oz + 1.0, // 7  (-,+,+)
        ];
        // Outward-facing winding (counter-clockwise from outside).
        let i: Vec<u32> = vec![
            0, 2, 1, 0, 3, 2, // bottom (-Z)
            4, 5, 6, 4, 6, 7, // top (+Z)
            0, 1, 5, 0, 5, 4, // front (-Y)
            2, 3, 7, 2, 7, 6, // back (+Y)
            1, 2, 6, 1, 6, 5, // right (+X)
            0, 4, 7, 0, 7, 3, // left (-X)
        ];
        (v, i)
    }

    #[test]
    fn build_succeeds_on_unit_cube() {
        let (v, i) = unit_cube_at([0.0, 0.0, 0.0]);
        let mesh = build_trimesh(&v, &i).expect("unit cube should build");
        assert_eq!(mesh.vertices().len(), 8);
        assert_eq!(mesh.indices().len(), 12);
    }

    #[test]
    fn rejects_empty_buffers() {
        // TriMesh isn't PartialEq, so the test asserts via the
        // pattern-match form instead of `assert_eq!`.
        assert!(matches!(
            build_trimesh(&[], &[]),
            Err(MeshBuildError::InvalidVertexBuffer)
        ));
    }

    #[test]
    fn rejects_misaligned_vertex_buffer() {
        let v = vec![0.0, 1.0]; // 2 floats — not a multiple of 3
        let i = vec![0, 1, 2];
        assert!(matches!(
            build_trimesh(&v, &i),
            Err(MeshBuildError::InvalidVertexBuffer)
        ));
    }

    #[test]
    fn rejects_misaligned_index_buffer() {
        let (v, _) = unit_cube_at([0.0, 0.0, 0.0]);
        let i = vec![0, 1]; // 2 indices — not a multiple of 3
        assert!(matches!(
            build_trimesh(&v, &i),
            Err(MeshBuildError::InvalidIndexBuffer)
        ));
    }

    #[test]
    fn rejects_out_of_bounds_index() {
        let (v, _) = unit_cube_at([0.0, 0.0, 0.0]);
        let i = vec![0, 1, 99]; // 99 is past the 8 vertices
        assert!(matches!(
            build_trimesh(&v, &i),
            Err(MeshBuildError::OutOfBoundsIndex)
        ));
    }

    fn translation(x: f32, y: f32, z: f32) -> [f32; 16] {
        let mut m = [0.0f32; 16];
        m[0] = 1.0;
        m[5] = 1.0;
        m[10] = 1.0;
        m[15] = 1.0;
        m[12] = x;
        m[13] = y;
        m[14] = z;
        m
    }

    #[test]
    fn bake_world_keeps_millimetres_at_site_coordinates() {
        // GH #156: a 1 mm feature 6.7e6 m from the origin survives the
        // f64 bake + anchor rebase. In absolute f32 it cannot — one ULP
        // up there is ~0.5 m.
        let local = vec![0.0f32, 0.0, 0.0, 0.001, 0.0, 0.0];
        let m = translation(6.7e6, 5.0e5, 100.0);
        let anchor = [6.7e6f64, 5.0e5, 100.0];

        let baked = bake_world(&local, &m, anchor);
        assert_eq!(baked[0], 0.0);
        assert!(
            (baked[3] - 0.001).abs() < 1e-9,
            "1 mm feature must survive the bake, got {}",
            baked[3]
        );

        // What the unanchored all-f32 path did with the same feature.
        let naive = (6.7e6f32 + 0.001) - 6.7e6f32;
        assert_eq!(naive, 0.0, "absolute f32 quantises 1 mm to nothing");
    }

    #[test]
    fn bake_world_honours_rotation_and_scale() {
        // 90° about Z with a 2x scale, column-major.
        let mut m = [0.0f32; 16];
        m[1] = 2.0; // column 0 -> +Y
        m[4] = -2.0; // column 1 -> -X
        m[10] = 2.0;
        m[15] = 1.0;
        m[12] = 10.0;
        let baked = bake_world(&[1.0, 0.0, 0.5], &m, [0.0, 0.0, 0.0]);
        assert!((baked[0] - 10.0).abs() < 1e-6, "{baked:?}");
        assert!((baked[1] - 2.0).abs() < 1e-6, "{baked:?}");
        assert!((baked[2] - 1.0).abs() < 1e-6, "{baked:?}");
    }

    #[test]
    fn rebase_world_subtracts_the_anchor_in_f64() {
        let world = vec![6.7e6f32, 5.0e5, 100.0];
        let out = rebase_world(&world, [6.7e6, 5.0e5, 100.0]);
        assert_eq!(out, vec![0.0, 0.0, 0.0]);
    }
}
