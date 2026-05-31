//! `_ifcfast` — fast native IFC parsing and data extraction.
//!
//! Public surface:
//!
//!   * [`entity_table::EntityTable`] — the shared byte-range lookup over
//!     the IFC's DATA section. Every extractor walks this table once.
//!   * [`indexer`]                    — tier-1: products, storeys, sites,
//!     buildings, projects, spaces, containment + aggregate relationships.
//!   * [`extractors`]                 — the four data-layer extractors:
//!     property sets, element quantities, material assignments,
//!     classification references.
//!   * [`mesh`]                       — geometry pipeline: extrusions,
//!     mapped items, polygonal / triangulated face sets, faceted BREP,
//!     glTF / OBJ output, per-product geometric stats, drift detector.
//!     Behind the `mesh` Cargo feature (default-on in maturin builds).
//!
//! With the `python` feature enabled (the default, used by maturin),
//! this crate exposes a `_ifcfast` Python extension. Without it, the
//! crate is pure-Rust — used by the standalone `ifcfast-bench` and
//! `ifcfast-mesh` binaries.

pub mod entity_table;
pub mod extractors;
pub mod indexer;
pub mod lexer;
pub mod source;

#[cfg(feature = "mesh")]
pub mod mesh;

#[cfg(feature = "bundle")]
pub mod bundle;

#[cfg(feature = "geom")]
pub mod geom;

#[cfg(feature = "clash")]
pub mod clash;

#[cfg(feature = "python")]
mod python {
    use std::path::Path;
    use std::time::Instant;

    use pyo3::prelude::*;
    use pyo3::types::{PyDict, PyList};
    #[cfg(feature = "mesh")]
    use pyo3::types::PyBytes;

    use crate::indexer;
    use crate::source::IfcSource;

    // ----- index_ifc ----------------------------------------------------

    #[pyfunction]
    fn index_ifc<'py>(py: Python<'py>, path: &str) -> PyResult<Bound<'py, PyDict>> {
        let (mmap, open_ms) = open_mmap(path)?;

        let t_index = Instant::now();
        let idx = py.allow_threads(|| indexer::index(&mmap));
        let index_ms = t_index.elapsed().as_secs_f64() * 1000.0;

        let t_marshal = Instant::now();

        let dict = PyDict::new_bound(py);
        dict.set_item("schema", &idx.schema)?;
        dict.set_item("project_name", &idx.project_name)?;
        dict.set_item("authoring_app", &idx.authoring_app)?;
        dict.set_item("unit_scale", idx.unit_scale)?;
        dict.set_item("size_bytes", mmap.len() as u64)?;
        dict.set_item("open_ms", open_ms)?;
        dict.set_item("index_ms", index_ms)?;

        let tc = PyDict::new_bound(py);
        for (k, v) in &idx.type_counts {
            tc.set_item(k, v)?;
        }
        dict.set_item("type_counts", tc)?;

        let products = PyDict::new_bound(py);
        products.set_item("step_id", PyList::new_bound(py, &idx.product_step_id))?;
        products.set_item("guid", PyList::new_bound(py, &idx.product_guid))?;
        products.set_item("entity", PyList::new_bound(py, &idx.product_entity))?;
        products.set_item("name", PyList::new_bound(py, &idx.product_name))?;
        products.set_item(
            "predefined_type",
            PyList::new_bound(py, &idx.product_predefined_type),
        )?;
        products.set_item("object_type", PyList::new_bound(py, &idx.product_object_type))?;
        products.set_item("tag", PyList::new_bound(py, &idx.product_tag))?;
        dict.set_item("products", products)?;

        let storeys = PyDict::new_bound(py);
        storeys.set_item("step_id", PyList::new_bound(py, &idx.storey_step_id))?;
        storeys.set_item("guid", PyList::new_bound(py, &idx.storey_guid))?;
        storeys.set_item("name", PyList::new_bound(py, &idx.storey_name))?;
        storeys.set_item("elevation", PyList::new_bound(py, &idx.storey_elevation))?;
        storeys.set_item(
            "building_step_id",
            PyList::new_bound(py, &idx.storey_building_step_id),
        )?;
        dict.set_item("storeys", storeys)?;

        let contained = PyDict::new_bound(py);
        contained.set_item("child", PyList::new_bound(py, &idx.contained_in_child))?;
        contained.set_item("structure", PyList::new_bound(py, &idx.contained_in_structure))?;
        dict.set_item("contained_in", contained)?;

        let agg = PyDict::new_bound(py);
        agg.set_item("child", PyList::new_bound(py, &idx.aggregates_child))?;
        agg.set_item("parent", PyList::new_bound(py, &idx.aggregates_parent))?;
        dict.set_item("aggregates", agg)?;

        let sb = PyDict::new_bound(py);
        sb.set_item("storey", PyList::new_bound(py, &idx.storey_building_storey))?;
        sb.set_item("building", PyList::new_bound(py, &idx.storey_building_building))?;
        dict.set_item("storey_building", sb)?;

        let voids = PyDict::new_bound(py);
        voids.set_item("opening", PyList::new_bound(py, &idx.voids_opening))?;
        voids.set_item("host", PyList::new_bound(py, &idx.voids_host))?;
        dict.set_item("voids", voids)?;

        // IfcRelDefinesByType: (product_step_id, type_step_id) pairs, plus
        // the IfcTypeObject table that lets Python resolve type_step_id to
        // (type_guid, type_name, type_entity).
        let dbt = PyDict::new_bound(py);
        dbt.set_item("product", PyList::new_bound(py, &idx.defines_by_type_product))?;
        dbt.set_item("type", PyList::new_bound(py, &idx.defines_by_type_type))?;
        dict.set_item("defines_by_type", dbt)?;

        let types = PyDict::new_bound(py);
        types.set_item("step_id", PyList::new_bound(py, &idx.type_object_step_id))?;
        types.set_item("entity", PyList::new_bound(py, &idx.type_object_entity))?;
        types.set_item("guid", PyList::new_bound(py, &idx.type_object_guid))?;
        types.set_item("name", PyList::new_bound(py, &idx.type_object_name))?;
        dict.set_item("type_objects", types)?;

        let site_ids: Vec<u64> = idx.site_step_id_to_guid.keys().copied().collect();
        let site_guids: Vec<&str> = site_ids
            .iter()
            .map(|i| idx.site_step_id_to_guid.get(i).unwrap().as_str())
            .collect();
        let sites = PyDict::new_bound(py);
        sites.set_item("step_id", PyList::new_bound(py, site_ids))?;
        sites.set_item("guid", PyList::new_bound(py, site_guids))?;
        dict.set_item("sites", sites)?;

        let bldg_ids: Vec<u64> = idx.building_step_id_to_guid.keys().copied().collect();
        let bldg_guids: Vec<&str> = bldg_ids
            .iter()
            .map(|i| idx.building_step_id_to_guid.get(i).unwrap().as_str())
            .collect();
        let buildings = PyDict::new_bound(py);
        buildings.set_item("step_id", PyList::new_bound(py, bldg_ids))?;
        buildings.set_item("guid", PyList::new_bound(py, bldg_guids))?;
        dict.set_item("buildings", buildings)?;

        let proj_ids: Vec<u64> = idx.project_step_id_to_guid.keys().copied().collect();
        let proj_guids: Vec<&str> = proj_ids
            .iter()
            .map(|i| idx.project_step_id_to_guid.get(i).unwrap().as_str())
            .collect();
        let projects = PyDict::new_bound(py);
        projects.set_item("step_id", PyList::new_bound(py, proj_ids))?;
        projects.set_item("guid", PyList::new_bound(py, proj_guids))?;
        dict.set_item("projects", projects)?;

        let space_ids: Vec<u64> = idx.space_step_id_to_guid.keys().copied().collect();
        let space_guids: Vec<&str> = space_ids
            .iter()
            .map(|i| idx.space_step_id_to_guid.get(i).unwrap().as_str())
            .collect();
        let spaces = PyDict::new_bound(py);
        spaces.set_item("step_id", PyList::new_bound(py, space_ids))?;
        spaces.set_item("guid", PyList::new_bound(py, space_guids))?;
        dict.set_item("spaces", spaces)?;

        let marshal_ms = t_marshal.elapsed().as_secs_f64() * 1000.0;
        dict.set_item("marshal_ms", marshal_ms)?;
        Ok(dict)
    }

    // ----- shared GUID-index helper used by every extractor below ------

    fn build_guid_index(table: &crate::entity_table::EntityTable) -> std::collections::HashMap<u64, String> {
        let mut step_to_guid: std::collections::HashMap<u64, String> =
            std::collections::HashMap::with_capacity(64_000);
        for (sid, type_name, args) in table.iter() {
            if !type_name.starts_with(b"IFC") {
                continue;
            }
            let fields = crate::lexer::split_top_level_args(args);
            if let Some(first) = fields.first() {
                if let crate::lexer::Field::String(s) = crate::lexer::parse_field(first) {
                    if s.len() == 22 {
                        step_to_guid.insert(sid, s);
                    }
                }
            }
        }
        step_to_guid
    }

    /// Load an IFC source for the PyO3 layer. Dispatches on magic
    /// bytes via [`crate::source::open`]: plain `.ifc` → mmap (zero
    /// copy), `.ifczip` → decompressed owned buffer. Either variant
    /// derefs to `&[u8]` so callers don't change.
    fn open_mmap(path: &str) -> PyResult<(IfcSource, f64)> {
        let t_open = Instant::now();
        let src = crate::source::open(Path::new(path))
            .map_err(|e| pyo3::exceptions::PyIOError::new_err(format!("open {path}: {e}")))?;
        Ok((src, t_open.elapsed().as_secs_f64() * 1000.0))
    }

    // ----- extract_psets -----------------------------------------------

    #[pyfunction]
    pub fn extract_psets<'py>(py: Python<'py>, path: &str) -> PyResult<Bound<'py, PyDict>> {
        let (mmap, open_ms) = open_mmap(path)?;
        let t_table = Instant::now();
        let table = crate::entity_table::EntityTable::build(&mmap);
        let table_ms = t_table.elapsed().as_secs_f64() * 1000.0;
        let t_guids = Instant::now();
        let step_to_guid = build_guid_index(&table);
        let guid_ms = t_guids.elapsed().as_secs_f64() * 1000.0;
        let t_psets = Instant::now();
        let psets = py.allow_threads(|| crate::extractors::psets::build(&table, &step_to_guid));
        let pset_ms = t_psets.elapsed().as_secs_f64() * 1000.0;

        let t_marshal = Instant::now();
        let out = PyDict::new_bound(py);
        out.set_item("guid", PyList::new_bound(py, &psets.guid))?;
        out.set_item("pset_name", PyList::new_bound(py, &psets.pset_name))?;
        out.set_item("prop_name", PyList::new_bound(py, &psets.prop_name))?;
        out.set_item("value", PyList::new_bound(py, &psets.value))?;
        out.set_item("value_type", PyList::new_bound(py, &psets.value_type))?;
        out.set_item("open_ms", open_ms)?;
        out.set_item("entity_table_ms", table_ms)?;
        out.set_item("guid_index_ms", guid_ms)?;
        out.set_item("pset_extract_ms", pset_ms)?;
        out.set_item("marshal_ms", t_marshal.elapsed().as_secs_f64() * 1000.0)?;
        out.set_item("size_bytes", mmap.len() as u64)?;
        Ok(out)
    }

    // ----- extract_quantities ------------------------------------------

    #[pyfunction]
    pub fn extract_quantities<'py>(py: Python<'py>, path: &str) -> PyResult<Bound<'py, PyDict>> {
        let (mmap, open_ms) = open_mmap(path)?;
        let t_table = Instant::now();
        let table = crate::entity_table::EntityTable::build(&mmap);
        let table_ms = t_table.elapsed().as_secs_f64() * 1000.0;
        let t_guids = Instant::now();
        let step_to_guid = build_guid_index(&table);
        let guid_ms = t_guids.elapsed().as_secs_f64() * 1000.0;
        let t_qto = Instant::now();
        let qto = py.allow_threads(|| crate::extractors::quantities::build(&table, &step_to_guid));
        let qto_ms = t_qto.elapsed().as_secs_f64() * 1000.0;

        let t_marshal = Instant::now();
        let out = PyDict::new_bound(py);
        out.set_item("guid", PyList::new_bound(py, &qto.guid))?;
        out.set_item("qto_name", PyList::new_bound(py, &qto.qto_name))?;
        out.set_item("quantity_name", PyList::new_bound(py, &qto.quantity_name))?;
        out.set_item("value", PyList::new_bound(py, &qto.value))?;
        out.set_item("quantity_type", PyList::new_bound(py, &qto.quantity_type))?;
        out.set_item("unit_step_id", PyList::new_bound(py, &qto.unit_step_id))?;
        out.set_item("open_ms", open_ms)?;
        out.set_item("entity_table_ms", table_ms)?;
        out.set_item("guid_index_ms", guid_ms)?;
        out.set_item("qto_extract_ms", qto_ms)?;
        out.set_item("marshal_ms", t_marshal.elapsed().as_secs_f64() * 1000.0)?;
        out.set_item("size_bytes", mmap.len() as u64)?;
        Ok(out)
    }

    // ----- extract_materials -------------------------------------------

    #[pyfunction]
    pub fn extract_materials<'py>(py: Python<'py>, path: &str) -> PyResult<Bound<'py, PyDict>> {
        let (mmap, open_ms) = open_mmap(path)?;
        let t_table = Instant::now();
        let table = crate::entity_table::EntityTable::build(&mmap);
        let table_ms = t_table.elapsed().as_secs_f64() * 1000.0;
        let t_guids = Instant::now();
        let step_to_guid = build_guid_index(&table);
        let guid_ms = t_guids.elapsed().as_secs_f64() * 1000.0;
        let t_mat = Instant::now();
        let mats = py.allow_threads(|| {
            let unit_scale = crate::indexer::extract_unit_scale(&table).unwrap_or(1.0);
            crate::extractors::materials::build(&table, &step_to_guid, unit_scale)
        });
        let mat_ms = t_mat.elapsed().as_secs_f64() * 1000.0;

        let t_marshal = Instant::now();
        let out = PyDict::new_bound(py);
        out.set_item("guid", PyList::new_bound(py, &mats.guid))?;
        out.set_item("role", PyList::new_bound(py, &mats.role))?;
        out.set_item("layer_index", PyList::new_bound(py, &mats.layer_index))?;
        out.set_item("material_name", PyList::new_bound(py, &mats.material_name))?;
        out.set_item("fraction", PyList::new_bound(py, &mats.fraction))?;
        out.set_item(
            "layer_thickness_mm",
            PyList::new_bound(py, &mats.layer_thickness_mm),
        )?;
        out.set_item("category", PyList::new_bound(py, &mats.category))?;
        out.set_item("open_ms", open_ms)?;
        out.set_item("entity_table_ms", table_ms)?;
        out.set_item("guid_index_ms", guid_ms)?;
        out.set_item("materials_extract_ms", mat_ms)?;
        out.set_item("marshal_ms", t_marshal.elapsed().as_secs_f64() * 1000.0)?;
        out.set_item("size_bytes", mmap.len() as u64)?;
        Ok(out)
    }

    // ----- extract_classifications -------------------------------------

    #[pyfunction]
    pub fn extract_classifications<'py>(
        py: Python<'py>,
        path: &str,
    ) -> PyResult<Bound<'py, PyDict>> {
        let (mmap, open_ms) = open_mmap(path)?;
        let t_table = Instant::now();
        let table = crate::entity_table::EntityTable::build(&mmap);
        let table_ms = t_table.elapsed().as_secs_f64() * 1000.0;
        let t_guids = Instant::now();
        let step_to_guid = build_guid_index(&table);
        let guid_ms = t_guids.elapsed().as_secs_f64() * 1000.0;
        let t_cls = Instant::now();
        let cls = py.allow_threads(|| crate::extractors::classifications::build(&table, &step_to_guid));
        let cls_ms = t_cls.elapsed().as_secs_f64() * 1000.0;

        let t_marshal = Instant::now();
        let out = PyDict::new_bound(py);
        out.set_item("guid", PyList::new_bound(py, &cls.guid))?;
        out.set_item("system_name", PyList::new_bound(py, &cls.system_name))?;
        out.set_item("edition", PyList::new_bound(py, &cls.edition))?;
        out.set_item("identification", PyList::new_bound(py, &cls.identification))?;
        out.set_item("name", PyList::new_bound(py, &cls.name))?;
        out.set_item("location", PyList::new_bound(py, &cls.location))?;
        out.set_item("source", PyList::new_bound(py, &cls.source))?;
        out.set_item("open_ms", open_ms)?;
        out.set_item("entity_table_ms", table_ms)?;
        out.set_item("guid_index_ms", guid_ms)?;
        out.set_item("classifications_extract_ms", cls_ms)?;
        out.set_item("marshal_ms", t_marshal.elapsed().as_secs_f64() * 1000.0)?;
        out.set_item("size_bytes", mmap.len() as u64)?;
        Ok(out)
    }

    // ----- extract_all -------------------------------------------------

    /// All four extractors against a single shared EntityTable. 2-3× faster
    /// than calling them individually on big files.
    #[pyfunction]
    pub fn extract_all<'py>(py: Python<'py>, path: &str) -> PyResult<Bound<'py, PyDict>> {
        let t_total = Instant::now();
        let (mmap, open_ms) = open_mmap(path)?;
        let t_table = Instant::now();
        let table = crate::entity_table::EntityTable::build(&mmap);
        let table_ms = t_table.elapsed().as_secs_f64() * 1000.0;
        let t_guids = Instant::now();
        let step_to_guid = build_guid_index(&table);
        let guid_ms = t_guids.elapsed().as_secs_f64() * 1000.0;

        let (psets, quantities, materials, classifications,
             pset_ms, qto_ms, mat_ms, cls_ms) =
            py.allow_threads(|| {
                // Materials needs the project's linear-unit scale to
                // normalize LayerThickness to mm. Cheap walk over the
                // table for IfcUnitAssignment + IfcSIUnit only — much
                // less work than a full indexer pass.
                let unit_scale =
                    crate::indexer::extract_unit_scale(&table).unwrap_or(1.0);
                let t = Instant::now();
                let p = crate::extractors::psets::build(&table, &step_to_guid);
                let pt = t.elapsed().as_secs_f64() * 1000.0;
                let t = Instant::now();
                let q = crate::extractors::quantities::build(&table, &step_to_guid);
                let qt = t.elapsed().as_secs_f64() * 1000.0;
                let t = Instant::now();
                let m = crate::extractors::materials::build(&table, &step_to_guid, unit_scale);
                let mt = t.elapsed().as_secs_f64() * 1000.0;
                let t = Instant::now();
                let c = crate::extractors::classifications::build(&table, &step_to_guid);
                let ct = t.elapsed().as_secs_f64() * 1000.0;
                (p, q, m, c, pt, qt, mt, ct)
            });

        let t_marshal = Instant::now();
        let out = PyDict::new_bound(py);
        {
            let d = PyDict::new_bound(py);
            d.set_item("guid", PyList::new_bound(py, &psets.guid))?;
            d.set_item("pset_name", PyList::new_bound(py, &psets.pset_name))?;
            d.set_item("prop_name", PyList::new_bound(py, &psets.prop_name))?;
            d.set_item("value", PyList::new_bound(py, &psets.value))?;
            d.set_item("value_type", PyList::new_bound(py, &psets.value_type))?;
            out.set_item("psets", d)?;
        }
        {
            let d = PyDict::new_bound(py);
            d.set_item("guid", PyList::new_bound(py, &quantities.guid))?;
            d.set_item("qto_name", PyList::new_bound(py, &quantities.qto_name))?;
            d.set_item("quantity_name", PyList::new_bound(py, &quantities.quantity_name))?;
            d.set_item("value", PyList::new_bound(py, &quantities.value))?;
            d.set_item("quantity_type", PyList::new_bound(py, &quantities.quantity_type))?;
            d.set_item("unit_step_id", PyList::new_bound(py, &quantities.unit_step_id))?;
            out.set_item("quantities", d)?;
        }
        {
            let d = PyDict::new_bound(py);
            d.set_item("guid", PyList::new_bound(py, &materials.guid))?;
            d.set_item("role", PyList::new_bound(py, &materials.role))?;
            d.set_item("layer_index", PyList::new_bound(py, &materials.layer_index))?;
            d.set_item("material_name", PyList::new_bound(py, &materials.material_name))?;
            d.set_item(
                "layer_thickness_mm",
                PyList::new_bound(py, &materials.layer_thickness_mm),
            )?;
            d.set_item("category", PyList::new_bound(py, &materials.category))?;
            d.set_item("fraction", PyList::new_bound(py, &materials.fraction))?;
            out.set_item("materials", d)?;
        }
        {
            let d = PyDict::new_bound(py);
            d.set_item("guid", PyList::new_bound(py, &classifications.guid))?;
            d.set_item("system_name", PyList::new_bound(py, &classifications.system_name))?;
            d.set_item("edition", PyList::new_bound(py, &classifications.edition))?;
            d.set_item("identification", PyList::new_bound(py, &classifications.identification))?;
            d.set_item("name", PyList::new_bound(py, &classifications.name))?;
            d.set_item("location", PyList::new_bound(py, &classifications.location))?;
            d.set_item("source", PyList::new_bound(py, &classifications.source))?;
            out.set_item("classifications", d)?;
        }
        let marshal_ms = t_marshal.elapsed().as_secs_f64() * 1000.0;
        let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;
        out.set_item("open_ms", open_ms)?;
        out.set_item("entity_table_ms", table_ms)?;
        out.set_item("guid_index_ms", guid_ms)?;
        out.set_item("psets_extract_ms", pset_ms)?;
        out.set_item("quantities_extract_ms", qto_ms)?;
        out.set_item("materials_extract_ms", mat_ms)?;
        out.set_item("classifications_extract_ms", cls_ms)?;
        out.set_item("marshal_ms", marshal_ms)?;
        out.set_item("total_ms", total_ms)?;
        out.set_item("size_bytes", mmap.len() as u64)?;
        Ok(out)
    }

    // ----- analyse_drift (mesh-only) -----------------------------------

    #[cfg(feature = "mesh")]
    /// Per-product geometric QTO — runs the streaming mesh pass and
    /// computes volume / surface area / orientation-bucketed area /
    /// distinct planar surfaces for every meshed product in one
    /// O(triangles) sweep. Output is in m² / m³ (the IFC's
    /// unit_scale is applied at compute time).
    ///
    /// Returns a PyDict with two parallel views:
    ///   * **Per-product columns** (one row per meshed product):
    ///     guid, entity, volume_m3, aabb_volume_m3, surface_area_m2,
    ///     area_top_m2, area_bottom_m2, area_side_m2, area_inclined_m2,
    ///     largest_surface_m2, smallest_surface_m2, surface_count.
    ///   * **Per-surface long-format** (one row per (product, distinct
    ///     planar surface)): surface_guid, surface_index, area_m2,
    ///     nx, ny, nz. Sort within a product is area-descending.
    ///
    /// Author-supplied `IfcElementQuantity` values are NOT consulted
    /// here — when present they live in `m.quantities` and remain
    /// the gold-standard QTO source. This function is the geometric
    /// truth that survives when authors omit Qto_* sets.
    #[cfg(feature = "mesh")]
    #[pyfunction]
    pub fn mesh_qto<'py>(py: Python<'py>, path: &str) -> PyResult<Bound<'py, PyDict>> {
        use crate::mesh::{
            mesh_ifc_streaming_framed, qto, BakeFrame, ProductMesh, ProductSink,
        };

        let t_total = Instant::now();
        let (mmap, _open_ms) = open_mmap(path)?;

        // Project unit-scale (mm files: 0.001; metre files: 1.0).
        // Pulled from the indexer — same source the bundle pre-pass
        // uses. None → assume metres so geometry-derived numbers stay
        // sane on schema-incomplete files.
        let t_idx = Instant::now();
        let idx = py.allow_threads(|| indexer::index(&mmap));
        let idx_ms = t_idx.elapsed().as_secs_f64() * 1000.0;
        let unit_scale = idx.unit_scale.unwrap_or(1.0) as f32;

        // Per-product accumulator sink. One row per meshed product
        // landing in `products`; one row per distinct planar surface
        // landing in `surfaces`. Avoids holding the meshes themselves
        // — drops each ProductMesh after computing its QTO.
        struct QtoSink {
            unit_scale: f32,
            guid: Vec<String>,
            entity: Vec<String>,
            volume_m3: Vec<f32>,
            aabb_volume_m3: Vec<f32>,
            surface_area_m2: Vec<f32>,
            area_top_m2: Vec<f32>,
            area_bottom_m2: Vec<f32>,
            area_side_m2: Vec<f32>,
            area_inclined_m2: Vec<f32>,
            largest_surface_m2: Vec<f32>,
            smallest_surface_m2: Vec<f32>,
            surface_count: Vec<u32>,
            // Long-format per-surface columns.
            s_guid: Vec<String>,
            s_index: Vec<u32>,
            s_area_m2: Vec<f32>,
            s_nx: Vec<f32>,
            s_ny: Vec<f32>,
            s_nz: Vec<f32>,
        }
        impl ProductSink for QtoSink {
            fn on_product(&mut self, mesh: ProductMesh) {
                let q = qto::compute(&mesh.vertices, &mesh.indices, self.unit_scale);
                self.guid.push(mesh.guid.clone());
                self.entity.push(mesh.entity.clone());
                self.volume_m3.push(q.volume_m3.abs());
                self.aabb_volume_m3.push(q.aabb_volume_m3);
                self.surface_area_m2.push(q.surface_area_m2);
                self.area_top_m2.push(q.area_top_m2);
                self.area_bottom_m2.push(q.area_bottom_m2);
                self.area_side_m2.push(q.area_side_m2);
                self.area_inclined_m2.push(q.area_inclined_m2);
                self.largest_surface_m2.push(q.largest_surface_m2);
                self.smallest_surface_m2.push(q.smallest_surface_m2);
                self.surface_count.push(q.surface_count);
                for (i, s) in q.surfaces.iter().enumerate() {
                    self.s_guid.push(mesh.guid.clone());
                    self.s_index.push(i as u32);
                    self.s_area_m2.push(s.area_m2);
                    self.s_nx.push(s.nx);
                    self.s_ny.push(s.ny);
                    self.s_nz.push(s.nz);
                }
            }
        }
        let mut sink = QtoSink {
            unit_scale,
            guid: Vec::with_capacity(idx.product_step_id.len()),
            entity: Vec::with_capacity(idx.product_step_id.len()),
            volume_m3: Vec::with_capacity(idx.product_step_id.len()),
            aabb_volume_m3: Vec::with_capacity(idx.product_step_id.len()),
            surface_area_m2: Vec::with_capacity(idx.product_step_id.len()),
            area_top_m2: Vec::with_capacity(idx.product_step_id.len()),
            area_bottom_m2: Vec::with_capacity(idx.product_step_id.len()),
            area_side_m2: Vec::with_capacity(idx.product_step_id.len()),
            area_inclined_m2: Vec::with_capacity(idx.product_step_id.len()),
            largest_surface_m2: Vec::with_capacity(idx.product_step_id.len()),
            smallest_surface_m2: Vec::with_capacity(idx.product_step_id.len()),
            surface_count: Vec::with_capacity(idx.product_step_id.len()),
            s_guid: Vec::new(),
            s_index: Vec::new(),
            s_area_m2: Vec::new(),
            s_nx: Vec::new(),
            s_ny: Vec::new(),
            s_nz: Vec::new(),
        };

        let t_mesh = Instant::now();
        // Local frame: QTO is translation-invariant, so meshing the shape
        // near origin gives correct volume/area/orientation AND stays
        // precise for far-from-origin objects (georeferenced MEP) that
        // would otherwise collapse into an f32-quantised point and report
        // surface_count = 0.
        let mesh_stats =
            py.allow_threads(|| mesh_ifc_streaming_framed(&mmap, &mut sink, BakeFrame::Local));
        let mesh_ms = t_mesh.elapsed().as_secs_f64() * 1000.0;

        let out = PyDict::new_bound(py);
        out.set_item("guid", PyList::new_bound(py, sink.guid))?;
        out.set_item("entity", PyList::new_bound(py, sink.entity))?;
        out.set_item("volume_m3", PyList::new_bound(py, sink.volume_m3))?;
        out.set_item("aabb_volume_m3", PyList::new_bound(py, sink.aabb_volume_m3))?;
        out.set_item("surface_area_m2", PyList::new_bound(py, sink.surface_area_m2))?;
        out.set_item("area_top_m2", PyList::new_bound(py, sink.area_top_m2))?;
        out.set_item("area_bottom_m2", PyList::new_bound(py, sink.area_bottom_m2))?;
        out.set_item("area_side_m2", PyList::new_bound(py, sink.area_side_m2))?;
        out.set_item("area_inclined_m2", PyList::new_bound(py, sink.area_inclined_m2))?;
        out.set_item("largest_surface_m2", PyList::new_bound(py, sink.largest_surface_m2))?;
        out.set_item("smallest_surface_m2", PyList::new_bound(py, sink.smallest_surface_m2))?;
        out.set_item("surface_count", PyList::new_bound(py, sink.surface_count))?;
        out.set_item("surface_guid", PyList::new_bound(py, sink.s_guid))?;
        out.set_item("surface_index", PyList::new_bound(py, sink.s_index))?;
        out.set_item("surface_area_m2_long", PyList::new_bound(py, sink.s_area_m2))?;
        out.set_item("surface_nx", PyList::new_bound(py, sink.s_nx))?;
        out.set_item("surface_ny", PyList::new_bound(py, sink.s_ny))?;
        out.set_item("surface_nz", PyList::new_bound(py, sink.s_nz))?;
        out.set_item("unit_scale", unit_scale as f64)?;
        out.set_item("indexer_ms", idx_ms)?;
        out.set_item("mesh_ms", mesh_ms)?;
        out.set_item("entity_table_ms", mesh_stats.entity_table_build_ms)?;
        out.set_item("total_ms", t_total.elapsed().as_secs_f64() * 1000.0)?;
        out.set_item("products_meshed", mesh_stats.products_meshed)?;
        out.set_item("size_bytes", mmap.len() as u64)?;
        Ok(out)
    }

    #[cfg(feature = "mesh")]
    #[pyfunction]
    pub fn analyse_drift<'py>(py: Python<'py>, path: &str) -> PyResult<Bound<'py, PyDict>> {
        let t_total = Instant::now();
        let (mmap, _open_ms) = open_mmap(path)?;
        let (meshes, mesh_stats) = py.allow_threads(|| crate::mesh::mesh_ifc(&mmap));
        let prod_stats: Vec<crate::mesh::stats::ProductStats> = meshes
            .iter()
            .map(crate::mesh::stats::ProductStats::from_mesh)
            .collect();
        let file_stats = crate::mesh::stats::FileStats::from_products(&prod_stats);

        let out = PyDict::new_bound(py);
        let n = prod_stats.len();
        let (mut guid, mut entity, mut source) =
            (Vec::with_capacity(n), Vec::with_capacity(n), Vec::with_capacity(n));
        let mut tri_count = Vec::with_capacity(n);
        let mut surface_area = Vec::with_capacity(n);
        let mut volume_abs = Vec::with_capacity(n);
        let (mut px, mut py_v, mut pz) =
            (Vec::with_capacity(n), Vec::with_capacity(n), Vec::with_capacity(n));
        let (mut cx, mut cy, mut cz) =
            (Vec::with_capacity(n), Vec::with_capacity(n), Vec::with_capacity(n));
        let mut drift_distance = Vec::with_capacity(n);
        let mut max_extent = Vec::with_capacity(n);
        let mut drift_ratio = Vec::with_capacity(n);
        let mut drift_severity = Vec::with_capacity(n);
        let mut aabb_volume = Vec::with_capacity(n);
        let mut mesh_quality = Vec::with_capacity(n);
        for s in &prod_stats {
            guid.push(s.guid.clone());
            entity.push(s.entity.clone());
            source.push(s.source);
            tri_count.push(s.triangle_count);
            surface_area.push(s.surface_area);
            volume_abs.push(s.volume.abs());
            px.push(s.placement_x);
            py_v.push(s.placement_y);
            pz.push(s.placement_z);
            cx.push((s.xmin + s.xmax) * 0.5);
            cy.push((s.ymin + s.ymax) * 0.5);
            cz.push((s.zmin + s.zmax) * 0.5);
            drift_distance.push(s.drift_distance);
            max_extent.push(s.max_extent);
            drift_ratio.push(s.drift_ratio);
            drift_severity.push(s.drift_severity);
            aabb_volume.push(s.aabb_volume);
            mesh_quality.push(s.mesh_quality);
        }
        out.set_item("guid", PyList::new_bound(py, guid))?;
        out.set_item("entity", PyList::new_bound(py, entity))?;
        out.set_item("source", PyList::new_bound(py, source))?;
        out.set_item("triangle_count", PyList::new_bound(py, tri_count))?;
        out.set_item("surface_area", PyList::new_bound(py, surface_area))?;
        out.set_item("volume_abs", PyList::new_bound(py, volume_abs))?;
        out.set_item("placement_x", PyList::new_bound(py, px))?;
        out.set_item("placement_y", PyList::new_bound(py, py_v))?;
        out.set_item("placement_z", PyList::new_bound(py, pz))?;
        out.set_item("centroid_x", PyList::new_bound(py, cx))?;
        out.set_item("centroid_y", PyList::new_bound(py, cy))?;
        out.set_item("centroid_z", PyList::new_bound(py, cz))?;
        out.set_item("drift_distance", PyList::new_bound(py, drift_distance))?;
        out.set_item("max_extent", PyList::new_bound(py, max_extent))?;
        out.set_item("drift_ratio", PyList::new_bound(py, drift_ratio))?;
        out.set_item("drift_severity", PyList::new_bound(py, drift_severity))?;
        out.set_item("aabb_volume", PyList::new_bound(py, aabb_volume))?;
        out.set_item("mesh_quality", PyList::new_bound(py, mesh_quality))?;
        out.set_item("drift_ok", file_stats.drift_ok)?;
        out.set_item("drift_warn", file_stats.drift_warn)?;
        out.set_item("drift_error", file_stats.drift_error)?;

        // Per-segment provenance — flat long-format columns, one row
        // per MeshSegment across all products. A product with a single
        // representation item contributes one row; an IfcBooleanResult
        // contributes one row per operand. The compound `role|leaf`
        // tag (e.g. "boolean_second_operand|halfspace_bounded") is
        // preserved verbatim in `seg_source` so consumers can split or
        // colour by either half.
        let total_segments: usize = meshes.iter().map(|m| m.segments.len()).sum();
        let mut seg_guid: Vec<String> = Vec::with_capacity(total_segments);
        let mut seg_product_index: Vec<u32> = Vec::with_capacity(total_segments);
        let mut seg_index: Vec<u32> = Vec::with_capacity(total_segments);
        let mut seg_source: Vec<String> = Vec::with_capacity(total_segments);
        let mut seg_triangle_count: Vec<u32> = Vec::with_capacity(total_segments);
        let mut seg_index_start: Vec<u32> = Vec::with_capacity(total_segments);
        for (pi, mesh) in meshes.iter().enumerate() {
            for (si, seg) in mesh.segments.iter().enumerate() {
                seg_guid.push(mesh.guid.clone());
                seg_product_index.push(pi as u32);
                seg_index.push(si as u32);
                seg_source.push(seg.source.clone());
                seg_triangle_count.push(seg.index_count / 3);
                seg_index_start.push(seg.index_start);
            }
        }
        out.set_item("seg_guid", PyList::new_bound(py, seg_guid))?;
        out.set_item("seg_product_index", PyList::new_bound(py, seg_product_index))?;
        out.set_item("seg_index", PyList::new_bound(py, seg_index))?;
        out.set_item("seg_source", PyList::new_bound(py, seg_source))?;
        out.set_item("seg_triangle_count", PyList::new_bound(py, seg_triangle_count))?;
        out.set_item("seg_index_start", PyList::new_bound(py, seg_index_start))?;

        out.set_item("mesh_emission_ms", mesh_stats.elapsed_ms)?;
        out.set_item("entity_table_ms", mesh_stats.entity_table_build_ms)?;
        out.set_item("total_ms", t_total.elapsed().as_secs_f64() * 1000.0)?;
        out.set_item("size_bytes", mmap.len() as u64)?;
        Ok(out)
    }

    // ----- global shift (CloudCompare contract) ------------------------

    /// Decide the model-wide global shift from the first geometry
    /// product's f64 world origin. Returns the rounded origin (so far-
    /// from-origin geometry is repositioned near the f32-precise origin)
    /// only when the origin is genuinely large; otherwise `[0, 0, 0]` so
    /// near-origin models keep absolute world coordinates unchanged.
    ///
    /// Threshold is in metres (origin scaled by `unit_scale`): 10 km.
    /// Below it, f32 already represents the coordinate finely enough
    /// (~1 mm quantum at 10 km) that no shift is warranted; above it
    /// (UTM eastings/northings, mm-based georef at 1e8–1e9) geometry
    /// collapses without the shift.
    #[cfg(feature = "mesh")]
    fn global_shift_for(world_origin: &[f64; 3], unit_scale: f32) -> [f64; 3] {
        const THRESHOLD_M: f64 = 1.0e4;
        let us = unit_scale as f64;
        let max_m = world_origin
            .iter()
            .map(|c| (c * us).abs())
            .fold(0.0_f64, f64::max);
        if max_m > THRESHOLD_M {
            [
                world_origin[0].round(),
                world_origin[1].round(),
                world_origin[2].round(),
            ]
        } else {
            [0.0, 0.0, 0.0]
        }
    }

    // ----- sample_point_cloud ------------------------------------------

    /// Sample `per_m2` points per square metre of surface on every
    /// meshed product. Returns parallel-list columns suitable for a
    /// flat `pd.DataFrame` build on the Python side.
    ///
    /// Designed for synthetic-training-data pipelines (scan-to-BIM
    /// classifier): you get (x, y, z, nx, ny, nz, guid, entity, class)
    /// for every sampled point, with the product's class as the
    /// training label. Deterministic from `seed`.
    #[cfg(feature = "mesh")]
    #[pyfunction]
    pub fn sample_point_cloud<'py>(
        py: Python<'py>,
        path: &str,
        per_m2: f32,
        seed: u64,
    ) -> PyResult<Bound<'py, PyDict>> {
        use crate::mesh::{sample::sample as sample_mesh, BakeFrame, ProductMesh, ProductSink};

        let t_total = Instant::now();
        let (mmap, _open_ms) = open_mmap(path)?;
        let t_idx = Instant::now();
        let idx = py.allow_threads(|| indexer::index(&mmap));
        let idx_ms = t_idx.elapsed().as_secs_f64() * 1000.0;
        let unit_scale = idx.unit_scale.unwrap_or(1.0) as f32;
        let area_scale = unit_scale * unit_scale;

        struct CloudSink {
            per_m2: f32,
            seed: u64,
            area_scale: f32,
            // Linear-unit-to-metres factor. Sampled point COORDINATES
            // are scaled by this so the output is always metres,
            // matching mesh_qto's m²/m³ convention. Normals are
            // direction vectors and stay unit-length (not scaled).
            unit_scale: f32,
            // Model-wide global shift (CloudCompare contract), in model
            // units. Set lazily from the first geometry product's f64
            // world origin (rounded). Points are positioned as
            // `local_shape + (world_origin - shift)` — both terms small,
            // the sum stays in f32-safe range even for georeferenced
            // models. Exposed back to the caller (scaled to metres) so
            // absolute world coords are `point + global_shift`.
            shift: Option<[f64; 3]>,
            // One row per emitted point. Each Vec has length equal to
            // the total point count.
            guid: Vec<String>,
            entity: Vec<String>,
            x: Vec<f32>,
            y: Vec<f32>,
            z: Vec<f32>,
            nx: Vec<f32>,
            ny: Vec<f32>,
            nz: Vec<f32>,
        }

        impl ProductSink for CloudSink {
            fn on_product(&mut self, mesh: ProductMesh) {
                // Derive a per-product seed from `(seed, ifc_id)` so
                // every product's PRNG stream is independent — adding
                // a product to the file doesn't shift every other
                // product's sampled points. Cheap one-line splitmix64.
                let mut s = self.seed ^ mesh.ifc_id.wrapping_mul(0x9E3779B97F4A7C15);
                s = (s ^ (s >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
                s = (s ^ (s >> 27)).wrapping_mul(0x94D049BB133111EB);
                s ^= s >> 31;

                let cloud = sample_mesh(
                    &mesh.vertices,
                    &mesh.indices,
                    self.area_scale,
                    self.per_m2,
                    s,
                );
                let n = cloud.len();
                if n == 0 {
                    return;
                }
                // Pin the model-wide shift to the first geometry product's
                // world origin (rounded to a clean model-unit value). All
                // later products subtract the same shift, so the relative
                // layout of the whole model is preserved while every point
                // stays near origin in f32. Threshold-gated like
                // CloudCompare: only shift when the origin is large enough
                // (>10 km in metres) to actually lose f32 precision, so
                // normal building models stay byte-identical (shift 0) and
                // return absolute world coordinates as before.
                let shift = *self
                    .shift
                    .get_or_insert_with(|| global_shift_for(&mesh.mesh_anchor, self.unit_scale));
                // Offset from shift to THIS product's precise origin, in
                // model units. Small for any product within a sane model
                // extent — computed in f64 so it never collapses.
                let off = [
                    mesh.mesh_anchor[0] - shift[0],
                    mesh.mesh_anchor[1] - shift[1],
                    mesh.mesh_anchor[2] - shift[2],
                ];
                self.guid.reserve(n);
                self.entity.reserve(n);
                for _ in 0..n {
                    self.guid.push(mesh.guid.clone());
                    self.entity.push(mesh.entity.clone());
                }
                // Position each local-frame point at `local + off` (f64),
                // then scale native-unit → metres. Local shape is near
                // origin, `off` is small → no f32 collapse. Normals are
                // direction vectors, copied through unchanged.
                let us = self.unit_scale as f64;
                self.x
                    .extend(cloud.x.iter().map(|v| ((*v as f64 + off[0]) * us) as f32));
                self.y
                    .extend(cloud.y.iter().map(|v| ((*v as f64 + off[1]) * us) as f32));
                self.z
                    .extend(cloud.z.iter().map(|v| ((*v as f64 + off[2]) * us) as f32));
                self.nx.extend(cloud.nx);
                self.ny.extend(cloud.ny);
                self.nz.extend(cloud.nz);
            }
        }

        let mut sink = CloudSink {
            per_m2,
            seed,
            area_scale,
            unit_scale,
            shift: None,
            guid: Vec::new(),
            entity: Vec::new(),
            x: Vec::new(),
            y: Vec::new(),
            z: Vec::new(),
            nx: Vec::new(),
            ny: Vec::new(),
            nz: Vec::new(),
        };
        let t_mesh = Instant::now();
        // Local frame: shape near origin (f32-precise even for
        // georeferenced models), repositioned per-product in f64 via the
        // global shift. World-frame baking would collapse small far-from-
        // origin geometry before sampling ever ran.
        let mesh_stats = py.allow_threads(|| {
            crate::mesh::mesh_ifc_streaming_framed(&mmap, &mut sink, BakeFrame::Local)
        });
        let mesh_ms = t_mesh.elapsed().as_secs_f64() * 1000.0;

        let t_marshal = Instant::now();
        let out = PyDict::new_bound(py);
        out.set_item("guid", PyList::new_bound(py, &sink.guid))?;
        out.set_item("entity", PyList::new_bound(py, &sink.entity))?;
        out.set_item("x", PyList::new_bound(py, &sink.x))?;
        out.set_item("y", PyList::new_bound(py, &sink.y))?;
        out.set_item("z", PyList::new_bound(py, &sink.z))?;
        out.set_item("nx", PyList::new_bound(py, &sink.nx))?;
        out.set_item("ny", PyList::new_bound(py, &sink.ny))?;
        out.set_item("nz", PyList::new_bound(py, &sink.nz))?;
        out.set_item("unit_scale", unit_scale as f64)?;
        // Global shift in METRES: add this back to (x, y, z) to recover
        // absolute world coordinates. `[0, 0, 0]` when the model has no
        // geometry or already sits near origin.
        let gs = sink.shift.unwrap_or([0.0, 0.0, 0.0]);
        let us = unit_scale as f64;
        out.set_item(
            "global_shift",
            PyList::new_bound(py, [gs[0] * us, gs[1] * us, gs[2] * us]),
        )?;
        out.set_item("per_m2", per_m2 as f64)?;
        out.set_item("seed", seed)?;
        out.set_item("points_emitted", sink.x.len() as u64)?;
        out.set_item("products_meshed", mesh_stats.products_meshed as u64)?;
        out.set_item("indexer_ms", idx_ms)?;
        out.set_item("mesh_ms", mesh_ms)?;
        out.set_item("marshal_ms", t_marshal.elapsed().as_secs_f64() * 1000.0)?;
        out.set_item("total_ms", t_total.elapsed().as_secs_f64() * 1000.0)?;
        out.set_item("size_bytes", mmap.len() as u64)?;
        Ok(out)
    }

    // ----- extract_meshes ----------------------------------------------

    /// Raw per-product triangle meshes. Returns parallel lists where
    /// each entry `i` describes one meshed product:
    ///
    /// - `guid[i]`, `entity[i]` — identity + raw IFC class
    /// - `vertices[i]` — `bytes`, world-coord `f32` LE triples
    ///   (`vertex_count[i] * 3` floats). Decode with
    ///   `np.frombuffer(b, np.float32).reshape(-1, 3)`.
    /// - `indices[i]` — `bytes`, `u32` LE triangle indices
    ///   (`triangle_count[i] * 3` ints). Decode with
    ///   `np.frombuffer(b, np.uint32).reshape(-1, 3)`.
    ///
    /// This is the fast drop-in for IfcOpenShell tessellation in
    /// point-sampling / scan-to-BIM corpus pipelines: same Rust mesher
    /// `mesh_qto` uses internally, but the triangles survive to Python
    /// instead of being consumed by the QTO sweep. Bytes encoding keeps
    /// the marshal zero-per-element — a single memcpy per product, not
    /// N PyFloat allocations.
    ///
    /// Geometryless products (no body) are omitted — they have no
    /// triangles to return. Use the substrate bundle or `m.products_df`
    /// if you need those rows.
    #[cfg(feature = "mesh")]
    #[pyfunction]
    pub fn extract_meshes<'py>(py: Python<'py>, path: &str) -> PyResult<Bound<'py, PyDict>> {
        use crate::mesh::{BakeFrame, ProductMesh, ProductSink};

        let t_total = Instant::now();
        let (mmap, _open_ms) = open_mmap(path)?;

        // Linear-unit-to-metres factor — vertices are scaled to metres
        // so the output matches the metres contract (and mesh_qto). The
        // indexer pass is the same source point_cloud uses.
        let idx = py.allow_threads(|| indexer::index(&mmap));
        let unit_scale = idx.unit_scale.unwrap_or(1.0) as f32;

        struct MeshSink {
            unit_scale: f32,
            // Model-wide global shift (CloudCompare contract), model
            // units. Same scheme as `sample_point_cloud`: set from the
            // first geometry product's f64 origin (rounded); per-product
            // vertices are `local + (world_origin - shift)`, scaled to
            // metres. Add `global_shift` back for absolute coords.
            shift: Option<[f64; 3]>,
            guid: Vec<String>,
            entity: Vec<String>,
            vertex_count: Vec<u32>,
            triangle_count: Vec<u32>,
            vertices_le: Vec<Vec<u8>>,
            indices_le: Vec<Vec<u8>>,
        }

        impl ProductSink for MeshSink {
            fn on_product(&mut self, mesh: ProductMesh) {
                // Skip geometryless products — no triangles to hand back.
                if mesh.indices.is_empty() || mesh.vertices.is_empty() {
                    return;
                }
                let shift = *self
                    .shift
                    .get_or_insert_with(|| global_shift_for(&mesh.mesh_anchor, self.unit_scale));
                let off = [
                    mesh.mesh_anchor[0] - shift[0],
                    mesh.mesh_anchor[1] - shift[1],
                    mesh.mesh_anchor[2] - shift[2],
                ];
                // Reposition local-frame shape to `local + off` (f64),
                // scale native-unit → metres. Far-from-origin geometry
                // stays precise: shape near origin, off small.
                let us = self.unit_scale as f64;
                let mut vbytes = Vec::with_capacity(mesh.vertices.len() * 4);
                for chunk in mesh.vertices.chunks_exact(3) {
                    let x = ((chunk[0] as f64 + off[0]) * us) as f32;
                    let y = ((chunk[1] as f64 + off[1]) * us) as f32;
                    let z = ((chunk[2] as f64 + off[2]) * us) as f32;
                    vbytes.extend_from_slice(&x.to_le_bytes());
                    vbytes.extend_from_slice(&y.to_le_bytes());
                    vbytes.extend_from_slice(&z.to_le_bytes());
                }
                let mut ibytes = Vec::with_capacity(mesh.indices.len() * 4);
                for i in &mesh.indices {
                    ibytes.extend_from_slice(&i.to_le_bytes());
                }
                self.guid.push(mesh.guid);
                self.entity.push(mesh.entity);
                self.vertex_count.push((mesh.vertices.len() / 3) as u32);
                self.triangle_count.push((mesh.indices.len() / 3) as u32);
                self.vertices_le.push(vbytes);
                self.indices_le.push(ibytes);
            }
        }

        let mut sink = MeshSink {
            unit_scale,
            shift: None,
            guid: Vec::new(),
            entity: Vec::new(),
            vertex_count: Vec::new(),
            triangle_count: Vec::new(),
            vertices_le: Vec::new(),
            indices_le: Vec::new(),
        };
        let t_mesh = Instant::now();
        // Local frame + per-product f64 shift — see sample_point_cloud.
        let mesh_stats = py.allow_threads(|| {
            crate::mesh::mesh_ifc_streaming_framed(&mmap, &mut sink, BakeFrame::Local)
        });
        let mesh_ms = t_mesh.elapsed().as_secs_f64() * 1000.0;

        let t_marshal = Instant::now();
        let out = PyDict::new_bound(py);
        out.set_item("guid", PyList::new_bound(py, &sink.guid))?;
        out.set_item("entity", PyList::new_bound(py, &sink.entity))?;
        out.set_item("vertex_count", PyList::new_bound(py, &sink.vertex_count))?;
        out.set_item("triangle_count", PyList::new_bound(py, &sink.triangle_count))?;
        let verts: Vec<Bound<'py, PyBytes>> = sink
            .vertices_le
            .iter()
            .map(|b| PyBytes::new_bound(py, b))
            .collect();
        let inds: Vec<Bound<'py, PyBytes>> = sink
            .indices_le
            .iter()
            .map(|b| PyBytes::new_bound(py, b))
            .collect();
        out.set_item("vertices", PyList::new_bound(py, verts))?;
        out.set_item("indices", PyList::new_bound(py, inds))?;
        // Global shift in METRES — add back to vertices for absolute
        // world coords. `[0, 0, 0]` for near-origin or empty models.
        let gs = sink.shift.unwrap_or([0.0, 0.0, 0.0]);
        let us = unit_scale as f64;
        out.set_item(
            "global_shift",
            PyList::new_bound(py, [gs[0] * us, gs[1] * us, gs[2] * us]),
        )?;
        out.set_item("products_meshed", mesh_stats.products_meshed as u64)?;
        out.set_item("mesh_ms", mesh_ms)?;
        out.set_item("marshal_ms", t_marshal.elapsed().as_secs_f64() * 1000.0)?;
        out.set_item("total_ms", t_total.elapsed().as_secs_f64() * 1000.0)?;
        out.set_item("size_bytes", mmap.len() as u64)?;
        Ok(out)
    }

    // ----- clash --------------------------------------------------------

    #[cfg(feature = "clash")]
    #[pyfunction]
    #[pyo3(signature = (
        bundle_dir,
        tolerance_m = 0.0,
        write_parquet = true,
        include_classes = Vec::<String>::new(),
        exclude_self_class = Vec::<String>::new(),
    ))]
    fn clash<'py>(
        py: Python<'py>,
        bundle_dir: &str,
        tolerance_m: f32,
        write_parquet: bool,
        include_classes: Vec<String>,
        exclude_self_class: Vec<String>,
    ) -> PyResult<Bound<'py, PyDict>> {
        use crate::clash::{
            clash as run_clash, write_clashes_parquet, ClashKind, ClashOptions,
        };
        let bundle_dir = Path::new(bundle_dir).to_path_buf();
        let opts = ClashOptions {
            tolerance_m,
            include_classes,
            exclude_self_class,
        };

        let t = Instant::now();
        let report = py
            .allow_threads(|| run_clash(&bundle_dir, &opts))
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("clash: {e}")))?;
        let clash_ms = t.elapsed().as_secs_f64() * 1000.0;

        let mut written_path: Option<String> = None;
        if write_parquet {
            let out = bundle_dir.join("clashes.parquet");
            py.allow_threads(|| write_clashes_parquet(&out, &report.pairs))
                .map_err(|e| {
                    pyo3::exceptions::PyIOError::new_err(format!(
                        "write {}: {e}",
                        out.display()
                    ))
                })?;
            written_path = Some(out.to_string_lossy().to_string());
        }

        let n = report.pairs.len();
        let mut ifc_id_a: Vec<u64> = Vec::with_capacity(n);
        let mut ifc_id_b: Vec<u64> = Vec::with_capacity(n);
        let mut guid_a: Vec<String> = Vec::with_capacity(n);
        let mut guid_b: Vec<String> = Vec::with_capacity(n);
        let mut class_a: Vec<String> = Vec::with_capacity(n);
        let mut class_b: Vec<String> = Vec::with_capacity(n);
        let mut kind: Vec<&'static str> = Vec::with_capacity(n);
        let mut min_distance_m: Vec<f32> = Vec::with_capacity(n);
        for p in &report.pairs {
            ifc_id_a.push(p.ifc_id_a);
            ifc_id_b.push(p.ifc_id_b);
            guid_a.push(p.guid_a.clone());
            guid_b.push(p.guid_b.clone());
            class_a.push(p.class_a.clone());
            class_b.push(p.class_b.clone());
            kind.push(match p.kind {
                ClashKind::Hard => "hard",
                ClashKind::Clearance => "clearance",
            });
            min_distance_m.push(p.min_distance_m);
        }

        let out = PyDict::new_bound(py);
        out.set_item("ifc_id_a", PyList::new_bound(py, &ifc_id_a))?;
        out.set_item("ifc_id_b", PyList::new_bound(py, &ifc_id_b))?;
        out.set_item("guid_a", PyList::new_bound(py, &guid_a))?;
        out.set_item("guid_b", PyList::new_bound(py, &guid_b))?;
        out.set_item("class_a", PyList::new_bound(py, &class_a))?;
        out.set_item("class_b", PyList::new_bound(py, &class_b))?;
        out.set_item("kind", PyList::new_bound(py, &kind))?;
        out.set_item("min_distance_m", PyList::new_bound(py, &min_distance_m))?;
        out.set_item("geometryless_skipped", report.geometryless_skipped as u64)?;
        out.set_item("narrow_phase_residuals", report.narrow_phase_residuals as u64)?;
        out.set_item("pair_count", n as u64)?;
        out.set_item("tolerance_m", tolerance_m)?;
        out.set_item("clash_ms", clash_ms)?;
        if let Some(p) = written_path {
            out.set_item("clashes_parquet", p)?;
        }
        Ok(out)
    }

    #[pymodule]
    fn _core(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
        m.add_function(wrap_pyfunction!(index_ifc, m)?)?;
        m.add_function(wrap_pyfunction!(extract_psets, m)?)?;
        m.add_function(wrap_pyfunction!(extract_quantities, m)?)?;
        m.add_function(wrap_pyfunction!(extract_materials, m)?)?;
        m.add_function(wrap_pyfunction!(extract_classifications, m)?)?;
        m.add_function(wrap_pyfunction!(extract_all, m)?)?;
        #[cfg(feature = "mesh")]
        m.add_function(wrap_pyfunction!(analyse_drift, m)?)?;
        #[cfg(feature = "mesh")]
        m.add_function(wrap_pyfunction!(mesh_qto, m)?)?;
        #[cfg(feature = "mesh")]
        m.add_function(wrap_pyfunction!(sample_point_cloud, m)?)?;
        #[cfg(feature = "mesh")]
        m.add_function(wrap_pyfunction!(extract_meshes, m)?)?;
        #[cfg(feature = "clash")]
        m.add_function(wrap_pyfunction!(clash, m)?)?;
        m.add("__version__", env!("CARGO_PKG_VERSION"))?;
        Ok(())
    }
}
