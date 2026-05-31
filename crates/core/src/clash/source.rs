//! Substrate reader for the clash engine.
//!
//! Reads `instances.parquet` + `representations.parquet` into the
//! minimal Rust structs the engine needs. Intentionally scoped: we
//! only decode the columns clash actually touches (`ifc_id`, `guid`,
//! `class`, `rep_id`, `transform`, `bbox_min/max_xyz`, plus the rep's
//! `source_kind` + binary triangle buffers). Adding the full row would
//! pull psets / materials / qto into memory for no benefit.

use std::collections::HashMap;
use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{
    Array, BinaryArray, FixedSizeListArray, Float32Array, StringArray, UInt32Array, UInt64Array,
};
use arrow::datatypes::Schema;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

/// A single instance row, narrowed to what the clash engine needs.
#[derive(Debug, Clone)]
pub struct InstanceRow {
    pub ifc_id: u64,
    pub guid: String,
    pub class: String,
    /// `None` for geometryless products — broad-phase skips them.
    pub rep_id: Option<u64>,
    /// Column-major 4×4. For `composite` reps this is identity (rep is
    /// already world-baked); for `shared_or_direct` reps this maps
    /// rep-local → world.
    pub transform: [f32; 16],
    pub bbox_min: [f32; 3],
    pub bbox_max: [f32; 3],
}

/// A single representation row, narrowed to what the clash engine needs.
#[derive(Debug, Clone)]
pub struct RepresentationRow {
    pub rep_id: u64,
    /// `"shared_or_direct"` (local-frame mesh; instance transform
    /// applies) or `"composite"` (world-baked mesh; instance transform
    /// is identity).
    pub source_kind: String,
    /// Decoded flat-buffer vertices: `[x, y, z, x, y, z, …]`.
    pub vertices: Vec<f32>,
    /// Decoded flat-buffer indices: `[i0, i1, i2, …]` (three per triangle).
    pub indices: Vec<u32>,
}

#[derive(Debug)]
pub enum SubstrateReadError {
    Io(std::io::Error),
    Parquet(parquet::errors::ParquetError),
    Arrow(arrow::error::ArrowError),
    MissingColumn(&'static str),
    UnexpectedColumnType(&'static str),
    InconsistentBinary(&'static str),
}

impl std::fmt::Display for SubstrateReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io: {e}"),
            Self::Parquet(e) => write!(f, "parquet: {e}"),
            Self::Arrow(e) => write!(f, "arrow: {e}"),
            Self::MissingColumn(c) => write!(f, "missing required column `{c}`"),
            Self::UnexpectedColumnType(c) => write!(f, "column `{c}` has unexpected type"),
            Self::InconsistentBinary(c) => {
                write!(f, "column `{c}` has malformed binary buffer (length not aligned)")
            }
        }
    }
}

impl std::error::Error for SubstrateReadError {}

impl From<std::io::Error> for SubstrateReadError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<parquet::errors::ParquetError> for SubstrateReadError {
    fn from(e: parquet::errors::ParquetError) -> Self {
        Self::Parquet(e)
    }
}
impl From<arrow::error::ArrowError> for SubstrateReadError {
    fn from(e: arrow::error::ArrowError) -> Self {
        Self::Arrow(e)
    }
}

/// Read the `ifcfast.unit_scale` value from the parquet file's schema
/// metadata. Returns `1.0` when the metadata is missing (older bundles
/// written before unit_scale was recorded). Caller multiplies any
/// source-unit value (vertex coord, bbox extent) by this to convert
/// to metres.
pub fn read_unit_scale(path: &Path) -> Result<f64, SubstrateReadError> {
    let file = File::open(path)?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
    Ok(extract_unit_scale(builder.schema()))
}

fn extract_unit_scale(schema: &Arc<Schema>) -> f64 {
    schema
        .metadata()
        .get("ifcfast.unit_scale")
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(1.0)
}

/// Read every instance row from `instances.parquet`. Bbox extents are
/// converted from source units to metres using the file's
/// `ifcfast.unit_scale` schema metadata, so the returned struct is
/// already in metres throughout.
pub fn read_instances(path: &Path) -> Result<Vec<InstanceRow>, SubstrateReadError> {
    let file = File::open(path)?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
    let unit_scale = extract_unit_scale(builder.schema()) as f32;
    let reader = builder.build()?;

    let mut out: Vec<InstanceRow> = Vec::new();
    for batch in reader {
        let batch = batch?;
        let n = batch.num_rows();

        let ifc_id = column_u64(&batch, "ifc_id")?;
        let guid = column_string(&batch, "guid")?;
        let class = column_string(&batch, "class")?;
        let rep_id = column_u64_opt(&batch, "rep_id")?;
        let transform = column_fixed_f32(&batch, "transform", 16)?;
        let bbox_min = column_fixed_f32(&batch, "bbox_min_xyz", 3)?;
        let bbox_max = column_fixed_f32(&batch, "bbox_max_xyz", 3)?;

        for i in 0..n {
            // Transform's rotation block stays unitless; only the
            // translation column (entries 12..14, column-major) carries
            // source-unit length. Scale that block to metres so the
            // engine sees a consistent metre-frame transform.
            let mut t = [0.0f32; 16];
            t.copy_from_slice(&transform[i * 16..(i + 1) * 16]);
            t[12] *= unit_scale;
            t[13] *= unit_scale;
            t[14] *= unit_scale;

            let mut bmin = [0.0f32; 3];
            for (k, v) in bbox_min[i * 3..(i + 1) * 3].iter().enumerate() {
                bmin[k] = *v * unit_scale;
            }
            let mut bmax = [0.0f32; 3];
            for (k, v) in bbox_max[i * 3..(i + 1) * 3].iter().enumerate() {
                bmax[k] = *v * unit_scale;
            }

            out.push(InstanceRow {
                ifc_id: ifc_id[i],
                guid: guid[i].clone(),
                class: class[i].clone(),
                rep_id: rep_id[i],
                transform: t,
                bbox_min: bmin,
                bbox_max: bmax,
            });
        }
    }
    Ok(out)
}

/// Read every representation row from `representations.parquet`, keyed
/// by `rep_id` for O(1) lookup from the instance loop. Vertex buffers
/// are converted from source units to metres using the file's
/// `ifcfast.unit_scale` schema metadata.
pub fn read_representations(
    path: &Path,
) -> Result<HashMap<u64, RepresentationRow>, SubstrateReadError> {
    let file = File::open(path)?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
    let unit_scale = extract_unit_scale(builder.schema()) as f32;
    let reader = builder.build()?;

    let mut out: HashMap<u64, RepresentationRow> = HashMap::new();
    for batch in reader {
        let batch = batch?;
        let n = batch.num_rows();

        let rep_id = column_u64(&batch, "rep_id")?;
        let source_kind = column_string(&batch, "source_kind")?;
        let vertex_count = column_u32(&batch, "vertex_count")?;
        let triangle_count = column_u32(&batch, "triangle_count")?;
        let vertices_le = column_binary(&batch, "vertices_le")?;
        let indices_le = column_binary(&batch, "indices_le")?;

        for i in 0..n {
            let vraw = vertices_le[i];
            let iraw = indices_le[i];
            if vraw.len() != (vertex_count[i] as usize) * 3 * 4 {
                return Err(SubstrateReadError::InconsistentBinary("vertices_le"));
            }
            if iraw.len() != (triangle_count[i] as usize) * 3 * 4 {
                return Err(SubstrateReadError::InconsistentBinary("indices_le"));
            }
            let mut vertices: Vec<f32> = Vec::with_capacity(vraw.len() / 4);
            for c in vraw.chunks_exact(4) {
                vertices.push(f32::from_le_bytes([c[0], c[1], c[2], c[3]]) * unit_scale);
            }
            let mut indices: Vec<u32> = Vec::with_capacity(iraw.len() / 4);
            for c in iraw.chunks_exact(4) {
                indices.push(u32::from_le_bytes([c[0], c[1], c[2], c[3]]));
            }

            out.insert(
                rep_id[i],
                RepresentationRow {
                    rep_id: rep_id[i],
                    source_kind: source_kind[i].clone(),
                    vertices,
                    indices,
                },
            );
        }
    }
    Ok(out)
}

// ---------- column accessors -----------------------------------------

fn column_u64(
    batch: &arrow::record_batch::RecordBatch,
    name: &'static str,
) -> Result<Vec<u64>, SubstrateReadError> {
    let arr = batch
        .column_by_name(name)
        .ok_or(SubstrateReadError::MissingColumn(name))?;
    let arr = arr
        .as_any()
        .downcast_ref::<UInt64Array>()
        .ok_or(SubstrateReadError::UnexpectedColumnType(name))?;
    Ok((0..arr.len()).map(|i| arr.value(i)).collect())
}

fn column_u64_opt(
    batch: &arrow::record_batch::RecordBatch,
    name: &'static str,
) -> Result<Vec<Option<u64>>, SubstrateReadError> {
    let arr = batch
        .column_by_name(name)
        .ok_or(SubstrateReadError::MissingColumn(name))?;
    let arr = arr
        .as_any()
        .downcast_ref::<UInt64Array>()
        .ok_or(SubstrateReadError::UnexpectedColumnType(name))?;
    Ok((0..arr.len())
        .map(|i| if arr.is_null(i) { None } else { Some(arr.value(i)) })
        .collect())
}

fn column_u32(
    batch: &arrow::record_batch::RecordBatch,
    name: &'static str,
) -> Result<Vec<u32>, SubstrateReadError> {
    let arr = batch
        .column_by_name(name)
        .ok_or(SubstrateReadError::MissingColumn(name))?;
    let arr = arr
        .as_any()
        .downcast_ref::<UInt32Array>()
        .ok_or(SubstrateReadError::UnexpectedColumnType(name))?;
    Ok((0..arr.len()).map(|i| arr.value(i)).collect())
}

fn column_string(
    batch: &arrow::record_batch::RecordBatch,
    name: &'static str,
) -> Result<Vec<String>, SubstrateReadError> {
    let arr = batch
        .column_by_name(name)
        .ok_or(SubstrateReadError::MissingColumn(name))?;
    let arr = arr
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or(SubstrateReadError::UnexpectedColumnType(name))?;
    Ok((0..arr.len()).map(|i| arr.value(i).to_string()).collect())
}

fn column_binary<'a>(
    batch: &'a arrow::record_batch::RecordBatch,
    name: &'static str,
) -> Result<Vec<&'a [u8]>, SubstrateReadError> {
    let arr = batch
        .column_by_name(name)
        .ok_or(SubstrateReadError::MissingColumn(name))?;
    let arr = arr
        .as_any()
        .downcast_ref::<BinaryArray>()
        .ok_or(SubstrateReadError::UnexpectedColumnType(name))?;
    Ok((0..arr.len()).map(|i| arr.value(i)).collect())
}

/// Decode a `FixedSizeList<Float32, expected_size>` column into a flat
/// `Vec<f32>` of length `n_rows × expected_size`.
fn column_fixed_f32(
    batch: &arrow::record_batch::RecordBatch,
    name: &'static str,
    expected_size: usize,
) -> Result<Vec<f32>, SubstrateReadError> {
    let arr = batch
        .column_by_name(name)
        .ok_or(SubstrateReadError::MissingColumn(name))?;
    let list = arr
        .as_any()
        .downcast_ref::<FixedSizeListArray>()
        .ok_or(SubstrateReadError::UnexpectedColumnType(name))?;
    if list.value_length() as usize != expected_size {
        return Err(SubstrateReadError::UnexpectedColumnType(name));
    }
    let values = list
        .values()
        .as_any()
        .downcast_ref::<Float32Array>()
        .ok_or(SubstrateReadError::UnexpectedColumnType(name))?;
    let n_rows = batch.num_rows();
    let mut out = Vec::with_capacity(n_rows * expected_size);
    for i in 0..(n_rows * expected_size) {
        out.push(values.value(i));
    }
    Ok(out)
}

