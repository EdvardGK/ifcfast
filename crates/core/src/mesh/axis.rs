//! Centerline + profile extraction — `axis_from_extrusion`, the second
//! foundational primitive for constraint-aware MEP rerouting (GH #63) and
//! the geometry-hotswap roadmap (profile recognition → centerline →
//! parametric duct/pipe swap). Both features need the same primitive:
//! recover a swept element's *centerline* (the routable axis) and its
//! *cross-section profile* (what must fit through a gap).
//!
//! **Path-A (parametric).** For an `IfcExtrudedAreaSolid` we already have
//! the exact swept description in [`ExtrudeParams`] — profile, direction,
//! depth, local placement — so the centerline is analytic: the profile's
//! area centroid swept from base to `base + dir·depth`. No mesh skeleton
//! needed. A mesh-based Path-B (PCA / medial axis for non-extrusion or
//! tessellation-only runs) is the documented follow-up.
//!
//! **Units.** [`ExtrudeParams`] is source-unit *body-local*: `local_xform`
//! is the `IfcAxis2Placement3D` only, not the product's world
//! `ObjectPlacement`. So this primitive takes the product's `world`
//! placement and the model's `unit_scale` and emits the centerline in
//! **world metres** — the occupancy/reroute boundary unit (GH #63
//! feasibility flag: "the centerline path must apply world placement +
//! unit_scale itself"). Pass a near-origin (rebased) `world` for
//! far-origin models; f32 cannot hold raw UTM magnitudes (see the
//! far-origin precision notes / `prism_csg::rebase_params`).

use glam::{Mat4, Vec2, Vec3};

use crate::mesh::extrusion::ExtrudeParams;
use crate::mesh::profile::Polygon2D;

/// A swept element's routable centerline plus its cross-section.
#[derive(Debug, Clone)]
pub struct Axis {
    /// Centerline as a world-metre polyline. A straight extrusion is two
    /// points `[start, end]`; segmented / curved runs extend this.
    pub polyline: Vec<Vec3>,
    /// Centerline length in metres (sum of polyline segment lengths).
    pub length_m: f32,
    /// Unit centerline direction in world frame (start→end for a straight
    /// run). Zero for a degenerate (zero-length) axis.
    pub dir_world: Vec3,
    /// The swept cross-section, in metres.
    pub profile: ProfileShape,
}

/// A cross-section profile, centroid-centred, in metres.
#[derive(Debug, Clone)]
pub struct ProfileShape {
    /// Outer ring, centroid-relative (metres).
    pub outer: Vec<Vec2>,
    /// Inner rings / voids, centroid-relative (metres).
    pub holes: Vec<Vec<Vec2>>,
    /// Axis-aligned cross-section size `(width, height)` in metres.
    pub bbox_m: Vec2,
    /// Net cross-section area (outer − holes) in m².
    pub area_m2: f32,
}

/// Extract the parametric centerline + profile of an extrusion, in world
/// metres. `world` is the product's `ObjectPlacement` (composed in front
/// of the solid-local `local_xform`); `unit_scale` is the source→metres
/// factor (mm → 0.001, m → 1.0). Returns `None` if the profile has no
/// usable outer ring.
pub fn axis_from_extrusion(
    p: &ExtrudeParams,
    world: Mat4,
    unit_scale: f32,
) -> Option<Axis> {
    if p.profile.outer.len() < 3 {
        return None;
    }
    let us = if unit_scale.is_finite() && unit_scale > 0.0 {
        unit_scale
    } else {
        1.0
    };

    // Profile centroid in the solid-local (pre-local_xform) XY plane.
    let c2d = polygon_centroid(&p.profile.outer);
    let base_local = Vec3::new(c2d.x, c2d.y, 0.0);
    let top_local = base_local + p.dir * p.depth;

    // Compose world ∘ local, transform the two centerline ends, then
    // scale source→metres. Scaling the transformed world position is
    // correct: unit_scale converts *all* lengths, including absolute
    // placement, from source units to metres.
    let m = world * p.local_xform;
    let start = m.transform_point3(base_local) * us;
    let end = m.transform_point3(top_local) * us;

    let span = end - start;
    let length_m = span.length();
    let dir_world = if length_m > 1e-9 {
        span / length_m
    } else {
        Vec3::ZERO
    };

    Some(Axis {
        polyline: vec![start, end],
        length_m,
        dir_world,
        profile: profile_shape(&p.profile, c2d, us),
    })
}

/// Build a centroid-relative, metre-scaled [`ProfileShape`] from a
/// source-unit body-local [`Polygon2D`].
fn profile_shape(poly: &Polygon2D, centroid: Vec2, us: f32) -> ProfileShape {
    let shift = |ring: &[Vec2]| -> Vec<Vec2> {
        ring.iter().map(|v| (*v - centroid) * us).collect()
    };
    let outer = shift(&poly.outer);
    let holes: Vec<Vec<Vec2>> = poly.holes.iter().map(|h| shift(h)).collect();

    let (mut lo, mut hi) = (Vec2::splat(f32::INFINITY), Vec2::splat(f32::NEG_INFINITY));
    for v in &outer {
        lo = lo.min(*v);
        hi = hi.max(*v);
    }
    let bbox_m = if outer.is_empty() {
        Vec2::ZERO
    } else {
        (hi - lo).max(Vec2::ZERO)
    };

    let outer_area = polygon_area(&outer);
    let holes_area: f32 = holes.iter().map(|h| polygon_area(h)).sum();
    let area_m2 = (outer_area - holes_area).max(0.0);

    ProfileShape { outer, holes, bbox_m, area_m2 }
}

/// Area-weighted centroid of a simple polygon ring. Falls back to the
/// vertex mean for a degenerate (near-zero-area) ring.
fn polygon_centroid(ring: &[Vec2]) -> Vec2 {
    let n = ring.len();
    if n == 0 {
        return Vec2::ZERO;
    }
    let mut a2 = 0.0f32; // 2× signed area
    let mut cx = 0.0f32;
    let mut cy = 0.0f32;
    for i in 0..n {
        let p = ring[i];
        let q = ring[(i + 1) % n];
        let cross = p.x * q.y - q.x * p.y;
        a2 += cross;
        cx += (p.x + q.x) * cross;
        cy += (p.y + q.y) * cross;
    }
    if a2.abs() < 1e-12 {
        // Degenerate ring — mean of vertices.
        let s = ring.iter().fold(Vec2::ZERO, |acc, v| acc + *v);
        return s / n as f32;
    }
    Vec2::new(cx / (3.0 * a2), cy / (3.0 * a2))
}

/// Unsigned area of a simple polygon ring (shoelace).
fn polygon_area(ring: &[Vec2]) -> f32 {
    let n = ring.len();
    if n < 3 {
        return 0.0;
    }
    let mut a2 = 0.0f32;
    for i in 0..n {
        let p = ring[i];
        let q = ring[(i + 1) % n];
        a2 += p.x * q.y - q.x * p.y;
    }
    (a2 * 0.5).abs()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A centred rectangle profile (width × depth, source units).
    fn rect(w: f32, d: f32) -> Polygon2D {
        let (hw, hd) = (w * 0.5, d * 0.5);
        Polygon2D {
            outer: vec![
                Vec2::new(-hw, -hd),
                Vec2::new(hw, -hd),
                Vec2::new(hw, hd),
                Vec2::new(-hw, hd),
            ],
            holes: Vec::new(),
        }
    }

    #[test]
    fn vertical_extrusion_centerline_in_metres() {
        // 200×400 mm column extruded 3000 mm up; mm model.
        let p = ExtrudeParams {
            profile: rect(200.0, 400.0),
            dir: Vec3::Z,
            depth: 3000.0,
            local_xform: Mat4::IDENTITY,
        };
        let ax = axis_from_extrusion(&p, Mat4::IDENTITY, 0.001).unwrap();
        assert!((ax.polyline[0] - Vec3::ZERO).length() < 1e-5);
        assert!((ax.polyline[1] - Vec3::new(0.0, 0.0, 3.0)).length() < 1e-5);
        assert!((ax.length_m - 3.0).abs() < 1e-5, "len {}", ax.length_m);
        assert!((ax.dir_world - Vec3::Z).length() < 1e-5);
        assert!((ax.profile.bbox_m - Vec2::new(0.2, 0.4)).length() < 1e-5);
        assert!((ax.profile.area_m2 - 0.08).abs() < 1e-5, "area {}", ax.profile.area_m2);
    }

    #[test]
    fn world_placement_translates_and_rotates_centerline() {
        // Horizontal duct: local +Z mapped to world +X by a 90° Y-rot,
        // then offset to (5, 2, 3) m-equivalent (5000 mm) in a mm model.
        let rot = Mat4::from_rotation_y(std::f32::consts::FRAC_PI_2);
        let world = Mat4::from_translation(Vec3::new(5000.0, 2000.0, 3000.0)) * rot;
        let p = ExtrudeParams {
            profile: rect(300.0, 300.0),
            dir: Vec3::Z,
            depth: 2000.0,
            local_xform: Mat4::IDENTITY,
        };
        let ax = axis_from_extrusion(&p, world, 0.001).unwrap();
        // Start at the placement origin (5,2,3) m; +Z(local) → +X(world).
        assert!(
            (ax.polyline[0] - Vec3::new(5.0, 2.0, 3.0)).length() < 1e-4,
            "start {:?}",
            ax.polyline[0]
        );
        assert!(
            (ax.polyline[1] - Vec3::new(7.0, 2.0, 3.0)).length() < 1e-4,
            "end {:?}",
            ax.polyline[1]
        );
        assert!((ax.length_m - 2.0).abs() < 1e-4);
        assert!((ax.dir_world - Vec3::X).length() < 1e-4, "dir {:?}", ax.dir_world);
    }

    #[test]
    fn hollow_profile_area_subtracts_holes() {
        // 1000×1000 mm outer, 800×800 mm hole → net 1.0 - 0.64 = 0.36 m².
        let mut poly = rect(1000.0, 1000.0);
        let (hw, hd) = (400.0, 400.0);
        poly.holes = vec![vec![
            Vec2::new(-hw, -hd),
            Vec2::new(hw, -hd),
            Vec2::new(hw, hd),
            Vec2::new(-hw, hd),
        ]];
        let p = ExtrudeParams { profile: poly, dir: Vec3::Z, depth: 1000.0, local_xform: Mat4::IDENTITY };
        let ax = axis_from_extrusion(&p, Mat4::IDENTITY, 0.001).unwrap();
        assert!((ax.profile.area_m2 - 0.36).abs() < 1e-4, "area {}", ax.profile.area_m2);
        assert!((ax.profile.bbox_m - Vec2::new(1.0, 1.0)).length() < 1e-5);
    }

    #[test]
    fn degenerate_profile_returns_none() {
        let p = ExtrudeParams {
            profile: Polygon2D { outer: vec![Vec2::ZERO, Vec2::X], holes: Vec::new() },
            dir: Vec3::Z,
            depth: 1000.0,
            local_xform: Mat4::IDENTITY,
        };
        assert!(axis_from_extrusion(&p, Mat4::IDENTITY, 0.001).is_none());
    }
}
