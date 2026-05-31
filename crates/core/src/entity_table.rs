//! Generic `step_id → entity byte range` lookup for the whole DATA section.

use std::collections::HashMap;
use std::sync::Arc;

use crate::lexer::{data_section_start, endsec_position, for_each_record, Record};
use crate::source::IfcSource;

/// Byte ranges into the source buffer for one STEP record.
#[derive(Debug, Clone, Copy)]
pub struct EntityRefs {
    pub type_start: usize,
    pub type_len: u32,
    pub args_start: usize,
    pub args_len: u32,
}

/// Incremental builder used during a shared index+table scan.
pub struct TableBuilder {
    buf_ptr: usize,
    entries: HashMap<u64, EntityRefs>,
    order: Vec<u64>,
}

impl TableBuilder {
    pub fn new(buf: &[u8]) -> Self {
        Self {
            buf_ptr: buf.as_ptr() as usize,
            entries: HashMap::new(),
            order: Vec::new(),
        }
    }

    pub fn record(&mut self, rec: &Record<'_>) {
        let type_start = rec.type_name.as_ptr() as usize - self.buf_ptr;
        let args_start = rec.args.as_ptr() as usize - self.buf_ptr;
        if self
            .entries
            .insert(
                rec.id,
                EntityRefs {
                    type_start,
                    type_len: rec.type_name.len() as u32,
                    args_start,
                    args_len: rec.args.len() as u32,
                },
            )
            .is_none()
        {
            self.order.push(rec.id);
        }
    }

    pub fn into_table(self, source: Arc<IfcSource>) -> EntityTable {
        EntityTable {
            source,
            entries: self.entries,
            order: self.order,
        }
    }
}

/// Lookup table for every entity in the IFC's DATA section.
pub struct EntityTable {
    source: Arc<IfcSource>,
    entries: HashMap<u64, EntityRefs>,
    order: Vec<u64>,
}

impl EntityTable {
    /// Build from shared IFC bytes (zero-copy mmap when source is mmap'd).
    pub fn build(source: Arc<IfcSource>) -> Self {
        let buf = source.as_bytes();
        let mut builder = TableBuilder::new(buf);
        let data_start = data_section_start(buf).unwrap_or(0);
        let data_end = endsec_position(buf, data_start);
        for_each_record(buf, data_start, data_end, |rec| builder.record(&rec));
        builder.into_table(source)
    }

    /// Legacy helper: copies into an owned `IfcSource::Owned` buffer.
    pub fn build_from_slice(buf: &[u8]) -> Self {
        Self::build(Arc::new(IfcSource::Owned(buf.to_vec())))
    }

    pub fn source(&self) -> &Arc<IfcSource> {
        &self.source
    }

    fn buf(&self) -> &[u8] {
        self.source.as_bytes()
    }

    #[inline]
    pub fn get(&self, id: u64) -> Option<(&[u8], &[u8])> {
        let e = self.entries.get(&id)?;
        let buf = self.buf();
        let type_end = e.type_start + e.type_len as usize;
        let args_end = e.args_start + e.args_len as usize;
        Some((
            &buf[e.type_start..type_end],
            &buf[e.args_start..args_end],
        ))
    }

    #[inline]
    pub fn type_of(&self, id: u64) -> Option<&[u8]> {
        let e = self.entries.get(&id)?;
        let end = e.type_start + e.type_len as usize;
        Some(&self.buf()[e.type_start..end])
    }

    pub fn iter(&self) -> impl Iterator<Item = (u64, &[u8], &[u8])> + '_ {
        self.order.iter().filter_map(|id| {
            let e = self.entries.get(id)?;
            let buf = self.buf();
            let type_end = e.type_start + e.type_len as usize;
            let args_end = e.args_start + e.args_len as usize;
            Some((
                *id,
                &buf[e.type_start..type_end],
                &buf[e.args_start..args_end],
            ))
        })
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
