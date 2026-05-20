//! `ifcfast-bundle` — IFC → streaming substrate (GeoParquet).
//!
//! One streaming pass over the IFC. Builds per-product semantic
//! snapshots (psets, materials, quantities, classifications, storey,
//! type, aggregates), pairs each with its mesh fragment, writes a row
//! to a Parquet file row-group-at-a-time, then drops the mesh from
//! RAM and moves on.
//!
//! Working-set RAM is bounded by the row-group buffer (default 1024
//! products) + the indexer/extractor maps (bounded by entity count,
//! not geometry size). The 1 GB Sannergata IFC that OOM-killed the
//! batch pipeline writes through this one without ever holding more
//! than ~1024 product meshes at once.
//!
//! Build:
//!     cargo build --release --bin ifcfast-bundle --no-default-features --features bundle
//!
//! Usage:
//!     ifcfast-bundle <path/to/file.ifc> [output.parquet]

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use memmap2::Mmap;

use _core::bundle::parquet_sink::ParquetSink;
use _core::bundle::Bundle;
use _core::mesh::mesh_ifc_streaming;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!(
            "usage: {} <path/to/file.ifc> [output.parquet]",
            args.first().map(String::as_str).unwrap_or("ifcfast-bundle")
        );
        return ExitCode::from(2);
    }

    let in_path = PathBuf::from(&args[1]);
    let out_path = if let Some(o) = args.get(2) {
        PathBuf::from(o)
    } else {
        let mut p = in_path.clone();
        let stem = p.file_stem().map(|s| s.to_owned()).unwrap_or_default();
        p.set_file_name(format!("{}.parquet", stem.to_string_lossy()));
        p
    };

    let t_open = Instant::now();
    let file = match std::fs::File::open(&in_path) {
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

    let mut sink = match ParquetSink::create(&out_path, &bundle) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("create {}: {e}", out_path.display());
            return ExitCode::from(1);
        }
    };

    let t_stream = Instant::now();
    let stats = mesh_ifc_streaming(&mmap, &mut sink);
    let stream_ms = t_stream.elapsed().as_secs_f64() * 1000.0;

    let written = match sink.finish() {
        Ok(n) => n,
        Err(e) => {
            eprintln!("finish {}: {e}", out_path.display());
            return ExitCode::from(1);
        }
    };

    eprintln!("[ifcfast-bundle] entity table     {:>8.1} ms", stats.entity_table_build_ms);
    eprintln!("[ifcfast-bundle] streaming mesh   {:>8.1} ms", stream_ms);
    eprintln!("[ifcfast-bundle] products seen:    {}", stats.products_seen);
    eprintln!("[ifcfast-bundle] products meshed:  {}", stats.products_meshed);
    eprintln!("[ifcfast-bundle] products written: {}", written);
    eprintln!("[ifcfast-bundle] products deferred:{}", stats.products_deferred);
    eprintln!("[ifcfast-bundle] triangles emitted:{}", stats.triangles);

    let parquet_bytes = std::fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
    eprintln!(
        "[ifcfast-bundle] wrote {} ({:.1} MB)",
        out_path.display(),
        parquet_bytes as f64 / 1e6,
    );

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
