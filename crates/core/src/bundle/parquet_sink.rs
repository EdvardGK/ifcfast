//! Streaming Parquet sinks — two files emitted in one pass:
//!
//! - `representations.parquet` — one row per unique mesh shape, keyed
//!   by `rep_id`. The geometry payload (`vertices_le`, `indices_le`,
//!   `segments`) lives ONLY here. Subsequent instances pointing to
//!   the same rep_id are detected by an in-memory `HashSet<u64>` and
//!   the row write is elided.
//!
//! - `instances.parquet` — one row per `IfcProduct`. Geometry-free
//!   except for a `rep_id` foreign key, a 4x4 world transform, the
//!   world-coord AABB, and the per-instance semantic payload (psets,
//!   materials, quantities, classifications).
//!
//! Working-set RAM is bounded by:
//! - the rep-id dedup HashSet (≤ unique rep count — small for AEC
//!   files: 10s to low-thousands of unique shapes typical),
//! - one row-group buffer per file (default 1024 rows each).
//!
//! Joins via DuckDB are one-liners:
//!   `SELECT * FROM instances i LEFT JOIN representations r USING (rep_id);`

use std::collections::HashSet;
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

use crate::bundle::record::{pair_split, InstanceRecord, RepresentationRecord};
use crate::bundle::Bundle;
use crate::mesh::{ProductMesh, ProductSink};

const DEFAULT_ROW_GROUP_SIZE: usize = 1024;

/// Streaming substrate writer — fans each `ProductMesh` out into the
/// representations + instances Parquet files. Implements `ProductSink`
/// so the standard `mesh_ifc_streaming` pipeline drives it directly.
pub struct ParquetSink<'a> {
    bundle: &'a Bundle,
    rep_schema: SchemaRef,
    inst_schema: SchemaRef,
    rep_writer: ArrowWriter<File>,
    inst_writer: ArrowWriter<File>,
    /// `rep_id`s already written to `representations.parquet`. Once a
    /// rep_id is here, subsequent products referencing it skip the rep
    /// write and only emit an instance row. Bounded by unique rep
    /// count → small even on huge files.
    seen_reps: HashSet<u64>,
    row_group_size: usize,
    pending_reps: Vec<RepresentationRecord>,
    pending_insts: Vec<InstanceRecord>,
    products_written: usize,
    reps_written: usize,
}

impl<'a> ParquetSink<'a> {
    /// Open both Parquet files in `out_dir` for streaming write.
    /// Layout: `{out_dir}/representations.parquet` +
    /// `{out_dir}/instances.parquet`. The directory must already exist.
    pub fn create_in_dir<P: AsRef<Path>>(
        out_dir: P,
        bundle: &'a Bundle,
    ) -> parquet::errors::Result<Self> {
        let rep_path = out_dir.as_ref().join("representations.parquet");
        let inst_path = out_dir.as_ref().join("instances.parquet");

        let rep_schema = Arc::new(build_representation_schema());
        let inst_schema = Arc::new(build_instance_schema());

        let rep_file = File::create(&rep_path).map_err(|e| {
            parquet::errors::ParquetError::General(format!(
                "create {}: {e}",
                rep_path.display()
            ))
        })?;
        let inst_file = File::create(&inst_path).map_err(|e| {
            parquet::errors::ParquetError::General(format!(
                "create {}: {e}",
                inst_path.display()
            ))
        })?;

        // Zstd compresses IFC GUIDs and pset strings extremely well via
        // dictionary encoding; geometry blobs still benefit modestly
        // over snappy at negligible CPU cost.
        let props = WriterProperties::builder()
            .set_compression(Compression::ZSTD(ZstdLevel::default()))
            .set_dictionary_enabled(true)
            .build();

        let rep_writer = ArrowWriter::try_new(rep_file, rep_schema.clone(), Some(props.clone()))?;
        let inst_writer = ArrowWriter::try_new(inst_file, inst_schema.clone(), Some(props))?;

        Ok(Self {
            bundle,
            rep_schema,
            inst_schema,
            rep_writer,
            inst_writer,
            seen_reps: HashSet::new(),
            row_group_size: DEFAULT_ROW_GROUP_SIZE,
            pending_reps: Vec::with_capacity(DEFAULT_ROW_GROUP_SIZE),
            pending_insts: Vec::with_capacity(DEFAULT_ROW_GROUP_SIZE),
            products_written: 0,
            reps_written: 0,
        })
    }

    pub fn with_row_group_size(mut self, n: usize) -> Self {
        let n = n.max(1);
        self.row_group_size = n;
        self.pending_reps = Vec::with_capacity(n);
        self.pending_insts = Vec::with_capacity(n);
        self
    }

    pub fn products_written(&self) -> usize {
        self.products_written
    }

    pub fn unique_reps_written(&self) -> usize {
        self.reps_written
    }

    /// Drain pending records, write final row groups, close both files.
    /// Returns `(products_written, reps_written)`.
    pub fn finish(mut self) -> parquet::errors::Result<(usize, usize)> {
        if !self.pending_reps.is_empty() {
            self.flush_reps()?;
        }
        if !self.pending_insts.is_empty() {
            self.flush_insts()?;
        }
        self.rep_writer.close()?;
        self.inst_writer.close()?;
        Ok((self.products_written, self.reps_written))
    }

    fn flush_reps(&mut self) -> parquet::errors::Result<()> {
        let batch = build_rep_batch(&self.rep_schema, &self.pending_reps).map_err(|e| {
            parquet::errors::ParquetError::General(format!("build rep batch: {e}"))
        })?;
        self.rep_writer.write(&batch)?;
        self.pending_reps.clear();
        Ok(())
    }

    fn flush_insts(&mut self) -> parquet::errors::Result<()> {
        let batch = build_instance_batch(&self.inst_schema, &self.pending_insts).map_err(|e| {
            parquet::errors::ParquetError::General(format!("build instance batch: {e}"))
        })?;
        self.inst_writer.write(&batch)?;
        self.pending_insts.clear();
        Ok(())
    }
}

impl ProductSink for ParquetSink<'_> {
    fn wants_geometryless(&self) -> bool {
        // Substrate consumers need every IfcProduct as an instance row —
        // identity, placement, psets, materials, classifications — even
        // when the file has no body geometry for it. `pair_split` already
        // returns `rep_id = None` for empty meshes, and the instance
        // schema's `rep_id` column is nullable, so this just flips the
        // upstream filter so those products reach us.
        true
    }

    fn on_product(&mut self, mesh: ProductMesh) {
        let semantics = self.bundle.semantics_for(&mesh.guid);
        // IFC project's linear-unit-to-metres factor (the indexer
        // resolves this from IfcSIUnit + IfcConversionBasedUnit at
        // parse time). Missing → assume metres (1.0) so the QTO
        // numbers are still right when the file genuinely is
        // authored in metres.
        let unit_scale = self.bundle.unit_scale.unwrap_or(1.0) as f32;
        let (rep, inst) = pair_split(mesh, semantics, unit_scale);

        if let Some(rep_record) = rep {
            // Dedup by rep_id — only the first instance of a shared
            // shape gets its geometry written. The HashSet membership
            // check is what makes the substrate hierarchical.
            if self.seen_reps.insert(rep_record.rep_id) {
                self.pending_reps.push(rep_record);
                self.reps_written += 1;
                if self.pending_reps.len() >= self.row_group_size {
                    if let Err(e) = self.flush_reps() {
                        panic!("parquet rep row-group flush failed: {e}");
                    }
                }
            }
        }

        self.pending_insts.push(inst);
        self.products_written += 1;
        if self.pending_insts.len() >= self.row_group_size {
            if let Err(e) = self.flush_insts() {
                panic!("parquet instance row-group flush failed: {e}");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Schemas
// ---------------------------------------------------------------------------

fn xyz_field() -> Arc<Field> {
    // `FixedSizeListBuilder` materializes its inner item field as
    // nullable; mirror that here or RecordBatch validation fails.
    Arc::new(Field::new("item", DataType::Float32, true))
}

fn build_representation_schema() -> Schema {
    let segment_fields = Fields::from(vec![
        Field::new("source", DataType::Utf8, false),
        Field::new("index_start", DataType::UInt32, false),
        Field::new("triangle_count", DataType::UInt32, false),
    ]);
    let xyz = xyz_field();

    Schema::new(vec![
        Field::new("rep_id", DataType::UInt64, false),
        Field::new("source_kind", DataType::Utf8, false),
        Field::new("mesh_source", DataType::Utf8, false),
        Field::new("vertex_count", DataType::UInt32, false),
        Field::new("triangle_count", DataType::UInt32, false),
        Field::new("vertices_le", DataType::Binary, false),
        Field::new("indices_le", DataType::Binary, false),
        Field::new(
            "segments",
            DataType::List(Arc::new(Field::new(
                "item",
                DataType::Struct(segment_fields),
                true,
            ))),
            false,
        ),
        Field::new(
            "local_bbox_min_xyz",
            DataType::FixedSizeList(xyz.clone(), 3),
            false,
        ),
        Field::new(
            "local_bbox_max_xyz",
            DataType::FixedSizeList(xyz, 3),
            false,
        ),
    ])
}

fn build_instance_schema() -> Schema {
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
    let xyz = xyz_field();
    let mat4_item = Arc::new(Field::new("item", DataType::Float32, true));

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
        Field::new("rep_id", DataType::UInt64, true),
        Field::new("transform", DataType::FixedSizeList(mat4_item, 16), false),
        Field::new("bbox_min_xyz", DataType::FixedSizeList(xyz.clone(), 3), false),
        Field::new("bbox_max_xyz", DataType::FixedSizeList(xyz.clone(), 3), false),
        Field::new("placement_xyz", DataType::FixedSizeList(xyz, 3), false),
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
        // Geometric QTO — computed from the world-coord mesh in the
        // streaming pass. Always m² / m³; no conversion at query time.
        Field::new("volume_m3", DataType::Float32, false),
        Field::new("aabb_volume_m3", DataType::Float32, false),
        Field::new("surface_area_m2", DataType::Float32, false),
        Field::new("area_top_m2", DataType::Float32, false),
        Field::new("area_bottom_m2", DataType::Float32, false),
        Field::new("area_side_m2", DataType::Float32, false),
        Field::new("area_inclined_m2", DataType::Float32, false),
        Field::new("largest_surface_m2", DataType::Float32, false),
        Field::new("smallest_surface_m2", DataType::Float32, false),
        Field::new("surface_count", DataType::UInt32, false),
        // List<Struct> of every distinct planar surface, sorted by
        // area descending. DuckDB UNNEST(surfaces) turns it into a
        // row-per-face stream.
        Field::new(
            "surfaces",
            DataType::List(Arc::new(Field::new(
                "item",
                DataType::Struct(Fields::from(vec![
                    Field::new("area_m2", DataType::Float32, false),
                    Field::new("nx", DataType::Float32, false),
                    Field::new("ny", DataType::Float32, false),
                    Field::new("nz", DataType::Float32, false),
                ])),
                true,
            ))),
            false,
        ),
    ])
}

// ---------------------------------------------------------------------------
// Row-group batch builders
// ---------------------------------------------------------------------------

fn build_rep_batch(
    schema: &SchemaRef,
    records: &[RepresentationRecord],
) -> arrow::error::Result<RecordBatch> {
    let n = records.len();

    let mut rep_id = UInt64Builder::with_capacity(n);
    let mut source_kind = StringBuilder::with_capacity(n, n * 12);
    let mut mesh_source = StringBuilder::with_capacity(n, n * 16);
    let mut vertex_count = UInt32Builder::with_capacity(n);
    let mut triangle_count = UInt32Builder::with_capacity(n);
    let mut vertices_le = BinaryBuilder::with_capacity(n, n * 256);
    let mut indices_le = BinaryBuilder::with_capacity(n, n * 256);

    let segment_struct_fields = list_struct_fields(schema, "segments");
    let mut segments = ListBuilder::new(StructBuilder::from_fields(segment_struct_fields, 0));

    let bb_min_inner = Float32Builder::with_capacity(n * 3);
    let mut bb_min = FixedSizeListBuilder::new(bb_min_inner, 3);
    let bb_max_inner = Float32Builder::with_capacity(n * 3);
    let mut bb_max = FixedSizeListBuilder::new(bb_max_inner, 3);

    for r in records {
        rep_id.append_value(r.rep_id);
        source_kind.append_value(r.source_kind);
        mesh_source.append_value(&r.mesh_source);
        vertex_count.append_value(r.vertex_count);
        triangle_count.append_value(r.triangle_count);
        vertices_le.append_value(&r.vertices_le);
        indices_le.append_value(&r.indices_le);

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

        append_xyz(&mut bb_min, r.local_bbox_min_xyz);
        append_xyz(&mut bb_max, r.local_bbox_max_xyz);
    }

    let arrays: Vec<ArrayRef> = vec![
        Arc::new(rep_id.finish()),
        Arc::new(source_kind.finish()),
        Arc::new(mesh_source.finish()),
        Arc::new(vertex_count.finish()),
        Arc::new(triangle_count.finish()),
        Arc::new(vertices_le.finish()),
        Arc::new(indices_le.finish()),
        Arc::new(segments.finish()),
        Arc::new(bb_min.finish()),
        Arc::new(bb_max.finish()),
    ];

    RecordBatch::try_new(schema.clone(), arrays)
}

fn build_instance_batch(
    schema: &SchemaRef,
    records: &[InstanceRecord],
) -> arrow::error::Result<RecordBatch> {
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
    let mut rep_id = UInt64Builder::with_capacity(n);

    let xform_inner = Float32Builder::with_capacity(n * 16);
    let mut transform = FixedSizeListBuilder::new(xform_inner, 16);
    let bb_min_inner = Float32Builder::with_capacity(n * 3);
    let mut bb_min = FixedSizeListBuilder::new(bb_min_inner, 3);
    let bb_max_inner = Float32Builder::with_capacity(n * 3);
    let mut bb_max = FixedSizeListBuilder::new(bb_max_inner, 3);
    let placement_inner = Float32Builder::with_capacity(n * 3);
    let mut placement = FixedSizeListBuilder::new(placement_inner, 3);

    let material_struct_fields = list_struct_fields(schema, "materials");
    let pset_struct_fields = list_struct_fields(schema, "psets");
    let quantity_struct_fields = list_struct_fields(schema, "quantities");
    let classification_struct_fields = list_struct_fields(schema, "classifications");
    let surface_struct_fields = list_struct_fields(schema, "surfaces");

    let mut materials = ListBuilder::new(StructBuilder::from_fields(material_struct_fields, 0));
    let mut psets = ListBuilder::new(StructBuilder::from_fields(pset_struct_fields, 0));
    let mut quantities = ListBuilder::new(StructBuilder::from_fields(quantity_struct_fields, 0));
    let mut classifications =
        ListBuilder::new(StructBuilder::from_fields(classification_struct_fields, 0));
    let mut surfaces = ListBuilder::new(StructBuilder::from_fields(surface_struct_fields, 0));

    // QTO scalar columns. f32 is plenty of dynamic range for QTO
    // (a 1000 m² façade is 1e3; smallest meaningful surface is 1e-6 m²).
    let mut volume_m3 = Float32Builder::with_capacity(n);
    let mut aabb_volume_m3 = Float32Builder::with_capacity(n);
    let mut surface_area_m2 = Float32Builder::with_capacity(n);
    let mut area_top_m2 = Float32Builder::with_capacity(n);
    let mut area_bottom_m2 = Float32Builder::with_capacity(n);
    let mut area_side_m2 = Float32Builder::with_capacity(n);
    let mut area_inclined_m2 = Float32Builder::with_capacity(n);
    let mut largest_surface_m2 = Float32Builder::with_capacity(n);
    let mut smallest_surface_m2 = Float32Builder::with_capacity(n);
    let mut surface_count = UInt32Builder::with_capacity(n);

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
        match r.rep_id {
            Some(id) => rep_id.append_value(id),
            None => rep_id.append_null(),
        }

        // 4x4 transform — col-major, 16 f32s.
        for v in r.transform.iter() {
            transform.values().append_value(*v);
        }
        transform.append(true);

        append_xyz(&mut bb_min, r.bbox_min_xyz);
        append_xyz(&mut bb_max, r.bbox_max_xyz);
        append_xyz(&mut placement, r.placement_xyz);

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

        // QTO scalars.
        volume_m3.append_value(r.volume_m3);
        aabb_volume_m3.append_value(r.aabb_volume_m3);
        surface_area_m2.append_value(r.surface_area_m2);
        area_top_m2.append_value(r.area_top_m2);
        area_bottom_m2.append_value(r.area_bottom_m2);
        area_side_m2.append_value(r.area_side_m2);
        area_inclined_m2.append_value(r.area_inclined_m2);
        largest_surface_m2.append_value(r.largest_surface_m2);
        smallest_surface_m2.append_value(r.smallest_surface_m2);
        surface_count.append_value(r.surface_count);

        // Per-surface list — one row per distinct planar face.
        {
            let s = surfaces.values();
            for face in &r.surfaces {
                s.field_builder::<Float32Builder>(0).unwrap().append_value(face.area_m2);
                s.field_builder::<Float32Builder>(1).unwrap().append_value(face.nx);
                s.field_builder::<Float32Builder>(2).unwrap().append_value(face.ny);
                s.field_builder::<Float32Builder>(3).unwrap().append_value(face.nz);
                s.append(true);
            }
            surfaces.append(true);
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
        Arc::new(rep_id.finish()),
        Arc::new(transform.finish()),
        Arc::new(bb_min.finish()),
        Arc::new(bb_max.finish()),
        Arc::new(placement.finish()),
        Arc::new(materials.finish()),
        Arc::new(psets.finish()),
        Arc::new(quantities.finish()),
        Arc::new(classifications.finish()),
        Arc::new(volume_m3.finish()),
        Arc::new(aabb_volume_m3.finish()),
        Arc::new(surface_area_m2.finish()),
        Arc::new(area_top_m2.finish()),
        Arc::new(area_bottom_m2.finish()),
        Arc::new(area_side_m2.finish()),
        Arc::new(area_inclined_m2.finish()),
        Arc::new(largest_surface_m2.finish()),
        Arc::new(smallest_surface_m2.finish()),
        Arc::new(surface_count.finish()),
        Arc::new(surfaces.finish()),
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
