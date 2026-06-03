//! `ifcfast-clash` — substrate → clashes.parquet.
//!
//! Reads `instances.parquet` + `representations.parquet` from the given
//! bundle directory, runs broad-phase AABB overlap + narrow-phase mesh-
//! mesh intersection / distance, writes `clashes.parquet` next to them.
//!
//! Build:
//!     cargo build --release --bin ifcfast-clash --no-default-features --features clash
//!
//! Usage:
//!     ifcfast-clash <bundle_dir> [--tolerance N] [--out file.parquet]
//!
//! Defaults:
//!   - tolerance = 0.0 (hard clashes only)
//!   - out       = <bundle_dir>/clashes.parquet

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use _core::clash::{clash as run_clash, write_clashes_parquet, ClashCategory, ClashOptions};

fn parse_args(args: &[String]) -> Result<(PathBuf, f32, PathBuf), String> {
    if args.len() < 2 {
        return Err(format!(
            "usage: {} <bundle_dir> [--tolerance N] [--out file.parquet]",
            args.first().map(String::as_str).unwrap_or("ifcfast-clash")
        ));
    }
    let bundle_dir = PathBuf::from(&args[1]);
    let mut tolerance: f32 = 0.0;
    let mut out: Option<PathBuf> = None;

    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--tolerance" | "-t" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| "--tolerance requires a value".to_string())?;
                tolerance = v
                    .parse::<f32>()
                    .map_err(|e| format!("invalid --tolerance value `{v}`: {e}"))?;
            }
            "--out" | "-o" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| "--out requires a path".to_string())?;
                out = Some(PathBuf::from(v));
            }
            other => return Err(format!("unknown arg `{other}`")),
        }
        i += 1;
    }

    let out = out.unwrap_or_else(|| bundle_dir.join("clashes.parquet"));
    Ok((bundle_dir, tolerance, out))
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let (bundle_dir, tolerance_m, out_path) = match parse_args(&args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::from(2);
        }
    };

    let opts = ClashOptions {
        tolerance_m,
        ..ClashOptions::default()
    };

    let t = Instant::now();
    let report = match run_clash(&bundle_dir, &opts) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("clash failed: {e}");
            return ExitCode::from(1);
        }
    };
    let elapsed_ms = t.elapsed().as_secs_f64() * 1000.0;

    let mut hard = 0usize;
    let mut clearance = 0usize;
    let mut cat_clash = 0usize;
    let mut cat_insulation = 0usize;
    let mut cat_connection = 0usize;
    let mut cat_non_physical = 0usize;
    for p in &report.pairs {
        match p.kind {
            _core::clash::ClashKind::Hard => hard += 1,
            _core::clash::ClashKind::Clearance => clearance += 1,
        }
        match p.category {
            ClashCategory::Clash => cat_clash += 1,
            ClashCategory::Insulation => cat_insulation += 1,
            ClashCategory::Connection => cat_connection += 1,
            ClashCategory::NonPhysical => cat_non_physical += 1,
        }
    }

    eprintln!("[ifcfast-clash] tolerance           : {tolerance_m} m");
    eprintln!("[ifcfast-clash] pairs (hard)        : {hard}");
    eprintln!("[ifcfast-clash] pairs (clearance)   : {clearance}");
    eprintln!("[ifcfast-clash] cat clash           : {cat_clash}");
    eprintln!("[ifcfast-clash] cat insulation      : {cat_insulation}");
    eprintln!("[ifcfast-clash] cat connection      : {cat_connection}");
    eprintln!("[ifcfast-clash] cat non_physical    : {cat_non_physical}");
    eprintln!("[ifcfast-clash] geometryless skip   : {}", report.geometryless_skipped);
    eprintln!("[ifcfast-clash] narrow residuals    : {}", report.narrow_phase_residuals);
    eprintln!("[ifcfast-clash] elapsed             : {:.1} ms", elapsed_ms);

    if let Err(e) = write_clashes_parquet(&out_path, &report.pairs) {
        eprintln!("write {}: {e}", out_path.display());
        return ExitCode::from(1);
    }
    eprintln!("[ifcfast-clash] wrote {}", out_path.display());

    ExitCode::SUCCESS
}
