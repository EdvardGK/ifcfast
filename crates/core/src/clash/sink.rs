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
//! | `source_model_a`   | Utf8    | substrate `source_model` of side A (GH #50); "" on pre-v29 bundles. In a federated bundle join back to instances on `(ifc_id, source_model)` — bare `ifc_id`/`guid` can collide across constituent models |
//! | `source_model_b`   | Utf8    | substrate `source_model` of side B          |
//! | `kind`             | Utf8    | "hard" or "clearance"                       |
//! | `category`         | Utf8    | "clash", "insulation", "connection", "non_physical" — see [`super::engine::categorise`] for the rules |
//! | `min_distance_m`   | Float32 | 0.0 for hard clash, positive for clearance  |

use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{ArrayRef, Float32Builder, RecordBatch, StringBuilder, UInt64Builder};
use arrow::datatypes::{DataType, Field, Schema, SchemaRef};
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;

use crate::bundle::parquet_sink::temp_sibling_path;

use super::engine::ClashPair;

fn build_clash_schema() -> Schema {
    Schema::new(vec![
        Field::new("ifc_id_a", DataType::UInt64, false),
        Field::new("ifc_id_b", DataType::UInt64, false),
        Field::new("guid_a", DataType::Utf8, false),
        Field::new("guid_b", DataType::Utf8, false),
        Field::new("class_a", DataType::Utf8, false),
        Field::new("class_b", DataType::Utf8, false),
        Field::new("source_model_a", DataType::Utf8, false),
        Field::new("source_model_b", DataType::Utf8, false),
        Field::new("kind", DataType::Utf8, false),
        Field::new("category", DataType::Utf8, false),
        Field::new("min_distance_m", DataType::Float32, false),
    ])
}

/// Write the report's pairs to `path` (e.g. `<bundle>/clashes.parquet`).
/// Writes zero rows when `pairs` is empty — the file is still created so
/// downstream queries can join against it unconditionally.
///
/// Atomic (GH #151): rows stream into a `<name>.tmp.<pid>` sibling and
/// the file is renamed over `path` only after a clean close. A failure
/// anywhere removes the staging file and leaves the previous report
/// exactly as it was — never a readable-but-truncated replacement.
pub fn write_clashes_parquet(path: &Path, pairs: &[ClashPair]) -> parquet::errors::Result<()> {
    let tmp = temp_sibling_path(path);
    match write_staged(&tmp, pairs) {
        Ok(()) => match std::fs::rename(&tmp, path) {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                Err(parquet::errors::ParquetError::General(format!(
                    "publish {}: {e}",
                    path.display()
                )))
            }
        },
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// The whole write, against the staging path. Every failure path leaves
/// cleanup to the caller.
fn write_staged(path: &Path, pairs: &[ClashPair]) -> parquet::errors::Result<()> {
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
    let mut source_model_a = StringBuilder::with_capacity(n, n * 12);
    let mut source_model_b = StringBuilder::with_capacity(n, n * 12);
    let mut kind = StringBuilder::with_capacity(n, n * 8);
    let mut category = StringBuilder::with_capacity(n, n * 12);
    let mut distance = Float32Builder::with_capacity(n);

    for p in pairs {
        ifc_id_a.append_value(p.ifc_id_a);
        ifc_id_b.append_value(p.ifc_id_b);
        guid_a.append_value(&p.guid_a);
        guid_b.append_value(&p.guid_b);
        class_a.append_value(&p.class_a);
        class_b.append_value(&p.class_b);
        source_model_a.append_value(&p.source_model_a);
        source_model_b.append_value(&p.source_model_b);
        kind.append_value(p.kind.as_str());
        category.append_value(p.category.as_str());
        distance.append_value(p.min_distance_m);
    }

    let columns: Vec<ArrayRef> = vec![
        Arc::new(ifc_id_a.finish()),
        Arc::new(ifc_id_b.finish()),
        Arc::new(guid_a.finish()),
        Arc::new(guid_b.finish()),
        Arc::new(class_a.finish()),
        Arc::new(class_b.finish()),
        Arc::new(source_model_a.finish()),
        Arc::new(source_model_b.finish()),
        Arc::new(kind.finish()),
        Arc::new(category.finish()),
        Arc::new(distance.finish()),
    ];

    RecordBatch::try_new(schema.clone(), columns)
        .map_err(|e| parquet::errors::ParquetError::General(format!("clashes batch: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ifcfast-sink-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn successful_write_publishes_and_leaves_no_staging_file() {
        let dir = scratch_dir("ok");
        let path = dir.join("clashes.parquet");
        write_clashes_parquet(&path, &[]).expect("write");
        assert!(path.exists(), "report published");
        assert!(
            !temp_sibling_path(&path).exists(),
            "staging file must be renamed away, not left behind"
        );
    }

    #[test]
    fn failed_write_leaves_the_previous_report_intact() {
        // GH #151: the old failure mode replaced a good report with a
        // truncated one. Publish a good file, then block the staging
        // path with a directory so the write cannot even open it.
        let dir = scratch_dir("fail");
        let path = dir.join("clashes.parquet");
        write_clashes_parquet(&path, &[]).expect("first write");
        let before = std::fs::read(&path).expect("read published report");
        assert!(!before.is_empty());

        let tmp = temp_sibling_path(&path);
        std::fs::create_dir(&tmp).expect("block the staging path");
        let err = write_clashes_parquet(&path, &[]).expect_err("staging blocked → must fail");
        assert!(
            err.to_string().contains("create"),
            "error must name the failing create: {err}"
        );

        assert_eq!(
            std::fs::read(&path).expect("previous report still readable"),
            before,
            "a failed write must not touch the previously published report"
        );
    }
}
