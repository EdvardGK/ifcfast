//! Routing — classic A* pathfinding over the occupancy go/nogo field
//! ([`crate::occupancy`]), shaped by per-system MEP constraints (GH #63).
//!
//! The pipeline the user framed: voxelize → go/nogo → find paths, with the
//! real MEP rules layered onto the search:
//!
//! * **Bend angles: ≤ 90°, and gentler is better.** The grid is searched
//!   **26-connected**, so a step can continue straight, turn 45°, or turn
//!   90°. A turn sharper than 90° (a hairpin) is forbidden, and the
//!   [`SystemConstraints::turn_cost`] is scaled by the bend angle — a 45°
//!   bend costs about half a 90° bend — so the optimal route *prefers
//!   shallow bends* and only takes a right angle when it must.
//! * **Stay at one elevation by default.** Any vertical-containing move is
//!   multiplied by [`SystemConstraints::z_cost_mult`]; with the `Unknown`
//!   profile that penalty is high, so absent better information a route
//!   hugs a single height.
//! * **Per-system Z discipline** ([`ZMode`], selected per [`SystemKind`]):
//!   - `Free` — vertical allowed (penalised); ducts, unknown systems.
//!   - `Planar` — locked to the start layer; pressure systems take no Z
//!     variation.
//!   - `MonotoneDown` — Z may only fall or hold; gravity drainage. A
//!     target "fall" grade (e.g. 1:80) refines monotone descent (follow-up).
//!
//! **System selection.** The constraints are *configured by system*. The
//! system kind is meant to be inferred from IFC properties
//! (`IfcDistributionSystem.PredefinedType`, system-classification Psets)
//! once that wiring exists; until then the caller supplies it, and the
//! honest default is [`SystemKind::Unknown`] → stay-at-elevation.
//!
//! The search is a textbook A* with an admissible 3D-octile heuristic
//! (turn/Z penalties only ever *add* cost, so it never overestimates →
//! optimal paths). Turn cost makes the state `(voxel, incoming-direction)`.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

use glam::Vec3;

use crate::occupancy::Occupancy;

/// Broad MEP system class — selects a [`SystemConstraints`] profile.
/// Inferred from IFC properties when available; `Unknown` otherwise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemKind {
    /// Pressurised pipe (water supply, etc.) — no Z variation.
    PressurePipe,
    /// Gravity drainage — must run downhill.
    GravityDrain,
    /// Ducting / ventilation — level changes allowed, penalised.
    Duct,
    /// Unidentified — stay at one elevation, level changes allowed but
    /// strongly penalised.
    Unknown,
}

/// Per-system vertical-movement discipline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZMode {
    /// Vertical moves allowed (penalised by `z_cost_mult`).
    Free,
    /// Locked to the start voxel's Z layer — pressure systems.
    Planar,
    /// Z may only decrease or stay equal — gravity drainage.
    MonotoneDown,
}

/// Per-system routing constraints.
#[derive(Clone, Copy, Debug)]
pub struct SystemConstraints {
    pub z_mode: ZMode,
    /// Cost multiplier on any vertical-containing step; `> 1` keeps routes
    /// level. High for `Unknown` (stay at elevation when we don't know).
    pub z_cost_mult: f32,
    /// Base cost of a 90° bend (metres-equivalent); scaled down for a 45°
    /// bend, zero for straight. `> 0` minimises and softens elbows.
    pub turn_cost: f32,
}

impl SystemConstraints {
    /// Resolve the constraint profile for a system kind. This is the
    /// *configured-by-system* table; property-based [`SystemKind`]
    /// inference feeds it once the extractor wiring lands.
    pub fn for_kind(kind: SystemKind) -> Self {
        match kind {
            SystemKind::PressurePipe => {
                Self { z_mode: ZMode::Planar, z_cost_mult: 1.0, turn_cost: 0.5 }
            }
            SystemKind::GravityDrain => {
                Self { z_mode: ZMode::MonotoneDown, z_cost_mult: 1.5, turn_cost: 0.7 }
            }
            SystemKind::Duct => Self { z_mode: ZMode::Free, z_cost_mult: 3.0, turn_cost: 0.5 },
            // Don't know the system → keep it level; allow a level change
            // only when the horizontal penalty makes it unavoidable.
            SystemKind::Unknown => {
                Self { z_mode: ZMode::Free, z_cost_mult: 6.0, turn_cost: 0.5 }
            }
        }
    }
}

impl Default for SystemConstraints {
    fn default() -> Self {
        Self::for_kind(SystemKind::Unknown)
    }
}

/// A found route through the go voxels.
#[derive(Clone, Debug)]
pub struct Route {
    /// Voxel coordinates, start..=goal.
    pub voxels: Vec<[usize; 3]>,
    /// World-metre polyline (voxel centres).
    pub polyline: Vec<Vec3>,
    /// Total geometric path length in metres (un-penalised).
    pub length_m: f32,
    /// Count of direction changes (any non-zero bend).
    pub bends: usize,
    /// Largest bend angle on the route, in degrees (≤ 90 by construction).
    pub max_bend_deg: f32,
}

// The 26 axis/diagonal moves (all dx,dy,dz ∈ {-1,0,1} except origin).
fn moves() -> [[i32; 3]; 26] {
    let mut m = [[0i32; 3]; 26];
    let mut n = 0;
    for dx in -1..=1 {
        for dy in -1..=1 {
            for dz in -1..=1 {
                if dx == 0 && dy == 0 && dz == 0 {
                    continue;
                }
                m[n] = [dx, dy, dz];
                n += 1;
            }
        }
    }
    m
}

const NONE_DIR: u8 = 26;

/// Angle (degrees) between two integer move vectors.
fn bend_deg(a: [i32; 3], b: [i32; 3]) -> f32 {
    let av = Vec3::new(a[0] as f32, a[1] as f32, a[2] as f32);
    let bv = Vec3::new(b[0] as f32, b[1] as f32, b[2] as f32);
    let denom = av.length() * bv.length();
    if denom <= 0.0 {
        return 0.0;
    }
    (av.dot(bv) / denom).clamp(-1.0, 1.0).acos().to_degrees()
}

/// Find an MEP-constrained route from `start` to `goal` through the go
/// (free) voxels of `occ`. Returns `None` if an endpoint is occupied /
/// out-of-bounds, or no route exists under the constraints.
pub fn find_path(
    occ: &Occupancy,
    start: [usize; 3],
    goal: [usize; 3],
    c: &SystemConstraints,
) -> Option<Route> {
    let [nx, ny, nz] = occ.grid.dims;
    let in_grid = |v: [usize; 3]| v[0] < nx && v[1] < ny && v[2] < nz;
    if !in_grid(start) || !in_grid(goal) {
        return None;
    }
    if !occ.is_free(start[0], start[1], start[2]) || !occ.is_free(goal[0], goal[1], goal[2]) {
        return None;
    }
    let cell = occ.grid.cell;
    let moves = moves();
    let lin = |v: [usize; 3]| (v[2] * ny + v[1]) * nx + v[0];

    // 3D-octile heuristic in metres — exact unobstructed diagonal cost,
    // admissible because turn / Z penalties only add to the true cost.
    let h = |v: [usize; 3]| -> f32 {
        let mut d = [v[0].abs_diff(goal[0]), v[1].abs_diff(goal[1]), v[2].abs_diff(goal[2])];
        d.sort_unstable();
        let (lo, mid, hi) = (d[0] as f32, d[1] as f32, d[2] as f32);
        // hi-mid straight, (mid-lo) face-diagonals, lo body-diagonals.
        ((hi - mid) + (mid - lo) * std::f32::consts::SQRT_2 + lo * 3.0_f32.sqrt()) * cell
    };

    let key = |v: [usize; 3], dir: u8| (lin(v) as u64) << 5 | dir as u64;

    let mut g: HashMap<u64, f32> = HashMap::new();
    let mut parent: HashMap<u64, (u64, [usize; 3])> = HashMap::new();
    let mut open: BinaryHeap<HeapItem> = BinaryHeap::new();

    g.insert(key(start, NONE_DIR), 0.0);
    open.push(HeapItem { f: h(start), g: 0.0, voxel: start, dir: NONE_DIR });

    while let Some(cur) = open.pop() {
        let cur_k = key(cur.voxel, cur.dir);
        if cur.g > *g.get(&cur_k).unwrap_or(&f32::INFINITY) {
            continue; // stale heap entry
        }
        if cur.voxel == goal {
            return Some(reconstruct(cur.voxel, cur.dir, &parent, occ, cell));
        }

        for (d, mv) in moves.iter().enumerate() {
            let d = d as u8;
            let dz = mv[2];
            // Per-system Z discipline.
            match c.z_mode {
                ZMode::Planar if dz != 0 => continue,
                ZMode::MonotoneDown if dz > 0 => continue,
                _ => {}
            }
            // Bend constraint: forbid turns sharper than 90°.
            let bend = if cur.dir == NONE_DIR {
                0.0
            } else {
                bend_deg(moves[cur.dir as usize], *mv)
            };
            if bend > 90.0 + 1e-3 {
                continue;
            }
            let ni = cur.voxel[0] as i32 + mv[0];
            let nj = cur.voxel[1] as i32 + mv[1];
            let nk = cur.voxel[2] as i32 + mv[2];
            if ni < 0 || nj < 0 || nk < 0 {
                continue;
            }
            let nv = [ni as usize, nj as usize, nk as usize];
            if !occ.is_free(nv[0], nv[1], nv[2]) {
                continue;
            }
            // Step cost: geometric length, ×Z penalty if vertical, + a
            // bend penalty scaled by angle (45° ≈ half of 90°).
            let len = ((mv[0] * mv[0] + mv[1] * mv[1] + mv[2] * mv[2]) as f32).sqrt() * cell;
            let mut step = len;
            if dz != 0 {
                step *= c.z_cost_mult.max(1.0);
            }
            step += c.turn_cost.max(0.0) * (bend / 90.0);
            let ng = cur.g + step;
            let nkk = key(nv, d);
            if ng < *g.get(&nkk).unwrap_or(&f32::INFINITY) {
                g.insert(nkk, ng);
                parent.insert(nkk, (cur_k, cur.voxel));
                open.push(HeapItem { f: ng + h(nv), g: ng, voxel: nv, dir: d });
            }
        }
    }
    None
}

fn reconstruct(
    goal: [usize; 3],
    goal_dir: u8,
    parent: &HashMap<u64, (u64, [usize; 3])>,
    occ: &Occupancy,
    _cell: f32,
) -> Route {
    let (nx, ny) = (occ.grid.dims[0], occ.grid.dims[1]);
    let lin = |v: [usize; 3]| (v[2] * ny + v[1]) * nx + v[0];
    let mut voxels = vec![goal];
    let mut cur_k = (lin(goal) as u64) << 5 | goal_dir as u64;
    while let Some(&(pk, pv)) = parent.get(&cur_k) {
        voxels.push(pv);
        cur_k = pk;
    }
    voxels.reverse();

    let polyline: Vec<Vec3> =
        voxels.iter().map(|v| occ.grid.voxel_center(v[0], v[1], v[2])).collect();
    let length_m = polyline.windows(2).map(|w| (w[1] - w[0]).length()).sum::<f32>();
    let (bends, max_bend_deg) = bend_stats(&voxels);
    Route { voxels, polyline, length_m, bends, max_bend_deg }
}

/// (number of direction changes, largest bend angle in degrees).
fn bend_stats(voxels: &[[usize; 3]]) -> (usize, f32) {
    if voxels.len() < 3 {
        return (0, 0.0);
    }
    let step = |a: [usize; 3], b: [usize; 3]| {
        [b[0] as i32 - a[0] as i32, b[1] as i32 - a[1] as i32, b[2] as i32 - a[2] as i32]
    };
    let mut bends = 0;
    let mut max_deg = 0.0f32;
    let mut prev = step(voxels[0], voxels[1]);
    for w in voxels.windows(2).skip(1) {
        let d = step(w[0], w[1]);
        if d != prev {
            bends += 1;
            max_deg = max_deg.max(bend_deg(prev, d));
            prev = d;
        }
    }
    (bends, max_deg)
}

/// Min-heap entry ordered by `f = g + h` (lower is better).
struct HeapItem {
    f: f32,
    g: f32,
    voxel: [usize; 3],
    dir: u8,
}
impl PartialEq for HeapItem {
    fn eq(&self, other: &Self) -> bool {
        self.f == other.f
    }
}
impl Eq for HeapItem {}
impl PartialOrd for HeapItem {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for HeapItem {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reversed: BinaryHeap is a max-heap, we want the smallest `f`.
        other.f.total_cmp(&self.f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::occupancy::{build, OccupancyParams};

    fn empty(nx: usize, ny: usize, nz: usize) -> Occupancy {
        let max = Vec3::new(nx as f32, ny as f32, nz as f32) * 0.1;
        build(Vec3::ZERO, max, OccupancyParams { cell_m: 0.1, default_keepout_m: 2.4 }, &[], &[])
            .unwrap()
    }

    fn box_mesh(min: Vec3, max: Vec3) -> (Vec<f32>, Vec<u32>) {
        let v = [
            [min.x, min.y, min.z],
            [max.x, min.y, min.z],
            [max.x, max.y, min.z],
            [min.x, max.y, min.z],
            [min.x, min.y, max.z],
            [max.x, min.y, max.z],
            [max.x, max.y, max.z],
            [min.x, max.y, max.z],
        ];
        let verts: Vec<f32> = v.iter().flat_map(|p| p.iter().copied()).collect();
        let idx: Vec<u32> = vec![
            0, 2, 1, 0, 3, 2, 4, 5, 6, 4, 6, 7, 0, 1, 5, 0, 5, 4, 1, 2, 6, 1, 6, 5, 2, 3, 7, 2, 7,
            6, 3, 0, 4, 3, 4, 7,
        ];
        (verts, idx)
    }

    #[test]
    fn straight_path_has_no_bends() {
        let occ = empty(10, 3, 3);
        let r = find_path(&occ, [0, 1, 1], [9, 1, 1], &SystemConstraints::default()).unwrap();
        assert_eq!(r.voxels.first(), Some(&[0, 1, 1]));
        assert_eq!(r.voxels.last(), Some(&[9, 1, 1]));
        assert_eq!(r.bends, 0);
        assert!((r.length_m - 0.9).abs() < 1e-4);
    }

    #[test]
    fn diagonal_goal_uses_45_degree_moves() {
        // Empty layer: a diagonal target should be reached with 45° moves,
        // not an L of right angles — so length ≈ 5·√2·cell < Manhattan.
        let occ = empty(6, 6, 3);
        let c = SystemConstraints { z_mode: ZMode::Planar, ..SystemConstraints::default() };
        let r = find_path(&occ, [0, 0, 1], [5, 5, 1], &c).unwrap();
        let manhattan = 10.0 * 0.1;
        assert!(r.length_m < manhattan - 1e-3, "diagonal route, got {} m", r.length_m);
        assert!(r.max_bend_deg <= 90.0 + 1e-3, "no hairpins, got {}", r.max_bend_deg);
    }

    #[test]
    fn routes_around_obstacle_within_90_degrees() {
        let max = Vec3::new(1.0, 1.0, 0.3);
        let (v, i) = box_mesh(Vec3::new(0.4, 0.0, 0.0), Vec3::new(0.5, 0.8, 0.3));
        let occ = build(
            Vec3::ZERO,
            max,
            OccupancyParams { cell_m: 0.1, default_keepout_m: 2.4 },
            &[(&v, &i)],
            &[],
        )
        .unwrap();
        let r = find_path(&occ, [0, 1, 1], [9, 1, 1], &SystemConstraints::default()).unwrap();
        assert!(r.bends >= 1, "must detour, got {} bends", r.bends);
        assert!(r.max_bend_deg <= 90.0 + 1e-3, "≤90° bends only, got {}", r.max_bend_deg);
        // Detour avoids the wall voxels (x col 4, y 0..7).
        assert!(r.voxels.iter().all(|v| !occ.grid.is_occupied(v[0], v[1], v[2])));
    }

    #[test]
    fn pressure_system_stays_planar() {
        let occ = empty(6, 6, 6);
        let c = SystemConstraints::for_kind(SystemKind::PressurePipe);
        assert_eq!(c.z_mode, ZMode::Planar);
        let r = find_path(&occ, [0, 0, 3], [5, 5, 3], &c).unwrap();
        assert!(r.voxels.iter().all(|v| v[2] == 3), "pressure system holds elevation");
    }

    #[test]
    fn gravity_drain_never_rises() {
        let occ = empty(6, 3, 6);
        let c = SystemConstraints::for_kind(SystemKind::GravityDrain);
        assert_eq!(c.z_mode, ZMode::MonotoneDown);
        let r = find_path(&occ, [0, 1, 5], [5, 1, 2], &c).unwrap();
        for w in r.voxels.windows(2) {
            assert!(w[1][2] <= w[0][2], "drain never rises: {:?} -> {:?}", w[0], w[1]);
        }
    }

    #[test]
    fn unknown_system_prefers_one_elevation() {
        // Start and goal on the same layer, but a clear path exists either
        // level or via a detour through another layer. The high Z penalty
        // must keep the Unknown route on the start layer.
        let occ = empty(8, 3, 4);
        let c = SystemConstraints::default(); // Unknown
        let r = find_path(&occ, [0, 1, 1], [7, 1, 1], &c).unwrap();
        assert!(r.voxels.iter().all(|v| v[2] == 1), "stays at elevation when system unknown");
    }

    #[test]
    fn no_path_through_a_sealed_obstacle() {
        // The wall overhangs the grid in Y/Z so it seals the full
        // cross-section including the one-cell pad `from_bounds` adds.
        let max = Vec3::new(1.0, 0.5, 0.5);
        let (v, i) = box_mesh(Vec3::new(0.4, -0.2, -0.2), Vec3::new(0.5, 0.7, 0.7));
        let occ = build(
            Vec3::ZERO,
            max,
            OccupancyParams { cell_m: 0.1, default_keepout_m: 2.4 },
            &[(&v, &i)],
            &[],
        )
        .unwrap();
        assert!(find_path(&occ, [0, 2, 2], [9, 2, 2], &SystemConstraints::default()).is_none());
    }
}
