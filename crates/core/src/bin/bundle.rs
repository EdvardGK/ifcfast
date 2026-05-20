//! `ifcfast-bundle` — IFC → streaming substrate (hierarchical GeoParquet).
//!
//! Two Parquet files are emitted per IFC, plus a DuckDB `view.sql`:
//!
//!   {out_dir}/representations.parquet    -- one row per unique mesh shape (rep_id)
//!   {out_dir}/instances.parquet          -- one row per IfcProduct (FK to rep_id)
//!   {out_dir}/view.sql                   -- CREATE VIEW products AS ... (join)
//!
//! The split is what makes the substrate chip-class capable: a 5000-
//! window facade whose families share an `IfcRepresentationMap` writes
//! ~1 rep row + 5000 instance rows instead of 5000 baked-geometry rows.
//! DuckDB consumers can read the joined `products` view and see the
//! same fields as before, or query the two tables directly when they
//! want to do something hierarchical (`SUM` of instance count × rep
//! triangle_count gives "if I expanded everything, how big would the
//! mesh be").
//!
//! Working-set RAM is bounded by the row-group buffer (default 1024
//! rows per file) + the rep-id dedup HashSet (≤ unique rep count,
//! typically 10s-1000s) + the indexer/extractor maps (bounded by entity
//! count). The 1 GB Sannergata IFC writes through this without ever
//! holding more than ~1024 product meshes at once.
//!
//! Build:
//!     cargo build --release --bin ifcfast-bundle --no-default-features --features bundle
//!
//! Usage:
//!     ifcfast-bundle <path/to/file.ifc> [out_dir]
//!
//! If `out_dir` is omitted, defaults to `{stem}.bundle/` next to the
//! input file.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use memmap2::Mmap;

use _core::bundle::parquet_sink::ParquetSink;
use _core::bundle::Bundle;
use _core::mesh::mesh_ifc_streaming;

const VIEW_SQL: &str = r#"-- ifcfast substrate convenience view.
-- Open both tables and join them so downstream queries that don't care
-- about instancing see one row per product with geometry attached.
--
--   duckdb -c ".read view.sql; SELECT class, COUNT(*) FROM products GROUP BY class;"
--
-- For hierarchical queries (the whole point of the split), read the
-- representations + instances tables directly:
--
--   SELECT i.class,
--          COUNT(*)                AS instance_count,
--          COUNT(DISTINCT i.rep_id) AS unique_shapes,
--          SUM(r.triangle_count)    AS total_triangle_bytes_if_expanded
--     FROM instances i
--     LEFT JOIN representations r USING (rep_id)
--    GROUP BY i.class
--    ORDER BY instance_count DESC;

CREATE OR REPLACE VIEW products AS
  SELECT
      i.*,
      r.source_kind        AS rep_source_kind,
      r.mesh_source        AS rep_mesh_source,
      r.vertex_count       AS rep_vertex_count,
      r.triangle_count     AS rep_triangle_count,
      r.vertices_le        AS rep_vertices_le,
      r.indices_le         AS rep_indices_le,
      r.segments           AS rep_segments,
      r.local_bbox_min_xyz AS rep_local_bbox_min_xyz,
      r.local_bbox_max_xyz AS rep_local_bbox_max_xyz
    FROM instances i
    LEFT JOIN representations r USING (rep_id);
"#;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!(
            "usage: {} <path/to/file.ifc> [out_dir]",
            args.first().map(String::as_str).unwrap_or("ifcfast-bundle")
        );
        return ExitCode::from(2);
    }

    let in_path = PathBuf::from(&args[1]);
    let out_dir = if let Some(o) = args.get(2) {
        PathBuf::from(o)
    } else {
        let stem = in_path.file_stem().map(|s| s.to_owned()).unwrap_or_default();
        let mut p = in_path.clone();
        p.set_file_name(format!("{}.bundle", stem.to_string_lossy()));
        p
    };

    if let Err(e) = fs::create_dir_all(&out_dir) {
        eprintln!("mkdir {}: {e}", out_dir.display());
        return ExitCode::from(1);
    }

    let t_open = Instant::now();
    let file = match fs::File::open(&in_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("open {}: {e}", in_path.display());
            return ExitCode::from(1);
        }
    };
    let mmap = match unsafe { Mmap::map(&file) } {
        Ok(m) => m,
        Err(e) => {
            eprintln!("mmap {}: {e}", in_path.display());
            return ExitCode::from(1);
        }
    };
    let open_ms = t_open.elapsed().as_secs_f64() * 1000.0;
    let size_mb = mmap.len() as f64 / 1_000_000.0;
    eprintln!(
        "[ifcfast-bundle] open(mmap) {:.2}ms — {:.1} MB",
        open_ms, size_mb
    );

    let t_bundle = Instant::now();
    let bundle = Bundle::build(&mmap);
    let bundle_ms = t_bundle.elapsed().as_secs_f64() * 1000.0;
    let sem = bundle.semantic_stats();
    eprintln!(
        "[ifcfast-bundle] semantic pre-pass {:>8.1} ms",
        bundle_ms
    );
    eprintln!("[ifcfast-bundle]   products indexed:    {}", sem.products_indexed);
    eprintln!("[ifcfast-bundle]   pset rows:           {}", sem.pset_rows);
    eprintln!("[ifcfast-bundle]   material rows:       {}", sem.material_rows);
    eprintln!("[ifcfast-bundle]   quantity rows:       {}", sem.quantity_rows);
    eprintln!("[ifcfast-bundle]   classification rows: {}", sem.classification_rows);

    let mut sink = match ParquetSink::create_in_dir(&out_dir, &bundle) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("create sink in {}: {e}", out_dir.display());
            return ExitCode::from(1);
        }
    };

    let t_stream = Instant::now();
    let stats = mesh_ifc_streaming(&mmap, &mut sink);
    let stream_ms = t_stream.elapsed().as_secs_f64() * 1000.0;

    let (instances_written, reps_written) = match sink.finish() {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("finish substrate write in {}: {e}", out_dir.display());
            return ExitCode::from(1);
        }
    };

    let view_path = out_dir.join("view.sql");
    if let Err(e) = fs::write(&view_path, VIEW_SQL) {
        eprintln!("write {}: {e}", view_path.display());
        return ExitCode::from(1);
    }

    eprintln!("[ifcfast-bundle] entity table     {:>8.1} ms", stats.entity_table_build_ms);
    eprintln!("[ifcfast-bundle] streaming mesh   {:>8.1} ms", stream_ms);
    eprintln!("[ifcfast-bundle] products seen:    {}", stats.products_seen);
    eprintln!("[ifcfast-bundle] products meshed:  {}", stats.products_meshed);
    eprintln!("[ifcfast-bundle] instances written:{}", instances_written);
    eprintln!("[ifcfast-bundle] unique reps:      {}", reps_written);
    if instances_written > 0 && reps_written > 0 {
        let ratio = instances_written as f64 / reps_written as f64;
        eprintln!(
            "[ifcfast-bundle] instance/rep ratio: {:.2}x (higher = more sharing)",
            ratio
        );
    }
    eprintln!("[ifcfast-bundle] products deferred:{}", stats.products_deferred);
    eprintln!("[ifcfast-bundle] triangles emitted:{}", stats.triangles);

    let rep_path = out_dir.join("representations.parquet");
    let inst_path = out_dir.join("instances.parquet");
    let rep_bytes = fs::metadata(&rep_path).map(|m| m.len()).unwrap_or(0);
    let inst_bytes = fs::metadata(&inst_path).map(|m| m.len()).unwrap_or(0);
    let total_mb = (rep_bytes + inst_bytes) as f64 / 1e6;
    eprintln!(
        "[ifcfast-bundle] wrote {}/representations.parquet ({:.1} MB)",
        out_dir.display(),
        rep_bytes as f64 / 1e6,
    );
    eprintln!(
        "[ifcfast-bundle] wrote {}/instances.parquet      ({:.1} MB)",
        out_dir.display(),
        inst_bytes as f64 / 1e6,
    );
    eprintln!(
        "[ifcfast-bundle] wrote {}/view.sql               (DuckDB join view)",
        out_dir.display(),
    );
    eprintln!("[ifcfast-bundle] total substrate size: {:.1} MB", total_mb);

    if !stats.by_source.is_empty() {
        eprintln!("[ifcfast-bundle] by source:");
        let mut srcs: Vec<(&String, &usize)> = stats.by_source.iter().collect();
        srcs.sort_by(|a, b| b.1.cmp(a.1));
        for (k, v) in srcs {
            eprintln!("    {:<28} {}", k, v);
        }
    }

    ExitCode::SUCCESS
}
