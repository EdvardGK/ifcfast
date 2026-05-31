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
    if vertices.is_empty() || vertices.len() % 3 != 0 {
        return Err(MeshBuildError::InvalidVertexBuffer);
    }
    if indices.is_empty() || indices.len() % 3 != 0 {
        return Err(MeshBuildError::InvalidIndexBuffer);
    }

    let n_verts = (vertices.len() / 3) as u32;
    for &i in indices {
        if i >= n_verts {
            return Err(MeshBuildError::OutOfBoundsIndex);
        }
    }

    let points: Vec<Point<f32>> = vertices
        .chunks_exact(3)
        .map(|c| Point::new(c[0], c[1], c[2]))
        .collect();
    let tris: Vec<[u32; 3]> = indices
        .chunks_exact(3)
        .map(|t| [t[0], t[1], t[2]])
        .collect();

    // parry3d 0.17's `TriMesh::new` is infallible and returns the mesh
    // directly. (Newer versions returned `Result`; the `KernelRejected`
    // variant stays in the error enum for that future-proofing.)
    Ok(TriMesh::new(points, tris))
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
            ox, oy, oz,             // 0  (-,-,-)
            ox + 1.0, oy, oz,       // 1  (+,-,-)
            ox + 1.0, oy + 1.0, oz, // 2  (+,+,-)
            ox, oy + 1.0, oz,       // 3  (-,+,-)
            ox, oy, oz + 1.0,             // 4  (-,-,+)
            ox + 1.0, oy, oz + 1.0,       // 5  (+,-,+)
            ox + 1.0, oy + 1.0, oz + 1.0, // 6  (+,+,+)
            ox, oy + 1.0, oz + 1.0,       // 7  (-,+,+)
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
}
