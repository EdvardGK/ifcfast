//! Runnable demonstration of the GH #63 reroute pipeline:
//! mesh → voxelize → go/nogo occupancy → classic A* with MEP constraints.
//!
//! Run:
//!   cargo run -p ifcfast-core --no-default-features --features mesh \
//!             --example reroute_demo
//!
//! It builds two synthetic scenes (no IFC needed) and prints an ASCII map
//! of the route the solver finds under different per-system constraints.

use _core::mesh::profile::Polygon2D;
use _core::occupancy::{build, KeepoutSpace, OccupancyParams};
use _core::routing::{find_path, SystemConstraints, SystemKind};
use glam::{Vec2, Vec3};

/// Closed axis-aligned box `[min,max]` as a 12-triangle mesh.
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
        0, 2, 1, 0, 3, 2, 4, 5, 6, 4, 6, 7, 0, 1, 5, 0, 5, 4, 1, 2, 6, 1, 6, 5, 2, 3, 7, 2, 7, 6,
        3, 0, 4, 3, 4, 7,
    ];
    (verts, idx)
}

fn rect(min: Vec2, max: Vec2) -> Polygon2D {
    Polygon2D {
        outer: vec![
            Vec2::new(min.x, min.y),
            Vec2::new(max.x, min.y),
            Vec2::new(max.x, max.y),
            Vec2::new(min.x, max.y),
        ],
        holes: Vec::new(),
    }
}

fn main() {
    demo_plan_view();
    demo_drain_fall();
    println!();
    println!("Legend:  # obstacle   S start   G goal   * route   . free");
    println!("Each call is `routing::find_path(occupancy, start, goal, SystemConstraints)`.");
}

/// Demo 1 — top-down plan view: a duct routing horizontally around a wall
/// (with a gap) and a column, in the plenum. Shows 45°/90° bends.
fn demo_plan_view() {
    // 6 m × 4 m room, 0.25 m voxels, a single routing layer.
    let cell = 0.25;
    let min = Vec3::ZERO;
    let max = Vec3::new(6.0, 4.0, 0.5);

    // Obstacles: a wall at x≈2.6 m blocking y 0..2.75 (gap above), and a
    // column block mid-room. Full layer height so they show in the slice.
    let wall = box_mesh(Vec3::new(2.5, 0.0, 0.0), Vec3::new(2.75, 2.75, 0.5));
    let col = box_mesh(Vec3::new(4.0, 1.5, 0.0), Vec3::new(4.5, 2.5, 0.5));

    let occ = build(
        min,
        max,
        OccupancyParams { cell_m: cell, default_keepout_m: 2.4 },
        &[(&wall.0, &wall.1), (&col.0, &col.1)],
        &[],
    )
    .expect("grid");

    let g = &occ.grid;
    let layer = 1usize;
    let start = g.world_to_voxel(Vec3::new(0.1, 2.0, 0.125)).unwrap();
    let goal = g.world_to_voxel(Vec3::new(5.9, 2.0, 0.125)).unwrap();

    // Ducting: level changes allowed but penalised; ≤90° bends, prefer 45°.
    let c = SystemConstraints::for_kind(SystemKind::Duct);
    let route = find_path(&occ, start, goal, &c).expect("route exists");

    println!("=== Demo 1 — duct routing around a wall + column (plan view) ===");
    println!(
        "system = Duct | length = {:.2} m | bends = {} | max bend = {:.0}°",
        route.length_m, route.bends, route.max_bend_deg
    );
    print_plan(&occ, layer, start, goal, &route.voxels);

    // Same scene, pressure system (Planar): identical here since it is one
    // layer — but prove the constraint resolves.
    let pc = SystemConstraints::for_kind(SystemKind::PressurePipe);
    println!(
        "  (PressurePipe profile: z_mode = {:?} — would hold one elevation)",
        pc.z_mode
    );
}

/// Demo 2 — side view (X–Z): a gravity drain that must DESCEND from a high
/// inlet to a low outlet, stepping under a beam. MonotoneDown forbids any
/// rise, so the path only ever holds or drops in Z.
fn demo_drain_fall() {
    let cell = 0.25;
    let min = Vec3::ZERO;
    let max = Vec3::new(6.0, 0.5, 3.0);

    // A beam hanging at z≈2.0..2.25 across x 2.0..4.5 — the drain must dip
    // below it on its way down.
    let beam = box_mesh(Vec3::new(2.0, 0.0, 2.0), Vec3::new(4.5, 0.5, 2.25));
    // Keep-out: occupiable room below 1.0 m here (compressed for the demo).
    let keep = KeepoutSpace {
        footprint: rect(Vec2::ZERO, Vec2::new(6.0, 0.5)),
        floor_z_m: 0.0,
        height_m: Some(0.75),
    };

    let occ = build(
        min,
        max,
        OccupancyParams { cell_m: cell, default_keepout_m: 2.4 },
        &[(&beam.0, &beam.1)],
        &[keep],
    )
    .expect("grid");

    let g = &occ.grid;
    let start = g.world_to_voxel(Vec3::new(0.1, 0.25, 2.7)).unwrap(); // high inlet
    let goal = g.world_to_voxel(Vec3::new(5.9, 0.25, 1.0)).unwrap(); // low outlet

    let c = SystemConstraints::for_kind(SystemKind::GravityDrain);
    let route = find_path(&occ, start, goal, &c).expect("drain route exists");

    println!();
    println!("=== Demo 2 — gravity drain, must run downhill under a beam (side view) ===");
    println!(
        "system = GravityDrain | z_mode = {:?} | length = {:.2} m | bends = {} | max bend = {:.0}°",
        c.z_mode, route.length_m, route.bends, route.max_bend_deg
    );
    let rises = route.voxels.windows(2).filter(|w| w[1][2] > w[0][2]).count();
    println!("  uphill steps = {rises}  (MonotoneDown guarantees 0)");
    print_side(&occ, start, goal, &route.voxels);
}

/// Top-down X/Y slice at a fixed Z layer.
fn print_plan(
    occ: &_core::occupancy::Occupancy,
    layer: usize,
    start: [usize; 3],
    goal: [usize; 3],
    route: &[[usize; 3]],
) {
    let g = &occ.grid;
    let [nx, ny, _] = g.dims;
    let on_route = |i: usize, j: usize| route.iter().any(|v| v[0] == i && v[1] == j);
    // Print rows top (high y) to bottom so it reads like a map.
    for j in (0..ny).rev() {
        let mut line = String::new();
        for i in 0..nx {
            let ch = if [i, j, layer] == start {
                'S'
            } else if [i, j, layer] == goal {
                'G'
            } else if g.is_occupied(i, j, layer) {
                '#'
            } else if on_route(i, j) {
                '*'
            } else {
                '.'
            };
            line.push(ch);
        }
        println!("  {line}");
    }
}

/// Side X/Z slice (collapsing Y: a cell is obstacle if occupied at any Y).
fn print_side(
    occ: &_core::occupancy::Occupancy,
    start: [usize; 3],
    goal: [usize; 3],
    route: &[[usize; 3]],
) {
    let g = &occ.grid;
    let [nx, ny, nz] = g.dims;
    let occ_xz = |i: usize, k: usize| (0..ny).any(|j| g.is_occupied(i, j, k));
    let on_route = |i: usize, k: usize| route.iter().any(|v| v[0] == i && v[2] == k);
    for k in (0..nz).rev() {
        let mut line = String::new();
        for i in 0..nx {
            let ch = if i == start[0] && k == start[2] {
                'S'
            } else if i == goal[0] && k == goal[2] {
                'G'
            } else if occ_xz(i, k) {
                '#'
            } else if on_route(i, k) {
                '*'
            } else {
                '.'
            };
            line.push(ch);
        }
        println!("  {line}");
    }
}
