//! Per-product geometric analytics.
//!
//! Computed from the world-coordinate triangle mesh we already emit.
//! Everything is one O(triangles) pass — surface area via the cross-
//! product formula, mesh volume via the signed-tetrahedra divergence
//! method (for closed meshes; open shells get a reasonable estimate but
//! the value is signed and depends on winding).
//!
//! Why these are valuable for QTO:
//!   * `surface_area_mm2` — paint, finishes, fireproofing, cladding qty
//!   * `volume_mm3`       — concrete, insulation, mass × density
//!   * `aabb_*`           — geographic bounding, clash extents, layout
//!   * `triangle_count`   — model complexity / authoring tool fingerprint
//!   * `vertex_count`     — same, plus mesh-storage cost proxy
//!
//! Author-supplied `IfcElementQuantity` values (when present) override
//! these for the QTO pipeline; ours are the geometric truth that survives
//! authoring-tool quirks.

use crate::mesh::ProductMesh;

#[derive(Debug, Clone)]
pub struct ProductStats {
    pub guid: String,
    pub entity: String,
    pub source: &'static str,
    pub vertex_count: u32,
    pub triangle_count: u32,
    /// World-coord axis-aligned bounding box in model units.
    pub xmin: f32,
    pub ymin: f32,
    pub zmin: f32,
    pub xmax: f32,
    pub ymax: f32,
    pub zmax: f32,
    /// Sum of triangle areas (mm² for typical millimetre-unit IFCs).
    pub surface_area: f32,
    /// Signed mesh volume via divergence theorem (mm³). Closed meshes
    /// produce a sign that depends on winding; the consumer typically
    /// uses `volume.abs()`.
    pub volume: f32,
    /// AABB volume — equals xyz extents product. Always positive.
    pub aabb_volume: f32,
    /// World-space coordinate of the IfcLocalPlacement origin — where
    /// the authoring tool thinks the element "is".
    pub placement_x: f32,
    pub placement_y: f32,
    pub placement_z: f32,
    /// Euclidean distance from placement origin to mesh AABB centroid.
    /// Big values relative to `max_extent` mean the geometry is sitting
    /// somewhere different from where the placement says it should be.
    pub drift_distance: f32,
    /// Largest of (xmax-xmin, ymax-ymin, zmax-zmin) — the element's
    /// biggest dimension. Used to normalise `drift_distance`.
    pub max_extent: f32,
    /// `drift_distance / max_extent`. A 100m wall whose placement is at
    /// one end has ratio 0.5 (mesh centroid is 50m from placement,
    /// extent is 100m). A 50mm sensor 100m from its placement has ratio
    /// 2000 — almost certainly an authoring bug.
    pub drift_ratio: f32,
    /// `"ok"` / `"warn"` / `"error"` per the drift heuristic below.
    pub drift_severity: &'static str,
    /// Validity classifier mirroring
    /// [`crate::mesh::qto::MeshQto::mesh_quality`]: `"closed"` /
    /// `"open_shell"` / `"degenerate"`. Surfaced on the Python drift
    /// DataFrame so analysts can filter out garbage volumes before
    /// any SUM/AVG aggregation.
    pub mesh_quality: &'static str,
}

impl ProductStats {
    /// Compute stats for one product.
    pub fn from_mesh(mesh: &ProductMesh) -> Self {
        let n_verts = (mesh.vertices.len() / 3) as u32;
        let n_tris = (mesh.indices.len() / 3) as u32;

        let mut xmin = f32::INFINITY;
        let mut ymin = f32::INFINITY;
        let mut zmin = f32::INFINITY;
        let mut xmax = f32::NEG_INFINITY;
        let mut ymax = f32::NEG_INFINITY;
        let mut zmax = f32::NEG_INFINITY;

        for chunk in mesh.vertices.chunks_exact(3) {
            let (x, y, z) = (chunk[0], chunk[1], chunk[2]);
            if x < xmin { xmin = x; }
            if y < ymin { ymin = y; }
            if z < zmin { zmin = z; }
            if x > xmax { xmax = x; }
            if y > ymax { ymax = y; }
            if z > zmax { zmax = z; }
        }
        if !xmin.is_finite() {
            xmin = 0.0; ymin = 0.0; zmin = 0.0;
            xmax = 0.0; ymax = 0.0; zmax = 0.0;
        }

        let mut surface_area: f32 = 0.0;
        let mut volume_x6: f32 = 0.0; // volume × 6, divided out at end

        for tri in mesh.indices.chunks_exact(3) {
            let (a, b, c) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
            // Bounds-safe — we wrote these triangles ourselves.
            let ax = mesh.vertices[a * 3];     let ay = mesh.vertices[a * 3 + 1]; let az = mesh.vertices[a * 3 + 2];
            let bx = mesh.vertices[b * 3];     let by = mesh.vertices[b * 3 + 1]; let bz = mesh.vertices[b * 3 + 2];
            let cx = mesh.vertices[c * 3];     let cy = mesh.vertices[c * 3 + 1]; let cz = mesh.vertices[c * 3 + 2];

            // ((b - a) × (c - a)) — un-normalised area normal
            let ux = bx - ax; let uy = by - ay; let uz = bz - az;
            let vx = cx - ax; let vy = cy - ay; let vz = cz - az;
            let nx = uy * vz - uz * vy;
            let ny = uz * vx - ux * vz;
            let nz = ux * vy - uy * vx;
            let cross_mag = (nx * nx + ny * ny + nz * nz).sqrt();
            surface_area += 0.5 * cross_mag;

            // Signed tetrahedra divergence: V = (1/6) Σ a · (b × c)
            volume_x6 += ax * (by * cz - bz * cy)
                       + ay * (bz * cx - bx * cz)
                       + az * (bx * cy - by * cx);
        }
        let volume = volume_x6 / 6.0;

        let aabb_volume = (xmax - xmin) * (ymax - ymin) * (zmax - zmin);

        // --- Placement-vs-geometry drift -------------------------------
        // A common BIM bug: the element's IfcLocalPlacement sits in one
        // location but its mesh actually lives somewhere else. Visibly,
        // the element appears in the wrong floor / area; programmatically,
        // its placement reads one coordinate while its rendered geometry
        // is at another. We catch this by comparing the placement
        // world-origin against the mesh AABB centroid.
        let [px, py, pz] = mesh.placement_origin;
        let cx = (xmin + xmax) * 0.5;
        let cy = (ymin + ymax) * 0.5;
        let cz = (zmin + zmax) * 0.5;
        let dx = cx - px;
        let dy = cy - py;
        let dz = cz - pz;
        let drift = (dx * dx + dy * dy + dz * dz).sqrt();
        let ex = xmax - xmin;
        let ey = ymax - ymin;
        let ez = zmax - zmin;
        let max_extent = ex.max(ey).max(ez).max(1e-6);
        let ratio = drift / max_extent;
        // Severity heuristic:
        //   ok    — ratio <= 2.0  OR  drift < 10 mm absolute (rounding noise)
        //   warn  — 2.0 < ratio <= 10.0
        //   error — ratio > 10.0 AND drift > 10 mm
        let severity = if drift < 10.0 || ratio <= 2.0 {
            "ok"
        } else if ratio <= 10.0 {
            "warn"
        } else {
            "error"
        };

        // Mesh-quality classifier (mirrors `MeshQto::mesh_quality`):
        // see field docstring for the full taxonomy. Two-tier — cheap
        // `|volume| > aabb` upper-bound check first, then edge-pairing
        // manifold check for the under-detected cases. Computing it
        // here keeps the drift DataFrame self-contained — analysts get
        // the open-shell flag alongside the placement-drift columns
        // without a separate substrate-bundle pass.
        let mesh_quality = if aabb_volume <= 0.0 {
            "degenerate"
        } else if volume.abs() > aabb_volume * 1.001 {
            "open_shell"
        } else if !crate::mesh::qto::is_closed_manifold(&mesh.indices) {
            "open_shell"
        } else {
            "closed"
        };

        Self {
            guid: mesh.guid.clone(),
            entity: mesh.entity.clone(),
            source: mesh.source,
            vertex_count: n_verts,
            triangle_count: n_tris,
            xmin, ymin, zmin, xmax, ymax, zmax,
            surface_area,
            volume,
            aabb_volume,
            placement_x: px, placement_y: py, placement_z: pz,
            drift_distance: drift,
            max_extent,
            drift_ratio: ratio,
            drift_severity: severity,
            mesh_quality,
        }
    }
}

/// Write stats as CSV.
pub fn write_csv<W: std::io::Write>(stats: &[ProductStats], out: &mut W) -> std::io::Result<()> {
    use std::io::Write;
    let mut w = std::io::BufWriter::with_capacity(1 << 20, out);
    writeln!(
        w,
        "guid,entity,source,vertex_count,triangle_count,\
         xmin,ymin,zmin,xmax,ymax,zmax,\
         surface_area,volume_signed,volume_abs,aabb_volume,\
         placement_x,placement_y,placement_z,\
         drift_distance,max_extent,drift_ratio,drift_severity,\
         mesh_quality"
    )?;
    for s in stats {
        writeln!(
            w,
            "{},{},{},{},{},{},{},{},{},{},{},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{},{}",
            s.guid,
            s.entity,
            s.source,
            s.vertex_count,
            s.triangle_count,
            s.xmin, s.ymin, s.zmin,
            s.xmax, s.ymax, s.zmax,
            s.surface_area,
            s.volume,
            s.volume.abs(),
            s.aabb_volume,
            s.placement_x, s.placement_y, s.placement_z,
            s.drift_distance,
            s.max_extent,
            s.drift_ratio,
            s.drift_severity,
            s.mesh_quality,
        )?;
    }
    w.flush()?;
    Ok(())
}

/// File-level aggregate analytics.
#[derive(Debug, Default, Clone)]
pub struct FileStats {
    pub products_with_mesh: usize,
    pub total_vertices: u64,
    pub total_triangles: u64,
    pub total_surface_area: f64,
    pub total_abs_volume: f64,
    pub bbox_min: [f32; 3],
    pub bbox_max: [f32; 3],
    /// Histograms keyed by entity name.
    pub by_entity_triangles: std::collections::HashMap<String, u64>,
    pub by_entity_count: std::collections::HashMap<String, u32>,
    /// Top-N "fattest" elements by triangle count.
    pub top_complex: Vec<(String, String, u32)>, // (guid, entity, tri_count)
    // Drift counts ---------------------------------------------------
    pub drift_ok: usize,
    pub drift_warn: usize,
    pub drift_error: usize,
    /// Top-N elements ranked by `drift_ratio` (descending) — likely
    /// authoring bugs to inspect. Each entry: (guid, entity,
    /// drift_distance, max_extent, drift_ratio, severity).
    pub top_drift: Vec<(String, String, f32, f32, f32, &'static str)>,
}

impl FileStats {
    pub fn from_products(stats: &[ProductStats]) -> Self {
        if stats.is_empty() {
            return Self::default();
        }
        let mut bb_min = [f32::INFINITY; 3];
        let mut bb_max = [f32::NEG_INFINITY; 3];
        let mut by_tris: std::collections::HashMap<String, u64> = Default::default();
        let mut by_count: std::collections::HashMap<String, u32> = Default::default();
        let mut total_v: u64 = 0;
        let mut total_t: u64 = 0;
        let mut total_a: f64 = 0.0;
        let mut total_vol: f64 = 0.0;

        for s in stats {
            total_v += s.vertex_count as u64;
            total_t += s.triangle_count as u64;
            total_a += s.surface_area as f64;
            total_vol += s.volume.abs() as f64;
            if s.xmin < bb_min[0] { bb_min[0] = s.xmin; }
            if s.ymin < bb_min[1] { bb_min[1] = s.ymin; }
            if s.zmin < bb_min[2] { bb_min[2] = s.zmin; }
            if s.xmax > bb_max[0] { bb_max[0] = s.xmax; }
            if s.ymax > bb_max[1] { bb_max[1] = s.ymax; }
            if s.zmax > bb_max[2] { bb_max[2] = s.zmax; }

            *by_tris.entry(s.entity.clone()).or_insert(0) += s.triangle_count as u64;
            *by_count.entry(s.entity.clone()).or_insert(0) += 1;
        }

        // Top-20 fattest by triangle count.
        let mut top: Vec<(String, String, u32)> = stats
            .iter()
            .map(|s| (s.guid.clone(), s.entity.clone(), s.triangle_count))
            .collect();
        top.sort_by(|a, b| b.2.cmp(&a.2));
        top.truncate(20);

        // Drift severity histogram + top-50 worst offenders.
        let mut drift_ok = 0usize;
        let mut drift_warn = 0usize;
        let mut drift_error = 0usize;
        for s in stats {
            match s.drift_severity {
                "warn" => drift_warn += 1,
                "error" => drift_error += 1,
                _ => drift_ok += 1,
            }
        }
        let mut drift: Vec<(String, String, f32, f32, f32, &'static str)> = stats
            .iter()
            .filter(|s| s.drift_severity != "ok")
            .map(|s| (
                s.guid.clone(),
                s.entity.clone(),
                s.drift_distance,
                s.max_extent,
                s.drift_ratio,
                s.drift_severity,
            ))
            .collect();
        drift.sort_by(|a, b| b.4.partial_cmp(&a.4).unwrap_or(std::cmp::Ordering::Equal));
        drift.truncate(50);

        Self {
            products_with_mesh: stats.len(),
            total_vertices: total_v,
            total_triangles: total_t,
            total_surface_area: total_a,
            total_abs_volume: total_vol,
            bbox_min: bb_min,
            bbox_max: bb_max,
            by_entity_triangles: by_tris,
            by_entity_count: by_count,
            top_complex: top,
            drift_ok,
            drift_warn,
            drift_error,
            top_drift: drift,
        }
    }

    /// Render a human-readable summary suitable for the CLI.
    pub fn render_summary(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!(
            "=== file-level analytics ===\n\
             products with mesh: {:>10}\n\
             total vertices:     {:>10}\n\
             total triangles:    {:>10}\n\
             total surface area: {:>14.0} mm² ({:.2} m²)\n\
             total |volume|:     {:>14.0} mm³ ({:.2} m³)\n\
             bbox: ({:.0}, {:.0}, {:.0}) → ({:.0}, {:.0}, {:.0})\n\
             extents: {:.0} × {:.0} × {:.0} mm\n\n",
            self.products_with_mesh,
            self.total_vertices,
            self.total_triangles,
            self.total_surface_area,
            self.total_surface_area / 1e6,
            self.total_abs_volume,
            self.total_abs_volume / 1e9,
            self.bbox_min[0], self.bbox_min[1], self.bbox_min[2],
            self.bbox_max[0], self.bbox_max[1], self.bbox_max[2],
            self.bbox_max[0] - self.bbox_min[0],
            self.bbox_max[1] - self.bbox_min[1],
            self.bbox_max[2] - self.bbox_min[2],
        ));
        s.push_str("triangles by entity:\n");
        let mut by_e: Vec<(&String, &u64)> = self.by_entity_triangles.iter().collect();
        by_e.sort_by(|a, b| b.1.cmp(a.1));
        for (e, t) in by_e.iter().take(15) {
            let count = self.by_entity_count.get(*e).copied().unwrap_or(0);
            s.push_str(&format!(
                "  {:<35} {:>10} tris  ({} elements, {:.0} avg)\n",
                e, t, count, **t as f64 / count.max(1) as f64,
            ));
        }
        s.push_str("\ntop 10 complex products (triangle count):\n");
        for (guid, entity, t) in self.top_complex.iter().take(10) {
            s.push_str(&format!("  {:<22} {:<30} {:>8} tris\n", &guid[..guid.len().min(22)], entity, t));
        }
        s.push_str(&format!(
            "\nplacement-vs-geometry drift:\n  ok:    {}\n  warn:  {}  (drift_ratio in 2-10)\n  error: {}  (drift_ratio > 10 and drift > 10mm)\n",
            self.drift_ok, self.drift_warn, self.drift_error,
        ));
        if !self.top_drift.is_empty() {
            s.push_str("\ntop drift offenders (geometry far from placement, scaled by element size):\n");
            s.push_str(&format!(
                "  {:<22} {:<30} {:>12} {:>12} {:>12}  {}\n",
                "guid", "entity", "drift_mm", "extent_mm", "ratio", "severity",
            ));
            for (guid, entity, dd, mx, ratio, sev) in self.top_drift.iter().take(20) {
                s.push_str(&format!(
                    "  {:<22} {:<30} {:>12.0} {:>12.0} {:>12.1}  {}\n",
                    &guid[..guid.len().min(22)],
                    entity,
                    dd,
                    mx,
                    ratio,
                    sev,
                ));
            }
        }
        s
    }
}
