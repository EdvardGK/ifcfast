//! `clashes.parquet` writer.
//!
//! Writes one row per [`super::ClashPair`]. Column set is intentionally
//! lean — agents who need pset / storey / type context join back to
//! `instances.parquet` on `ifc_id_a` / `ifc_id_b` (or the `guid_*`
//! variants). Adding those columns here would duplicate the substrate
//! and force re-export on every change.
//!
//! Schema:
//!
//! | column             | type    | notes                                       |
//! |--------------------|---------|---------------------------------------------|
//! | `ifc_id_a`         | UInt64  | STEP entity id of the lower-ordered side    |
//! | `ifc_id_b`         | UInt64  | STEP entity id of the higher-ordered side   |
//! | `guid_a`           | Utf8    | IFC GUID of side A                          |
//! | `guid_b`           | Utf8    | IFC GUID of side B                          |
//! | `class_a`          | Utf8    | normalised class of side A (e.g. "Pipe")    |
//! | `class_b`          | Utf8    | normalised class of side B                  |
//! | `kind`             | Utf8    | "hard" or "clearance"                       |
//! | `min_distance_m`   | Float32 | 0.0 for hard clash, positive for clearance  |

use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{ArrayRef, Float32Builder, RecordBatch, StringBuilder, UInt64Builder};
use arrow::datatypes::{DataType, Field, Schema, SchemaRef};
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;

use super::engine::ClashPair;

fn build_clash_schema() -> Schema {
    Schema::new(vec![
        Field::new("ifc_id_a", DataType::UInt64, false),
        Field::new("ifc_id_b", DataType::UInt64, false),
        Field::new("guid_a", DataType::Utf8, false),
        Field::new("guid_b", DataType::Utf8, false),
        Field::new("class_a", DataType::Utf8, false),
        Field::new("class_b", DataType::Utf8, false),
        Field::new("kind", DataType::Utf8, false),
        Field::new("min_distance_m", DataType::Float32, false),
    ])
}

/// Write the report's pairs to `path` (e.g. `<bundle>/clashes.parquet`).
/// Overwrites any existing file. Writes zero rows when `pairs` is
/// empty — the file is still created so downstream queries can join
/// against it unconditionally.
pub fn write_clashes_parquet(path: &Path, pairs: &[ClashPair]) -> parquet::errors::Result<()> {
    let schema: SchemaRef = Arc::new(build_clash_schema());

    let file = File::create(path).map_err(|e| {
        parquet::errors::ParquetError::General(format!("create {}: {e}", path.display()))
    })?;

    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::default()))
        .set_dictionary_enabled(true)
        .build();

    let mut writer = ArrowWriter::try_new(file, schema.clone(), Some(props))?;

    if !pairs.is_empty() {
        let batch = build_batch(&schema, pairs)?;
        writer.write(&batch)?;
    }

    writer.close()?;
    Ok(())
}

fn build_batch(schema: &SchemaRef, pairs: &[ClashPair]) -> parquet::errors::Result<RecordBatch> {
    let n = pairs.len();
    let mut ifc_id_a = UInt64Builder::with_capacity(n);
    let mut ifc_id_b = UInt64Builder::with_capacity(n);
    let mut guid_a = StringBuilder::with_capacity(n, n * 22);
    let mut guid_b = StringBuilder::with_capacity(n, n * 22);
    let mut class_a = StringBuilder::with_capacity(n, n * 10);
    let mut class_b = StringBuilder::with_capacity(n, n * 10);
    let mut kind = StringBuilder::with_capacity(n, n * 8);
    let mut distance = Float32Builder::with_capacity(n);

    for p in pairs {
        ifc_id_a.append_value(p.ifc_id_a);
        ifc_id_b.append_value(p.ifc_id_b);
        guid_a.append_value(&p.guid_a);
        guid_b.append_value(&p.guid_b);
        class_a.append_value(&p.class_a);
        class_b.append_value(&p.class_b);
        kind.append_value(p.kind.as_str());
        distance.append_value(p.min_distance_m);
    }

    let columns: Vec<ArrayRef> = vec![
        Arc::new(ifc_id_a.finish()),
        Arc::new(ifc_id_b.finish()),
        Arc::new(guid_a.finish()),
        Arc::new(guid_b.finish()),
        Arc::new(class_a.finish()),
        Arc::new(class_b.finish()),
        Arc::new(kind.finish()),
        Arc::new(distance.finish()),
    ];

    RecordBatch::try_new(schema.clone(), columns).map_err(|e| {
        parquet::errors::ParquetError::General(format!("clashes batch: {e}"))
    })
}
