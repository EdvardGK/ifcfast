//! Single-pass IFC scan: tier-1 index + optional entity byte table.

use std::sync::Arc;
use std::time::Instant;

use crate::entity_table::{EntityTable, TableBuilder};
use crate::indexer::{self, IndexProfile, IndexedFile};
use crate::source::IfcSource;

pub struct ScanResult {
    pub indexed: IndexedFile,
    pub table: Option<EntityTable>,
    pub scan_ms: f64,
}

/// One lexer pass over the DATA section. When `build_table` is true, fills both
/// [`IndexedFile`] and [`EntityTable`]; otherwise tier-1 index only.
pub fn scan_ifc(source: Arc<IfcSource>, build_table: bool, profile: IndexProfile) -> ScanResult {
    let t0 = Instant::now();
    let buf = source.as_bytes();
    let mut builder = build_table.then(|| TableBuilder::new(buf));
    let indexed = indexer::index_with_table(buf, builder.as_mut(), profile);
    let table = builder.map(|b| b.into_table(Arc::clone(&source)));
    let scan_ms = t0.elapsed().as_secs_f64() * 1000.0;
    ScanResult {
        indexed,
        table,
        scan_ms,
    }
}
