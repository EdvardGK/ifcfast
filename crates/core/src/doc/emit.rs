//! STEP re-serialiser for [`Doc`] (GH #124).
//!
//! Emits the header prefix verbatim, then each *kept* record's verbatim
//! byte span in source order, then the trailer verbatim. Because each
//! record's emit span runs to the next record's start (the last to
//! `ENDSEC`), the kept-everything case reproduces the source bytes
//! exactly — the Phase 1 byte-identity gate (`tests/doc_roundtrip.rs`).
//!
//! Subset (GH #124 Phase 2) supplies a `keep` set; dropping a record
//! also drops its trailing separator, so the survivors stay well-formed
//! STEP (each still ends in `;`). Records whose *args* are rewritten
//! (relationship member-set pruning) will be supplied as `overrides`
//! once Phase 2 lands; for now `emit` is verbatim-or-skip.

use std::collections::HashSet;

use super::Doc;

/// Summary of an emit pass, for caller-facing reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmitStats {
    /// Records present in the source document.
    pub records_in: usize,
    /// Records written to the output.
    pub records_out: usize,
    /// Output length in bytes.
    pub bytes_out: usize,
}

/// Serialise `doc` to STEP bytes.
///
/// `keep == None` emits every record (byte-identical to the source).
/// `keep == Some(set)` emits only records whose step_id is in `set`,
/// preserving original ids (sparse output) and source order.
pub fn emit(doc: &Doc, keep: Option<&HashSet<u64>>) -> (Vec<u8>, EmitStats) {
    let buf = doc.buf();
    let prefix_end = doc.prefix_end();
    let endsec = doc.endsec();

    let mut out = Vec::with_capacity(buf.len());
    // Header + section markers + any whitespace up to the first record.
    out.extend_from_slice(&buf[..prefix_end]);

    let mut records_out = 0usize;
    for (id, i) in doc.records() {
        if let Some(set) = keep {
            if !set.contains(&id) {
                continue;
            }
        }
        out.extend_from_slice(&buf[doc.record_span(i)]);
        records_out += 1;
    }

    // Trailer: ENDSEC; ... END-ISO-10303-21; verbatim.
    out.extend_from_slice(&buf[endsec..]);

    let stats = EmitStats {
        records_in: doc.len(),
        records_out,
        bytes_out: out.len(),
    };
    (out, stats)
}
