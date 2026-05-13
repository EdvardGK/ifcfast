//! `ifcfast-bench` — PyO3-free benchmark for the native indexer.
//!
//! Build with:
//!     cargo build --release --bin ifcfast-bench --no-default-features
//!
//! `--no-default-features` is important: it skips the `python` feature so
//! the binary doesn't try to link against libpython. Output looks like:
//!
//!     file:           834.0 MB   schema=IFC2X3   unit_scale=0.001
//!     open(mmap):       0.01 ms
//!     index:         2384.20 ms   (= lex + extract, no Python)
//!     products:        87198
//!     storeys:            25
//!     contained_in:    87198
//!     aggregates:         27
//!     throughput:       350 MB/s

use std::env;
use std::fs::File;
use std::process::ExitCode;
use std::time::Instant;

use memmap2::Mmap;

use _core::indexer;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: {} <path/to/file.ifc>", args.first().map(String::as_str).unwrap_or("ifcfast-bench"));
        return ExitCode::from(2);
    }

    let path = &args[1];

    let t_open = Instant::now();
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("open {path}: {e}");
            return ExitCode::from(1);
        }
    };
    // SAFETY: mmap is safe for a regular file we own and don't write to.
    let mmap = match unsafe { Mmap::map(&file) } {
        Ok(m) => m,
        Err(e) => {
            eprintln!("mmap {path}: {e}");
            return ExitCode::from(1);
        }
    };
    let open_ms = t_open.elapsed().as_secs_f64() * 1000.0;

    let t_index = Instant::now();
    let idx = indexer::index(&mmap);
    let index_ms = t_index.elapsed().as_secs_f64() * 1000.0;

    let size_mb = mmap.len() as f64 / 1_000_000.0;
    let throughput = if index_ms > 0.0 {
        size_mb / (index_ms / 1000.0)
    } else {
        0.0
    };

    println!(
        "file:           {:>7.1} MB   schema={}   unit_scale={}",
        size_mb,
        idx.schema,
        idx.unit_scale
            .map(|s| format!("{s}"))
            .unwrap_or_else(|| "None".to_string()),
    );
    println!("open(mmap):     {open_ms:>10.2} ms");
    println!("index:          {index_ms:>10.2} ms   (lex + extract, no Python marshalling)");
    println!("products:       {:>10}", idx.product_step_id.len());
    println!("storeys:        {:>10}", idx.storey_step_id.len());
    println!("sites:          {:>10}", idx.site_step_id_to_guid.len());
    println!("buildings:      {:>10}", idx.building_step_id_to_guid.len());
    println!("contained_in:   {:>10}", idx.contained_in_child.len());
    println!("aggregates:     {:>10}", idx.aggregates_child.len());
    println!("storey_building:{:>10}", idx.storey_building_storey.len());
    println!("type_counts:    {:>10}   distinct entities", idx.type_counts.len());
    println!("throughput:     {throughput:>10.0} MB/s");

    if let Some(app) = &idx.authoring_app {
        println!("authoring_app:  {app:?}");
    }
    if let Some(proj) = &idx.project_name {
        println!("project_name:   {proj:?}");
    }

    ExitCode::SUCCESS
}
