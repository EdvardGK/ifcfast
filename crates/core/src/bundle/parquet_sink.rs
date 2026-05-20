//! Streaming Parquet sink — consumes `ProductMesh` from the mesh
//! pipeline, pairs each with semantics from the [`Bundle`], buffers
//! `row_group_size` records into Arrow builders, then flushes one
//! row group to Parquet and frees the builders.
//!
//! **RAM bound.** Working-set ≈ one row-group worth of records. With
//! the default `row_group_size = 1024`, that's at most ~1024 product
//! meshes plus their semantic payload in flight at once — independent
//! of the source IFC's size. The Sannergata OOM scenario (144k
//! products, 1 GB IFC) writes ~141 row groups sequentially with the
//! same peak footprint as writing one.

use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{
    ArrayRef, BinaryBuilder, FixedSizeListBuilder, Float32Builder, Float64Builder, Int32Builder,
    ListBuilder, RecordBatch, StringBuilder, StructBuilder, UInt32Builder, UInt64Builder,
};
use arrow::datatypes::{DataType, Field, Fields, Schema, SchemaRef};
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;

use crate::bundle::record::ProductRecord;
use crate::bundle::Bundle;
use crate::mesh::{ProductMesh, ProductSink};

const DEFAULT_ROW_GROUP_SIZE: usize = 1024;

/// Streaming Parquet writer for the per-product substrate. Implements
/// [`ProductSink`] so the standard `mesh_ifc_streaming` pipeline drives
/// it directly.
pub struct ParquetSink<'a> {
    bundle: &'a Bundle,
    schema: SchemaRef,
    writer: ArrowWriter<File>,
    row_group_size: usize,
    pending: Vec<ProductRecord>,
    products_written: usize,
}

impl<'a> ParquetSink<'a> {
    /// Open a Parquet file at `path` for streaming write. Picks zstd
    /// compression by default — IFC semantic strings (pset names,
    /// material names, GUIDs) dictionary-encode extremely well, and
    /// zstd gives ~3-5× better ratio than snappy on geometry blobs at
    /// negligible CPU cost.
    pub fn create<P: AsRef<Path>>(path: P, bundle: &'a Bundle) -> parquet::errors::Result<Self> {
        let schema = Arc::new(build_schema());
        let file = File::create(path).map_err(|e| {
            parquet::errors::ParquetError::General(format!("create parquet: {e}"))
        })?;
        let props = WriterProperties::builder()
            .set_compression(Compression::ZSTD(ZstdLevel::default()))
            .set_dictionary_enabled(true)
            .build();
        let writer = ArrowWriter::try_new(file, schema.clone(), Some(props))?;
        Ok(Self {
            bundle,
            schema,
            writer,
            row_group_size: DEFAULT_ROW_GROUP_SIZE,
            pending: Vec::with_capacity(DEFAULT_ROW_GROUP_SIZE),
            products_written: 0,
        })
    }

    pub fn with_row_group_size(mut self, n: usize) -> Self {
        self.row_group_size = n.max(1);
        self.pending = Vec::with_capacity(self.row_group_size);
        self
    }

    pub fn products_written(&self) -> usize {
        self.products_written
    }

    /// Drain pending records, write final row group, close the file.
    pub fn finish(mut self) -> parquet::errors::Result<usize> {
        if !self.pending.is_empty() {
            self.flush_row_group()?;
        }
        self.writer.close()?;
        Ok(self.products_written)
    }

    fn flush_row_group(&mut self) -> parquet::errors::Result<()> {
        let batch = build_batch(&self.schema, &self.pending).map_err(|e| {
            parquet::errors::ParquetError::General(format!("build record batch: {e}"))
        })?;
        self.writer.write(&batch)?;
        self.pending.clear();
        Ok(())
    }
}

impl ProductSink for ParquetSink<'_> {
    fn on_product(&mut self, mesh: ProductMesh) {
        let semantics = self.bundle.semantics_for(&mesh.guid);
        let record = ProductRecord::pair(mesh, semantics);
        self.pending.push(record);
        self.products_written += 1;
        if self.pending.len() >= self.row_group_size {
            // Sinks can't return errors through the trait — surface as
            // panic so a write failure halts the pipeline immediately
            // (better than silently dropping rows once the file is
            // partially flushed). Callers should catch errors at
            // `finish()` for clean ones, or wrap in `catch_unwind` if
            // they want fault tolerance on the streaming side.
            if let Err(e) = self.flush_row_group() {
                panic!("parquet row-group flush failed: {e}");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

fn build_schema() -> Schema {
    let segment_fields = Fields::from(vec![
        Field::new("source", DataType::Utf8, false),
        Field::new("index_start", DataType::UInt32, false),
        Field::new("triangle_count", DataType::UInt32, false),
    ]);
    let material_fields = Fields::from(vec![
        Field::new("role", DataType::Utf8, false),
        Field::new("layer_index", DataType::Int32, false),
        Field::new("name", DataType::Utf8, true),
        Field::new("thickness_mm", DataType::Float64, true),
        Field::new("category", DataType::Utf8, true),
    ]);
    let pset_fields = Fields::from(vec![
        Field::new("set_name", DataType::Utf8, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("value", DataType::Utf8, true),
        Field::new("value_type", DataType::Utf8, true),
    ]);
    let quantity_fields = Fields::from(vec![
        Field::new("set_name", DataType::Utf8, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("value", DataType::Utf8, true),
        Field::new("quantity_type", DataType::Utf8, false),
        Field::new("unit_step_id", DataType::UInt64, true),
    ]);
    let classification_fields = Fields::from(vec![
        Field::new("system_name", DataType::Utf8, true),
        Field::new("edition", DataType::Utf8, true),
        Field::new("identification", DataType::Utf8, true),
        Field::new("name", DataType::Utf8, true),
        Field::new("location", DataType::Utf8, true),
        Field::new("source", DataType::Utf8, true),
    ]);

    // `FixedSizeListBuilder` materializes its inner item field as
    // nullable by default. Mirror that in the schema or `RecordBatch`
    // validation rejects the batch with a nullability mismatch.
    let xyz_field = Arc::new(Field::new("item", DataType::Float32, true));

    Schema::new(vec![
        Field::new("ifc_id", DataType::UInt64, false),
        Field::new("guid", DataType::Utf8, false),
        Field::new("class", DataType::Utf8, false),
        Field::new("source_class", DataType::Utf8, false),
        Field::new("name", DataType::Utf8, true),
        Field::new("predefined_type", DataType::Utf8, true),
        Field::new("object_type", DataType::Utf8, true),
        Field::new("tag", DataType::Utf8, true),
        Field::new("storey_guid", DataType::Utf8, true),
        Field::new("storey_name", DataType::Utf8, true),
        Field::new("aggregates_parent_guid", DataType::Utf8, true),
        Field::new("type_guid", DataType::Utf8, true),
        Field::new("type_name", DataType::Utf8, true),
        Field::new("placement_xyz", DataType::FixedSizeList(xyz_field.clone(), 3), false),
        Field::new("bbox_min_xyz", DataType::FixedSizeList(xyz_field.clone(), 3), false),
        Field::new("bbox_max_xyz", DataType::FixedSizeList(xyz_field.clone(), 3), false),
        Field::new("vertex_count", DataType::UInt32, false),
        Field::new("triangle_count", DataType::UInt32, false),
        Field::new("vertices_le", DataType::Binary, false),
        Field::new("indices_le", DataType::Binary, false),
        Field::new("mesh_source", DataType::Utf8, false),
        Field::new(
            "segments",
            // `ListBuilder` materializes its inner item field as
            // nullable; mirror that here so RecordBatch validation
            // passes. The list itself is non-nullable (every product
            // gets one, possibly-empty list per column).
            DataType::List(Arc::new(Field::new(
                "item",
                DataType::Struct(segment_fields),
                true,
            ))),
            false,
        ),
        Field::new(
            "materials",
            DataType::List(Arc::new(Field::new(
                "item",
                DataType::Struct(material_fields),
                true,
            ))),
            false,
        ),
        Field::new(
            "psets",
            DataType::List(Arc::new(Field::new(
                "item",
                DataType::Struct(pset_fields),
                true,
            ))),
            false,
        ),
        Field::new(
            "quantities",
            DataType::List(Arc::new(Field::new(
                "item",
                DataType::Struct(quantity_fields),
                true,
            ))),
            false,
        ),
        Field::new(
            "classifications",
            DataType::List(Arc::new(Field::new(
                "item",
                DataType::Struct(classification_fields),
                true,
            ))),
            false,
        ),
    ])
}

// ---------------------------------------------------------------------------
// Row-group batch builder
// ---------------------------------------------------------------------------

fn build_batch(schema: &SchemaRef, records: &[ProductRecord]) -> arrow::error::Result<RecordBatch> {
    let n = records.len();

    let mut ifc_id = UInt64Builder::with_capacity(n);
    let mut guid = StringBuilder::with_capacity(n, n * 22);
    let mut class = StringBuilder::with_capacity(n, n * 12);
    let mut source_class = StringBuilder::with_capacity(n, n * 20);
    let mut name = StringBuilder::with_capacity(n, n * 24);
    let mut predefined_type = StringBuilder::with_capacity(n, n * 8);
    let mut object_type = StringBuilder::with_capacity(n, n * 16);
    let mut tag = StringBuilder::with_capacity(n, n * 8);
    let mut storey_guid = StringBuilder::with_capacity(n, n * 22);
    let mut storey_name = StringBuilder::with_capacity(n, n * 16);
    let mut agg_parent = StringBuilder::with_capacity(n, n * 22);
    let mut type_guid = StringBuilder::with_capacity(n, n * 22);
    let mut type_name = StringBuilder::with_capacity(n, n * 24);

    let placement_inner = Float32Builder::with_capacity(n * 3);
    let mut placement = FixedSizeListBuilder::new(placement_inner, 3);
    let bbox_min_inner = Float32Builder::with_capacity(n * 3);
    let mut bbox_min = FixedSizeListBuilder::new(bbox_min_inner, 3);
    let bbox_max_inner = Float32Builder::with_capacity(n * 3);
    let mut bbox_max = FixedSizeListBuilder::new(bbox_max_inner, 3);

    let mut vertex_count = UInt32Builder::with_capacity(n);
    let mut triangle_count = UInt32Builder::with_capacity(n);
    let mut vertices_le = BinaryBuilder::with_capacity(n, n * 256);
    let mut indices_le = BinaryBuilder::with_capacity(n, n * 256);
    let mut mesh_source = StringBuilder::with_capacity(n, n * 16);

    // Pre-built struct field schemas — needed to construct the inner
    // StructBuilders with the right child columns. Pulling them off
    // the top-level schema keeps schema+builder in sync if we add a
    // column later.
    let segment_struct_fields = list_struct_fields(schema, "segments");
    let material_struct_fields = list_struct_fields(schema, "materials");
    let pset_struct_fields = list_struct_fields(schema, "psets");
    let quantity_struct_fields = list_struct_fields(schema, "quantities");
    let classification_struct_fields = list_struct_fields(schema, "classifications");

    let mut segments = ListBuilder::new(StructBuilder::from_fields(segment_struct_fields, 0));
    let mut materials = ListBuilder::new(StructBuilder::from_fields(material_struct_fields, 0));
    let mut psets = ListBuilder::new(StructBuilder::from_fields(pset_struct_fields, 0));
    let mut quantities = ListBuilder::new(StructBuilder::from_fields(quantity_struct_fields, 0));
    let mut classifications =
        ListBuilder::new(StructBuilder::from_fields(classification_struct_fields, 0));

    for r in records {
        ifc_id.append_value(r.ifc_id);
        guid.append_value(&r.guid);
        class.append_value(&r.class);
        source_class.append_value(&r.source_class);
        append_opt(&mut name, r.name.as_deref());
        append_opt(&mut predefined_type, r.predefined_type.as_deref());
        append_opt(&mut object_type, r.object_type.as_deref());
        append_opt(&mut tag, r.tag.as_deref());
        append_opt(&mut storey_guid, r.storey_guid.as_deref());
        append_opt(&mut storey_name, r.storey_name.as_deref());
        append_opt(&mut agg_parent, r.aggregates_parent_guid.as_deref());
        append_opt(&mut type_guid, r.type_guid.as_deref());
        append_opt(&mut type_name, r.type_name.as_deref());

        append_xyz(&mut placement, r.placement_xyz);
        append_xyz(&mut bbox_min, r.bbox_min_xyz);
        append_xyz(&mut bbox_max, r.bbox_max_xyz);

        vertex_count.append_value(r.vertex_count);
        triangle_count.append_value(r.triangle_count);
        vertices_le.append_value(&r.vertices_le);
        indices_le.append_value(&r.indices_le);
        mesh_source.append_value(&r.mesh_source);

        // Segments
        {
            let s = segments.values();
            for seg in &r.segments {
                s.field_builder::<StringBuilder>(0).unwrap().append_value(&seg.source);
                s.field_builder::<UInt32Builder>(1).unwrap().append_value(seg.index_start);
                s.field_builder::<UInt32Builder>(2).unwrap().append_value(seg.triangle_count);
                s.append(true);
            }
            segments.append(true);
        }

        // Materials
        {
            let m = materials.values();
            for entry in &r.materials {
                m.field_builder::<StringBuilder>(0).unwrap().append_value(entry.role);
                m.field_builder::<Int32Builder>(1).unwrap().append_value(entry.layer_index);
                append_opt(
                    m.field_builder::<StringBuilder>(2).unwrap(),
                    entry.name.as_deref(),
                );
                let f = m.field_builder::<Float64Builder>(3).unwrap();
                match entry.thickness_mm {
                    Some(v) => f.append_value(v),
                    None => f.append_null(),
                }
                append_opt(
                    m.field_builder::<StringBuilder>(4).unwrap(),
                    entry.category.as_deref(),
                );
                m.append(true);
            }
            materials.append(true);
        }

        // Psets
        {
            let p = psets.values();
            for entry in &r.psets {
                p.field_builder::<StringBuilder>(0).unwrap().append_value(&entry.set_name);
                p.field_builder::<StringBuilder>(1).unwrap().append_value(&entry.name);
                append_opt(
                    p.field_builder::<StringBuilder>(2).unwrap(),
                    entry.value.as_deref(),
                );
                append_opt(
                    p.field_builder::<StringBuilder>(3).unwrap(),
                    entry.value_type.as_deref(),
                );
                p.append(true);
            }
            psets.append(true);
        }

        // Quantities
        {
            let q = quantities.values();
            for entry in &r.quantities {
                q.field_builder::<StringBuilder>(0).unwrap().append_value(&entry.set_name);
                q.field_builder::<StringBuilder>(1).unwrap().append_value(&entry.name);
                append_opt(
                    q.field_builder::<StringBuilder>(2).unwrap(),
                    entry.value.as_deref(),
                );
                q.field_builder::<StringBuilder>(3).unwrap().append_value(&entry.quantity_type);
                let u = q.field_builder::<UInt64Builder>(4).unwrap();
                match entry.unit_step_id {
                    Some(v) => u.append_value(v),
                    None => u.append_null(),
                }
                q.append(true);
            }
            quantities.append(true);
        }

        // Classifications
        {
            let c = classifications.values();
            for entry in &r.classifications {
                append_opt(
                    c.field_builder::<StringBuilder>(0).unwrap(),
                    entry.system_name.as_deref(),
                );
                append_opt(
                    c.field_builder::<StringBuilder>(1).unwrap(),
                    entry.edition.as_deref(),
                );
                append_opt(
                    c.field_builder::<StringBuilder>(2).unwrap(),
                    entry.identification.as_deref(),
                );
                append_opt(
                    c.field_builder::<StringBuilder>(3).unwrap(),
                    entry.name.as_deref(),
                );
                append_opt(
                    c.field_builder::<StringBuilder>(4).unwrap(),
                    entry.location.as_deref(),
                );
                append_opt(
                    c.field_builder::<StringBuilder>(5).unwrap(),
                    entry.source.as_deref(),
                );
                c.append(true);
            }
            classifications.append(true);
        }
    }

    let arrays: Vec<ArrayRef> = vec![
        Arc::new(ifc_id.finish()),
        Arc::new(guid.finish()),
        Arc::new(class.finish()),
        Arc::new(source_class.finish()),
        Arc::new(name.finish()),
        Arc::new(predefined_type.finish()),
        Arc::new(object_type.finish()),
        Arc::new(tag.finish()),
        Arc::new(storey_guid.finish()),
        Arc::new(storey_name.finish()),
        Arc::new(agg_parent.finish()),
        Arc::new(type_guid.finish()),
        Arc::new(type_name.finish()),
        Arc::new(placement.finish()),
        Arc::new(bbox_min.finish()),
        Arc::new(bbox_max.finish()),
        Arc::new(vertex_count.finish()),
        Arc::new(triangle_count.finish()),
        Arc::new(vertices_le.finish()),
        Arc::new(indices_le.finish()),
        Arc::new(mesh_source.finish()),
        Arc::new(segments.finish()),
        Arc::new(materials.finish()),
        Arc::new(psets.finish()),
        Arc::new(quantities.finish()),
        Arc::new(classifications.finish()),
    ];

    RecordBatch::try_new(schema.clone(), arrays)
}

fn list_struct_fields(schema: &SchemaRef, list_col: &str) -> Fields {
    let field = schema
        .field_with_name(list_col)
        .unwrap_or_else(|_| panic!("schema missing list column {list_col}"));
    match field.data_type() {
        DataType::List(inner) => match inner.data_type() {
            DataType::Struct(fields) => fields.clone(),
            other => panic!("{list_col} list item is not a struct: {other:?}"),
        },
        other => panic!("{list_col} is not a List: {other:?}"),
    }
}

fn append_opt(b: &mut StringBuilder, v: Option<&str>) {
    match v {
        Some(s) => b.append_value(s),
        None => b.append_null(),
    }
}

fn append_xyz(b: &mut FixedSizeListBuilder<Float32Builder>, xyz: [f32; 3]) {
    b.values().append_value(xyz[0]);
    b.values().append_value(xyz[1]);
    b.values().append_value(xyz[2]);
    b.append(true);
}
