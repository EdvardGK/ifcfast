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
    if vertices.is_empty() || !vertices.len().is_multiple_of(3) {
        return Err(CsgKernelError::InvalidVertexBuffer);
    }
    if indices.is_empty() || !indices.len().is_multiple_of(3) {
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
/// manifold unions the cutters, then subtracts the union once. That is
/// also what makes overlapping / touching cutters (the items of one
/// multi-item opening, GH #232) correct: each must be its OWN entry in
/// `cutters`, never concatenated into one self-overlapping mesh.
///
/// All meshes must be in the same coordinate frame (world or local,
/// the kernel doesn't care, but they must agree). All values are
/// treated as `f32`; manifold internally promotes to `f64` for the
/// CSG and we decode back to `f32` on the way out.
///
/// Returns `Ok((vertices, indices))` on success; the result may be
/// empty (zero triangles) if the cutters fully consume the host. The
/// result is Manifold's output as-is — see [`subtract_many_clean`] for
/// the opening-cut variant that strips zero-thickness residue.
pub fn subtract_many(
    host_vertices: &[f32],
    host_indices: &[u32],
    cutters: &[(&[f32], &[u32])],
) -> Result<(Vec<f32>, Vec<u32>), CsgKernelError> {
    let (host, all) = build_operands(host_vertices, host_indices, cutters)?;
    if all.is_empty() {
        return Ok(manifold_to_buffers(&host));
    }
    Ok(manifold_to_buffers(&checked_batch_difference(&all)?))
}

/// [`subtract_many`] plus the GH #233 residue cleanup — the kernel of the
/// opening cut (`cut_openings`: in-representation DIFFERENCE operands and
/// `IfcRelVoidsElement` openings).
///
/// Where a cutter face is flush with a host face (opening top = wall top,
/// opening end = wall end), Manifold's DIFFERENCE keeps a zero-thickness
/// sheet of the host — f32 rounding puts the two faces ~1e-7 · size
/// apart. Two shapes, two remedies, both scale-relative
/// (`floor = RESIDUE_THICKNESS_REL · host bbox diagonal`):
///
/// 1. a **detached** residue component (mean thickness `2V/A ≤ floor`) is
///    dropped — and when nothing else is left the host was consumed and
///    the result is empty;
/// 2. a sheet still **attached** to the kept solid (one component, so (1)
///    cannot see it; signature: coplanar, opposite-facing triangles)
///    triggers one retry in which only the cutter faces flush with a host
///    face — same plane and orientation within `floor` — are pushed
///    outward by `2·floor`. Every other cutter face stays in its plane, so
///    no genuine reveal moves.
///
/// A result with neither shape is returned exactly as [`subtract_many`]
/// returns it.
pub fn subtract_many_clean(
    host_vertices: &[f32],
    host_indices: &[u32],
    cutters: &[(&[f32], &[u32])],
) -> Result<(Vec<f32>, Vec<u32>), CsgKernelError> {
    let (host, all) = build_operands(host_vertices, host_indices, cutters)?;
    if all.is_empty() {
        return Ok(manifold_to_buffers(&host));
    }
    let floor = RESIDUE_THICKNESS_REL * bbox_diagonal(host_vertices);
    let result = drop_residue_components(checked_batch_difference(&all)?, floor);
    let (verts, idx) = manifold_to_buffers(&result);
    if floor > 0.0 && has_coplanar_sheet(&verts, &idx, floor) {
        let planes = host_planes(host_vertices, host_indices, floor);
        let mut grown: Vec<Manifold> = Vec::with_capacity(cutters.len() + 1);
        grown.push(host);
        for (v, i) in cutters {
            grown.push(build_manifold(&push_flush_faces(v, i, &planes, floor), i)?);
        }
        // A rejected retry keeps the first result (sheet and all) rather
        // than failing a cut that did succeed.
        if let Ok(r) = checked_batch_difference(&grown) {
            return Ok(manifold_to_buffers(&drop_residue_components(r, floor)));
        }
    }
    Ok((verts, idx))
}

/// Build the host and every cutter up front so any per-mesh validation
/// failure surfaces before the operation. Returns `(host, operands)` where
/// `operands = [host, cutter…]`, or an empty `operands` when there are no
/// cutters (the caller then echoes the host — round-tripped through
/// manifold so the topology is normalised the same way as a cut).
fn build_operands(
    host_vertices: &[f32],
    host_indices: &[u32],
    cutters: &[(&[f32], &[u32])],
) -> Result<(Manifold, Vec<Manifold>), CsgKernelError> {
    let host = build_manifold(host_vertices, host_indices)?;
    if cutters.is_empty() {
        return Ok((host, Vec::new()));
    }
    let mut all: Vec<Manifold> = Vec::with_capacity(cutters.len() + 1);
    all.push(host.clone());
    for (v, i) in cutters {
        all.push(build_manifold(v, i)?);
    }
    Ok((host, all))
}

/// `manifold[0] − (manifold[1] ∪ manifold[2] ∪ …)`, with the GH #160 status
/// check.
fn checked_batch_difference(all: &[Manifold]) -> Result<Manifold, CsgKernelError> {
    let result = Manifold::batch_difference(all);
    // GH #160: manifold's batch ops are infallible in the type system —
    // a rejected boolean comes back as a valid-looking handle carrying a
    // non-`NoError` status and decodes to ZERO triangles. Unchecked, the
    // caller reads that as "the cutters consumed the host" and flushes an
    // empty mesh over a real wall. Query the status and surface the
    // failure so `cut_openings` can classify it as a Fallback instead.
    // (An *empty* result with `NoError` is still legitimate — a cutter
    // genuinely can consume its host — so emptiness alone is not an
    // error.) The enum is `#[repr(C)]` and field-less; `NoError` is
    // discriminant 0 and the type itself is not re-exported by
    // `manifold-csg`, hence the integer comparison.
    let status = result.status();
    if (status as i32) != 0 {
        return Err(CsgKernelError::ManifoldRejected(CsgError::ManifoldStatus(
            status,
        )));
    }
    Ok(result)
}

/// Mean-thickness floor for a cut-result component, as a fraction of the
/// host's bounding-box diagonal (GH #233). A component whose
/// `2·|volume| / surface_area` is below `RESIDUE_THICKNESS_REL · diag` is
/// a zero-thickness residue, not material; the same length is the outward
/// cutter offset of the attached-sheet retry.
///
/// Calibration (HI90_ARK_MMI700 / G55_ARK): the residues Manifold leaves
/// where a cutter face is flush with a host face measure ~1e-7 · diag
/// (`1o1uaQnuP0xvnggRxTxvO0`: 2V/A = 8.5e-7 m on an 8 m host) — f32
/// rounding noise. The thinnest genuine remnant the 1e-5 floor could still
/// drop is 0.1 mm thick on a 10 m host; no building element is.
pub const RESIDUE_THICKNESS_REL: f64 = 1e-5;

/// Diagonal of the axis-aligned bounding box of a flat vertex buffer.
fn bbox_diagonal(vertices: &[f32]) -> f64 {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for c in vertices.as_chunks::<3>().0 {
        for k in 0..3 {
            let x = f64::from(c[k]);
            lo[k] = lo[k].min(x);
            hi[k] = hi[k].max(x);
        }
    }
    if !lo[0].is_finite() {
        return 0.0;
    }
    ((hi[0] - lo[0]).powi(2) + (hi[1] - lo[1]).powi(2) + (hi[2] - lo[2]).powi(2)).sqrt()
}

/// `true` if a connected component carries no material: its mean
/// thickness `2·|V| / A` is at or below `thickness_floor` (or it has no
/// area).
fn is_residue(component: &Manifold, thickness_floor: f64) -> bool {
    let area = component.surface_area();
    if area <= 0.0 {
        return true;
    }
    2.0 * component.volume().abs() <= thickness_floor * area
}

/// GH #233: drop the zero-thickness residue components a DIFFERENCE
/// leaves where a cutter face is coplanar with a host face (opening top
/// = wall top, opening end = wall end). Manifold returns them as valid,
/// closed, edge-balanced pieces of ~zero volume — invisible to the volume
/// and to `mesh_quality`, but they stretch the AABB / centroid by metres
/// and turn a host its openings fully consume into a phantom "closed"
/// sliver. ifcopenshell drops them; so do we.
///
/// The result is returned untouched (same handle, byte-identical mesh)
/// unless at least one component is residue. If every component is
/// residue the host was consumed: the result is empty.
pub fn drop_residue_components(result: Manifold, thickness_floor: f64) -> Manifold {
    if result.is_empty() || thickness_floor <= 0.0 {
        return result;
    }
    let parts = result.decompose();
    if !parts.iter().any(|p| is_residue(p, thickness_floor)) {
        return result;
    }
    let keep: Vec<Manifold> = parts
        .into_iter()
        .filter(|p| !is_residue(p, thickness_floor))
        .collect();
    if keep.is_empty() {
        return Manifold::empty();
    }
    Manifold::compose(&keep)
}

fn tri_normal_area(v: &[f32], t: &[u32]) -> ([f64; 3], f64) {
    let p = |k: usize| {
        let b = t[k] as usize * 3;
        [f64::from(v[b]), f64::from(v[b + 1]), f64::from(v[b + 2])]
    };
    let (a, b, c) = (p(0), p(1), p(2));
    let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let w = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let n = [
        u[1] * w[2] - u[2] * w[1],
        u[2] * w[0] - u[0] * w[2],
        u[0] * w[1] - u[1] * w[0],
    ];
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if len == 0.0 {
        return ([0.0; 3], 0.0);
    }
    ([n[0] / len, n[1] / len, n[2] / len], 0.5 * len)
}

/// Sheet signature (GH #233): two triangles on the same plane (normal to
/// ~1e-3, offset within `tol`) facing opposite ways, each with real area.
/// A solid never carries that — material would have to be zero-thick
/// between them.
fn has_coplanar_sheet(vertices: &[f32], indices: &[u32], tol: f64) -> bool {
    use std::collections::HashMap;
    let min_area = tol * tol;
    // key: canonical normal (quantised) + plane-offset bucket → orientation bits
    let mut seen: HashMap<([i32; 3], i64), u8> = HashMap::new();
    for t in indices.as_chunks::<3>().0 {
        let (n, area) = tri_normal_area(vertices, t);
        if area <= min_area {
            continue;
        }
        // canonical sign: first non-negligible component positive
        let flip = n.iter().find(|c| c.abs() > 1e-6).is_some_and(|c| *c < 0.0);
        let (cn, bit) = if flip {
            ([-n[0], -n[1], -n[2]], 2u8)
        } else {
            (n, 1u8)
        };
        let b = t[0] as usize * 3;
        let d = cn[0] * f64::from(vertices[b])
            + cn[1] * f64::from(vertices[b + 1])
            + cn[2] * f64::from(vertices[b + 2]);
        let q = [
            (cn[0] * 1e3).round() as i32,
            (cn[1] * 1e3).round() as i32,
            (cn[2] * 1e3).round() as i32,
        ];
        let bucket = (d / (2.0 * tol)).floor() as i64;
        for nb in [bucket - 1, bucket, bucket + 1] {
            if let Some(bits) = seen.get(&(q, nb)) {
                if bits & !bit & 3 != 0 {
                    return true;
                }
            }
        }
        *seen.entry((q, bucket)).or_insert(0) |= bit;
    }
    false
}

/// Distinct planes `(unit normal, offset)` of a mesh's faces, merged
/// within `tol` (normal dot ≥ 0.9999, offset within `tol`).
fn host_planes(vertices: &[f32], indices: &[u32], tol: f64) -> Vec<([f64; 3], f64)> {
    let mut planes: Vec<([f64; 3], f64)> = Vec::new();
    for t in indices.as_chunks::<3>().0 {
        let (n, area) = tri_normal_area(vertices, t);
        if area == 0.0 {
            continue;
        }
        let d = plane_offset(vertices, t[0], n);
        if !planes
            .iter()
            .any(|(m, e)| dot(*m, n) > 0.9999 && (e - d).abs() <= tol)
        {
            planes.push((n, d));
        }
    }
    planes
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn plane_offset(vertices: &[f32], vi: u32, n: [f64; 3]) -> f64 {
    let b = vi as usize * 3;
    n[0] * f64::from(vertices[b])
        + n[1] * f64::from(vertices[b + 1])
        + n[2] * f64::from(vertices[b + 2])
}

/// GH #233 overshoot, targeted: push every cutter face that lies on a host
/// face plane with the same orientation (offset within `tol`) outward by
/// `2·tol`; leave every other face in its plane. Each vertex moves by the
/// least-squares `d` with `d · n = 2·tol` for its flush faces and
/// `d · n = 0` for its other faces — exact for the ≤ 3-plane corners of
/// extruded openings — capped at `8·tol`. Vertices are grouped by exact
/// position so duplicated corners move together; topology is unchanged.
fn push_flush_faces(
    vertices: &[f32],
    indices: &[u32],
    host_planes: &[([f64; 3], f64)],
    tol: f64,
) -> Vec<f32> {
    use std::collections::HashMap;
    let push = 2.0 * tol;
    let key = |i: usize| {
        let b = i * 3;
        [
            vertices[b].to_bits(),
            vertices[b + 1].to_bits(),
            vertices[b + 2].to_bits(),
        ]
    };
    // per vertex position: distinct (normal, target) constraints
    let mut cons: HashMap<[u32; 3], Vec<([f64; 3], f64)>> = HashMap::new();
    let mut any_flush = false;
    for t in indices.as_chunks::<3>().0 {
        let (n, area) = tri_normal_area(vertices, t);
        if area == 0.0 {
            continue;
        }
        let d = plane_offset(vertices, t[0], n);
        // Same orientation only: the cutter reaches the host boundary from
        // INSIDE. An opposite-facing coplanar face is a cutter touching the
        // host from outside — pushing it "outward" would bite into the host.
        let flush = host_planes
            .iter()
            .any(|(m, e)| dot(*m, n) > 0.9999 && (e - d).abs() <= tol);
        any_flush |= flush;
        let target = if flush { push } else { 0.0 };
        for &vi in t {
            let cs = cons.entry(key(vi as usize)).or_default();
            match cs.iter_mut().find(|(m, _)| dot(*m, n) > 0.9999) {
                // a flush face wins over a coplanar non-flush sliver
                Some(c) => c.1 = c.1.max(target),
                None => cs.push((n, target)),
            }
        }
    }
    let mut out = vertices.to_vec();
    if !any_flush {
        return out;
    }
    for (vi, c) in out.as_chunks_mut::<3>().0.iter_mut().enumerate() {
        let Some(cs) = cons.get(&key(vi)) else {
            continue;
        };
        if !cs.iter().any(|(_, t)| *t > 0.0) {
            continue;
        }
        let d = solve_offset(cs, 4.0 * push);
        for k in 0..3 {
            c[k] = (f64::from(c[k]) + d[k]) as f32;
        }
    }
    out
}

/// Least-squares `d` with `d · n_i = t_i` (minimum-norm via a tiny ridge),
/// capped at length `cap`.
fn solve_offset(cs: &[([f64; 3], f64)], cap: f64) -> [f64; 3] {
    let mut a = [[0.0f64; 3]; 3];
    let mut b = [0.0f64; 3];
    for (n, t) in cs {
        for r in 0..3 {
            for c in 0..3 {
                a[r][c] += n[r] * n[c];
            }
            b[r] += t * n[r];
        }
    }
    for (r, row) in a.iter_mut().enumerate() {
        row[r] += 1e-9;
    }
    let det3 = |m: &[[f64; 3]; 3]| {
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    };
    let det = det3(&a);
    if det.abs() < 1e-18 {
        return [0.0; 3];
    }
    let col = |k: usize| {
        let mut m = a;
        for r in 0..3 {
            m[r][k] = b[r];
        }
        det3(&m) / det
    };
    let mut d = [col(0), col(1), col(2)];
    let len = dot(d, d).sqrt();
    if len > cap {
        let s = cap / len;
        d = [d[0] * s, d[1] * s, d[2] * s];
    }
    d
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
            ox,
            oy,
            oz,
            ox + 1.0,
            oy,
            oz,
            ox + 1.0,
            oy + 1.0,
            oz,
            ox,
            oy + 1.0,
            oz,
            ox,
            oy,
            oz + 1.0,
            ox + 1.0,
            oy,
            oz + 1.0,
            ox + 1.0,
            oy + 1.0,
            oz + 1.0,
            ox,
            oy + 1.0,
            oz + 1.0,
        ];
        let i: Vec<u32> = vec![
            0, 2, 1, 0, 3, 2, 4, 5, 6, 4, 6, 7, 0, 1, 5, 0, 5, 4, 2, 3, 7, 2, 7, 6, 1, 2, 6, 1, 6,
            5, 0, 4, 7, 0, 7, 3,
        ];
        (v, i)
    }

    fn box_at(min: [f32; 3], max: [f32; 3]) -> (Vec<f32>, Vec<u32>) {
        let v: Vec<f32> = vec![
            min[0], min[1], min[2], max[0], min[1], min[2], max[0], max[1], min[2], min[0], max[1],
            min[2], min[0], min[1], max[2], max[0], min[1], max[2], max[0], max[1], max[2], min[0],
            max[1], max[2],
        ];
        let i: Vec<u32> = vec![
            0, 2, 1, 0, 3, 2, 4, 5, 6, 4, 6, 7, 0, 1, 5, 0, 5, 4, 2, 3, 7, 2, 7, 6, 1, 2, 6, 1, 6,
            5, 0, 4, 7, 0, 7, 3,
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
        let (verts, idx) = subtract_many(&host.0, &host.1, &[(&o1.0, &o1.1), (&o2.0, &o2.1)])
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

    fn volume_of(v: &[f32], i: &[u32]) -> f64 {
        if i.is_empty() {
            return 0.0;
        }
        build_manifold(v, i).expect("result is manifold").volume()
    }

    fn max_axis(v: &[f32], k: usize) -> f32 {
        v.as_chunks::<3>()
            .0
            .iter()
            .map(|c| c[k])
            .fold(f32::NEG_INFINITY, f32::max)
    }

    /// GH #232: an opening authored as two touching items (Archicad's
    /// window opening + sill notch) subtracted as two cutters equals the
    /// same opening authored as one item.
    #[test]
    fn two_item_opening_equals_one_item_opening() {
        let host = box_at([0.0, 0.0, 0.0], [4.0, 0.3, 3.0]);
        let a = box_at([1.0, -0.1, 1.0], [2.0, 0.4, 2.0]);
        let b = box_at([1.0, -0.1, 2.0], [2.0, 0.4, 2.5]);
        let whole = box_at([1.0, -0.1, 1.0], [2.0, 0.4, 2.5]);
        let (v1, i1) = subtract_many(&host.0, &host.1, &[(&a.0, &a.1), (&b.0, &b.1)]).unwrap();
        let (v2, i2) = subtract(&host.0, &host.1, &whole.0, &whole.1).unwrap();
        let expected = 4.0 * 0.3 * 3.0 - 1.0 * 0.3 * 1.5;
        assert!((volume_of(&v1, &i1) - expected).abs() < 1e-5);
        assert!((volume_of(&v2, &i2) - expected).abs() < 1e-5);
    }

    /// GH #232: overlapping (not just touching) items are equally fine as
    /// separate cutters — the kernel subtracts their union.
    #[test]
    fn overlapping_opening_items_subtract_their_union() {
        let host = box_at([0.0, 0.0, 0.0], [4.0, 0.3, 3.0]);
        let a = box_at([1.0, -0.1, 1.0], [2.0, 0.4, 2.01]);
        let b = box_at([1.0, -0.1, 2.0], [2.0, 0.4, 2.5]);
        let (v, i) = subtract_many(&host.0, &host.1, &[(&a.0, &a.1), (&b.0, &b.1)]).unwrap();
        let expected = 4.0 * 0.3 * 3.0 - 1.0 * 0.3 * 1.5;
        assert!((volume_of(&v, &i) - expected).abs() < 1e-5);
    }

    /// A cutter face "flush" with the host face x = 1 up to a gap of
    /// 5e-6 · size — the gap rebased f32 openings land with, and thick
    /// enough that Manifold's own epsilon does not collapse it (a 1-ulp
    /// gap it does). Below the `RESIDUE_THICKNESS_REL · diag` floor.
    const GAP: f32 = 5e-6;
    const NEAR_ONE: f32 = 1.0 - GAP;

    /// GH #233: a cutter face one ulp short of the host face leaves a
    /// detached zero-thickness slab; it must be dropped, not emitted.
    #[test]
    fn coplanar_cut_leaves_no_detached_residue() {
        let host = box_at([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        let cutter = box_at([0.5, -0.1, -0.1], [NEAR_ONE, 1.1, 1.1]);
        let (v, i) = subtract_many_clean(&host.0, &host.1, &[(&cutter.0, &cutter.1)]).unwrap();
        assert!((volume_of(&v, &i) - 0.5).abs() < 1e-5);
        assert!(
            max_axis(&v, 0) <= 0.5 + 1e-5,
            "residue slab survived at x = 1"
        );
        assert_eq!(i.len(), 36, "result is the single kept box");
    }

    /// GH #233: the same flush-face slab, but ATTACHED to the kept solid
    /// along an edge (one connected component) — the overshoot retry must
    /// remove it.
    #[test]
    fn coplanar_cut_leaves_no_attached_sheet() {
        let host = box_at([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        let cutter = box_at([-0.1, -0.1, 0.5], [NEAR_ONE, 1.1, 1.1]);
        let (v, i) = subtract_many_clean(&host.0, &host.1, &[(&cutter.0, &cutter.1)]).unwrap();
        assert!((volume_of(&v, &i) - 0.5).abs() < 1e-4);
        assert!(
            max_axis(&v, 2) <= 0.5 + 1e-4,
            "sheet survived above z = 0.5"
        );
        assert!(!has_coplanar_sheet(&v, &i, 1e-5));
    }

    /// GH #233: a host its cutter consumes up to f32 rounding comes back
    /// empty, not as a "closed" sliver.
    #[test]
    fn fully_consumed_host_is_empty() {
        let host = box_at([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        let cutter = box_at([GAP, -0.1, -0.1], [NEAR_ONE, 1.1, 1.1]);
        let (_v, i) = subtract_many_clean(&host.0, &host.1, &[(&cutter.0, &cutter.1)]).unwrap();
        assert!(
            i.is_empty(),
            "expected empty, got {} triangles",
            i.len() / 3
        );
    }

    /// Only the cutter faces flush with a host face move; the rest stay.
    #[test]
    fn push_flush_faces_moves_only_flush_faces() {
        let host = box_at([0.0, 0.0, 0.0], [4.0, 0.3, 3.0]);
        // opening flush with the wall top (z = 3) — through the thickness
        let cutter = box_at([1.0, -0.1, 1.0], [2.0, 0.4, 3.0]);
        let planes = host_planes(&host.0, &host.1, 1e-4);
        let g = push_flush_faces(&cutter.0, &cutter.1, &planes, 1e-4);
        // top face z = 3 → 3.0002; everything else unchanged
        for (a, b) in cutter.0.as_chunks::<3>().0.iter().zip(g.as_chunks::<3>().0) {
            assert_eq!(a[0], b[0]);
            assert_eq!(a[1], b[1]);
            if a[2] == 3.0 {
                assert!((b[2] - 3.0002).abs() < 1e-6, "flush top moved to {}", b[2]);
            } else {
                assert_eq!(a[2], b[2]);
            }
        }
    }

    /// A cutter face touching the host from OUTSIDE (opposite orientation)
    /// is not flush-from-inside and must not move into the host.
    #[test]
    fn push_flush_faces_ignores_outside_touching_faces() {
        let host = box_at([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        let cutter = box_at([1.0, 0.2, 0.2], [2.0, 0.8, 0.8]); // abuts x = 1
        let planes = host_planes(&host.0, &host.1, 1e-4);
        assert_eq!(
            push_flush_faces(&cutter.0, &cutter.1, &planes, 1e-4),
            cutter.0
        );
    }

    /// The raw kernel is untouched by #233 (the GH #194 bounded-clip
    /// fallback uses it): the detached residue is still there.
    #[test]
    fn raw_subtract_keeps_manifold_output() {
        let host = box_at([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        let cutter = box_at([0.5, -0.1, -0.1], [NEAR_ONE, 1.1, 1.1]);
        let (_v, i) = subtract(&host.0, &host.1, &cutter.0, &cutter.1).unwrap();
        assert!(i.len() > 36, "raw result carries the residue slab");
    }

    #[test]
    fn clean_result_has_no_sheet_signature() {
        let host = box_at([0.0, 0.0, 0.0], [2.0, 1.0, 1.0]);
        let o = box_at([0.25, 0.25, 0.25], [0.75, 0.75, 0.75]);
        let (v, i) = subtract(&host.0, &host.1, &o.0, &o.1).unwrap();
        assert!(!has_coplanar_sheet(&v, &i, 1e-5));
    }

    /// Empty cutter list → echo the host through (round-tripped).
    #[test]
    fn subtract_with_no_cutters_returns_host_volume() {
        let host = box_at([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        let (verts, idx) = subtract_many(&host.0, &host.1, &[]).expect("no-cutter subtract");
        let m = build_manifold(&verts, &idx).expect("result is manifold");
        assert!(
            (m.volume() - 1.0).abs() < 1e-3,
            "expected ≈ 1.0, got {}",
            m.volume()
        );
    }
}
