//! Per-mesh point-cloud sampling — area-weighted uniform on the
//! triangle surface, deterministic from a u64 seed.
//!
//! Designed for synthetic training-data generation: turn an IFC product
//! into a labeled point cloud with surface normals, ready for a
//! PointNet / PointNet++ / RandLA-Net classifier. The Python wrapper
//! (`m.point_cloud(per_m2, seed)`) calls this once per ProductMesh
//! during the streaming mesh pass and tags every emitted point with
//! the product's GUID + entity + normalized class.
//!
//! Sampling math is the standard one — pick a triangle weighted by
//! its area, then sample uniformly inside the triangle via
//! barycentric `(1-√r1, √r1·(1-r2), √r1·r2)`. The PRNG is xorshift64
//! (deterministic, no external dep) so a `(file, per_m2, seed)`
//! triple reproduces exactly. That matters for ML training: bit-
//! identical synthetic data across runs is a hard requirement for
//! debugging classifier regressions.

/// Output buffer for one product's sampled points. Parallel-arrays
/// layout so the consumer (PyO3 layer) hands each column straight to
/// `PyList::new` without re-shuffling.
#[derive(Debug, Default)]
pub struct PointCloud {
    pub x: Vec<f32>,
    pub y: Vec<f32>,
    pub z: Vec<f32>,
    pub nx: Vec<f32>,
    pub ny: Vec<f32>,
    pub nz: Vec<f32>,
}

impl PointCloud {
    pub fn len(&self) -> usize {
        self.x.len()
    }

    pub fn is_empty(&self) -> bool {
        self.x.is_empty()
    }
}

/// Minimal xorshift64 PRNG. Deterministic from a u64 seed, fixed
/// period, no dep on `rand` crate. Two `f32` outputs per sample
/// point — fast enough to keep this from showing up in profiles.
struct XorShift64 {
    state: u64,
}

impl XorShift64 {
    fn new(seed: u64) -> Self {
        Self {
            // 0 is a fixed-point; bump to 1 to keep the stream alive.
            state: if seed == 0 { 1 } else { seed },
        }
    }

    #[inline(always)]
    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    /// 24-bit-mantissa f32 in `[0, 1)`. Top 24 bits of the u64.
    #[inline(always)]
    fn next_f32(&mut self) -> f32 {
        const SCALE: f32 = 1.0 / ((1u32 << 24) as f32);
        ((self.next_u64() >> 40) as u32) as f32 * SCALE
    }
}

/// Sample points uniformly on the surface of a triangle mesh.
///
/// - `vertices` / `indices`: world-coord triangle soup, same shape as
///   `ProductMesh.vertices` / `.indices`.
/// - `area_scale`: multiply triangle-area (in source-unit²) by this to
///   get area in m². Same `unit_scale²` factor `mesh::qto` already
///   uses; pass `1.0` if vertices are already in metres.
/// - `per_m2`: target sample density. Total points per product is
///   `(per_m2 * surface_area_m2).ceil()`.
/// - `seed`: deterministic PRNG seed. Identical `(vertices, indices,
///   per_m2, seed)` produces bit-identical output across runs.
///
/// Returns an empty `PointCloud` for degenerate inputs (no triangles
/// or zero surface area). Each emitted point carries the face normal
/// of the triangle it was sampled from.
pub fn sample(
    vertices: &[f32],
    indices: &[u32],
    area_scale: f32,
    per_m2: f32,
    seed: u64,
) -> PointCloud {
    let mut out = PointCloud::default();
    if indices.len() < 3 || vertices.len() < 9 || per_m2 <= 0.0 {
        return out;
    }

    // Two-pass: first sum triangle areas (and remember each), then
    // sample. Sampling needs a CDF anyway; building it during pass 1
    // gives us total_area for the point-count target.
    let n_tris = indices.len() / 3;
    let mut tri_areas: Vec<f32> = Vec::with_capacity(n_tris);
    let mut total_area_raw: f32 = 0.0;

    for tri in indices.chunks_exact(3) {
        let area_raw = triangle_area_raw(vertices, tri[0], tri[1], tri[2]);
        total_area_raw += area_raw;
        tri_areas.push(area_raw);
    }
    if total_area_raw <= 0.0 {
        return out;
    }

    let total_area_m2 = total_area_raw * area_scale;
    let n_total = (per_m2 * total_area_m2).ceil() as usize;
    if n_total == 0 {
        return out;
    }
    out.x.reserve(n_total);
    out.y.reserve(n_total);
    out.z.reserve(n_total);
    out.nx.reserve(n_total);
    out.ny.reserve(n_total);
    out.nz.reserve(n_total);

    // Cumulative area distribution — `cdf[i]` is the sum through
    // triangle `i` inclusive. Binary search picks a triangle in
    // O(log n_tris) per sample. Keeping CDF in raw units (no scale)
    // is fine since the random pick scales with `total_area_raw`.
    let mut cdf: Vec<f32> = Vec::with_capacity(n_tris);
    let mut acc = 0.0;
    for area in &tri_areas {
        acc += area;
        cdf.push(acc);
    }

    let mut rng = XorShift64::new(seed);
    for _ in 0..n_total {
        let pick = rng.next_f32() * total_area_raw;
        // Binary search for the smallest CDF entry >= pick.
        let tri_idx = cdf
            .binary_search_by(|v| {
                if *v < pick {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Greater
                }
            })
            .unwrap_or_else(|i| i.min(n_tris - 1));

        let base = tri_idx * 3;
        let a = indices[base] as usize;
        let b = indices[base + 1] as usize;
        let c = indices[base + 2] as usize;

        let (ax, ay, az) = (
            vertices[a * 3],
            vertices[a * 3 + 1],
            vertices[a * 3 + 2],
        );
        let (bx, by, bz) = (
            vertices[b * 3],
            vertices[b * 3 + 1],
            vertices[b * 3 + 2],
        );
        let (cx, cy, cz) = (
            vertices[c * 3],
            vertices[c * 3 + 1],
            vertices[c * 3 + 2],
        );

        // Barycentric sample via the standard sqrt-trick. Uniform
        // inside the triangle iff r1, r2 are iid U[0,1).
        let r1 = rng.next_f32();
        let r2 = rng.next_f32();
        let sr1 = r1.sqrt();
        let w_a = 1.0 - sr1;
        let w_b = sr1 * (1.0 - r2);
        let w_c = sr1 * r2;

        let px = w_a * ax + w_b * bx + w_c * cx;
        let py = w_a * ay + w_b * by + w_c * cy;
        let pz = w_a * az + w_b * bz + w_c * cz;
        out.x.push(px);
        out.y.push(py);
        out.z.push(pz);

        // Per-point normal = the triangle's face normal. Computed
        // once per sample (cheaper than caching all face normals
        // upfront for memory-light products; the cross + normalize
        // is sub-100ns).
        let ux = bx - ax;
        let uy = by - ay;
        let uz = bz - az;
        let vx = cx - ax;
        let vy = cy - ay;
        let vz = cz - az;
        let nrx = uy * vz - uz * vy;
        let nry = uz * vx - ux * vz;
        let nrz = ux * vy - uy * vx;
        let mag = (nrx * nrx + nry * nry + nrz * nrz).sqrt();
        if mag > 1e-12 {
            let inv = 1.0 / mag;
            out.nx.push(nrx * inv);
            out.ny.push(nry * inv);
            out.nz.push(nrz * inv);
        } else {
            out.nx.push(0.0);
            out.ny.push(0.0);
            out.nz.push(0.0);
        }
    }

    out
}

#[inline]
fn triangle_area_raw(verts: &[f32], a: u32, b: u32, c: u32) -> f32 {
    let (a, b, c) = (a as usize, b as usize, c as usize);
    let off_a = a * 3;
    let off_b = b * 3;
    let off_c = c * 3;
    if off_c + 2 >= verts.len() {
        return 0.0;
    }
    let ax = verts[off_a];
    let ay = verts[off_a + 1];
    let az = verts[off_a + 2];
    let bx = verts[off_b];
    let by = verts[off_b + 1];
    let bz = verts[off_b + 2];
    let cx = verts[off_c];
    let cy = verts[off_c + 1];
    let cz = verts[off_c + 2];
    let ux = bx - ax;
    let uy = by - ay;
    let uz = bz - az;
    let vx = cx - ax;
    let vy = cy - ay;
    let vz = cz - az;
    let nx = uy * vz - uz * vy;
    let ny = uz * vx - ux * vz;
    let nz = ux * vy - uy * vx;
    0.5 * (nx * nx + ny * ny + nz * nz).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unit square at z=0, two triangles, total area 1.0 m².
    fn unit_square() -> (Vec<f32>, Vec<u32>) {
        let v = vec![
            0.0, 0.0, 0.0,
            1.0, 0.0, 0.0,
            1.0, 1.0, 0.0,
            0.0, 1.0, 0.0,
        ];
        let i = vec![0, 1, 2, 0, 2, 3];
        (v, i)
    }

    #[test]
    fn density_matches_per_m2() {
        let (v, i) = unit_square();
        // 1.0 m² area × 100 points/m² = 100 points.
        let pc = sample(&v, &i, 1.0, 100.0, 42);
        assert_eq!(pc.len(), 100);
        assert_eq!(pc.x.len(), pc.y.len());
        assert_eq!(pc.x.len(), pc.z.len());
        assert_eq!(pc.x.len(), pc.nx.len());
    }

    #[test]
    fn points_lie_on_unit_square() {
        let (v, i) = unit_square();
        let pc = sample(&v, &i, 1.0, 50.0, 7);
        for k in 0..pc.len() {
            assert!(pc.x[k] >= 0.0 && pc.x[k] <= 1.0);
            assert!(pc.y[k] >= 0.0 && pc.y[k] <= 1.0);
            // All triangles lie at z=0; sampled points must too.
            assert!(pc.z[k].abs() < 1e-5);
        }
    }

    #[test]
    fn normals_match_face_normal() {
        let (v, i) = unit_square();
        let pc = sample(&v, &i, 1.0, 20.0, 1);
        // Both triangles wound CCW from +Z → all normals = (0, 0, 1).
        for k in 0..pc.len() {
            assert!(
                (pc.nz[k] - 1.0).abs() < 1e-5,
                "expected nz=1, got {} at sample {}",
                pc.nz[k], k,
            );
            assert!(pc.nx[k].abs() < 1e-5);
            assert!(pc.ny[k].abs() < 1e-5);
        }
    }

    #[test]
    fn deterministic_same_seed_same_output() {
        let (v, i) = unit_square();
        let a = sample(&v, &i, 1.0, 50.0, 42);
        let b = sample(&v, &i, 1.0, 50.0, 42);
        assert_eq!(a.x, b.x);
        assert_eq!(a.y, b.y);
        assert_eq!(a.z, b.z);
        assert_eq!(a.nx, b.nx);
    }

    #[test]
    fn different_seeds_diverge() {
        let (v, i) = unit_square();
        let a = sample(&v, &i, 1.0, 50.0, 1);
        let b = sample(&v, &i, 1.0, 50.0, 2);
        // At 50 points it's vanishingly unlikely two seeds produce
        // identical xorshift streams. Allow ties on individual
        // samples but require some divergence overall.
        let mut diffs = 0;
        for k in 0..a.len() {
            if (a.x[k] - b.x[k]).abs() > 1e-6 {
                diffs += 1;
            }
        }
        assert!(diffs > a.len() / 4, "expected diverging outputs, got {diffs}/{}", a.len());
    }

    #[test]
    fn empty_input_returns_empty_cloud() {
        let pc = sample(&[], &[], 1.0, 100.0, 42);
        assert_eq!(pc.len(), 0);
    }

    #[test]
    fn zero_density_returns_empty_cloud() {
        let (v, i) = unit_square();
        let pc = sample(&v, &i, 1.0, 0.0, 42);
        assert_eq!(pc.len(), 0);
    }

    #[test]
    fn area_scale_propagates_to_point_count() {
        // Same square, but pretend it was authored in mm (area scale
        // factor 0.001² = 1e-6). At 1000 pts/m² the total points
        // becomes ceil(1000 * 1e-6) = 1.
        let (v, i) = unit_square();
        let pc = sample(&v, &i, 0.001 * 0.001, 1000.0, 42);
        assert_eq!(pc.len(), 1);
    }

    #[test]
    fn area_weighted_distribution_favours_bigger_triangles() {
        // Two triangles: a small one (area 0.5) and a much bigger
        // one (area 50). The bigger triangle should receive ~99% of
        // the sample mass.
        let v = vec![
            0.0, 0.0, 0.0,
            1.0, 0.0, 0.0,
            0.0, 1.0, 0.0,
            // Second, bigger triangle 10 m away, area = 50
            10.0, 0.0, 0.0,
            20.0, 0.0, 0.0,
            10.0, 10.0, 0.0,
        ];
        let i = vec![0, 1, 2, 3, 4, 5];
        let pc = sample(&v, &i, 1.0, 1000.0, 42);
        let small_count = (0..pc.len())
            .filter(|&k| pc.x[k] < 5.0 && pc.y[k] < 5.0)
            .count();
        let big_count = pc.len() - small_count;
        // small_area / total_area = 0.5 / 50.5 ≈ 0.99% → small should
        // get ~1% of points. Generous bounds for f32 noise.
        let small_frac = small_count as f32 / pc.len() as f32;
        assert!(
            small_frac < 0.05,
            "small triangle (1% of area) got {}% of points",
            small_frac * 100.0
        );
        assert!(big_count > pc.len() * 9 / 10);
    }
}
