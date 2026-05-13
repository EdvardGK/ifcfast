//! `ifcfast-mesh` — IFC → triangle mesh → `.obj`.
//!
//! Phase 1A binary. Pure Rust, no PyO3, no ifcopenshell. Reads an IFC,
//! walks the products, meshes everything it can, writes Wavefront .obj.
//!
//! Build:
//!     cargo build --release --bin ifcfast-mesh --no-default-features --features mesh
//!
//! Usage:
//!     ifcfast-mesh <path/to/file.ifc> [output.obj]
//!
//! If no output path given, writes to `<input>.obj` next to the IFC.

use std::env;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use memmap2::Mmap;

use _core::mesh;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!(
            "usage: {} <path/to/file.ifc> [output.obj]",
            args.first().map(String::as_str).unwrap_or("ifcfast-mesh")
        );
        return ExitCode::from(2);
    }

    let in_path = PathBuf::from(&args[1]);
    let out_path = if let Some(o) = args.get(2) {
        PathBuf::from(o)
    } else {
        let mut p = in_path.clone();
        let stem = p.file_stem().map(|s| s.to_owned()).unwrap_or_default();
        p.set_file_name(format!("{}.obj", stem.to_string_lossy()));
        p
    };
    // Format inferred from the output extension:
    //   .glb / .gltf   → glTF binary
    //   .csv           → per-product geometric stats (vertex/triangle
    //                    counts, surface area, mesh volume, AABB)
    //   anything else  → Wavefront OBJ
    let ext = out_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let format_glb = ext == "glb" || ext == "gltf";
    let format_csv = ext == "csv";

    let t_open = Instant::now();
    let file = match File::open(&in_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("open {}: {e}", in_path.display());
            return ExitCode::from(1);
        }
    };
    // SAFETY: regular file we own and don't write to.
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
        "[ifcfast-mesh] open(mmap) {:.2}ms — {:.1} MB",
        open_ms, size_mb
    );

    let (meshes, stats) = mesh::mesh_ifc(&mmap);

    eprintln!("[ifcfast-mesh] entity table     {:>8.1} ms", stats.entity_table_build_ms);
    eprintln!("[ifcfast-mesh] mesh emission    {:>8.1} ms", stats.elapsed_ms);
    eprintln!("[ifcfast-mesh] products seen:    {}", stats.products_seen);
    eprintln!("[ifcfast-mesh] products meshed:  {}", stats.products_meshed);
    eprintln!("[ifcfast-mesh] products deferred:{}", stats.products_deferred);
    eprintln!("[ifcfast-mesh] triangles emitted:{}", stats.triangles);
    if !stats.by_source.is_empty() {
        eprintln!("[ifcfast-mesh] by source:");
        let mut srcs: Vec<(&String, &usize)> = stats.by_source.iter().collect();
        srcs.sort_by(|a, b| b.1.cmp(a.1));
        for (k, v) in srcs {
            eprintln!("    {:<20} {}", k, v);
        }
    }

    // Write OBJ.
    let t_write = Instant::now();
    let mut out = match File::create(&out_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("create {}: {e}", out_path.display());
            return ExitCode::from(1);
        }
    };
    // Always compute stats — cheap relative to meshing, and feeds the
    // CLI summary regardless of output format.
    let t_stats = Instant::now();
    let prod_stats: Vec<mesh::stats::ProductStats> = meshes
        .iter()
        .map(mesh::stats::ProductStats::from_mesh)
        .collect();
    let file_stats = mesh::stats::FileStats::from_products(&prod_stats);
    let stats_ms = t_stats.elapsed().as_secs_f64() * 1000.0;
    eprintln!("[ifcfast-mesh] geometric stats {:>8.1} ms", stats_ms);
    eprintln!();
    eprint!("{}", file_stats.render_summary());

    let write_result = if format_glb {
        mesh::gltf::write(&meshes, &mut out)
    } else if format_csv {
        mesh::stats::write_csv(&prod_stats, &mut out)
    } else {
        mesh::obj::write(&meshes, &mut out)
    };
    if let Err(e) = write_result {
        eprintln!("write {}: {e}", out_path.display());
        return ExitCode::from(1);
    }
    let write_ms = t_write.elapsed().as_secs_f64() * 1000.0;
    let written = std::fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
    eprintln!(
        "[ifcfast-mesh] wrote {} ({:.1} MB) in {:.1} ms",
        out_path.display(),
        written as f64 / 1e6,
        write_ms,
    );

    // Suppress `Path` unused warning when only used in messages.
    let _ = Path::new("");
    ExitCode::SUCCESS
}
