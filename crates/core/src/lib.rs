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
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::time::Instant;

    use pyo3::prelude::*;
    use pyo3::types::{PyDict, PyList};
    #[cfg(feature = "mesh")]
    use pyo3::types::PyBytes;

    use crate::indexer;
    use crate::source::IfcSource;

    // ----- panic / error helpers ---------------------------------------

    // Custom Python exception so callers can `except IfcfastError`
    // instead of getting hit with the uncatchable `pyo3_runtime.PanicException`
    // when a Rust panic crosses the boundary. Covers the third failure
    // mode reported in GH #23 (worker death under allocator pressure).
    pyo3::create_exception!(_core, IfcfastError, pyo3::exceptions::PyException);

    /// Stringify a panic payload (`Box<dyn Any + Send>`) the way the
    /// default Rust panic hook would, so the Python error message
    /// carries the actual panic text instead of `<non-string payload>`.
    fn panic_payload_to_string(payload: Box<dyn std::any::Any + Send>) -> String {
        if let Some(s) = payload.downcast_ref::<&'static str>() {
            (*s).to_string()
        } else if let Some(s) = payload.downcast_ref::<String>() {
            s.clone()
        } else {
            "<non-string panic payload>".to_string()
        }
    }

    /// Wrap a `PyResult`-returning closure in `catch_unwind` and
    /// translate any panic into an `IfcfastError`. Apply at the PyO3
    /// boundary so a Rust panic never reaches the Python interpreter
    /// as the uncatchable `PanicException`.
    fn catch_panic<F, R>(f: F) -> PyResult<R>
    where
        F: FnOnce() -> PyResult<R>,
    {
        match catch_unwind(AssertUnwindSafe(f)) {
            Ok(r) => r,
            Err(payload) => Err(PyErr::new::<IfcfastError, _>(format!(
                "ifcfast Rust panic: {}",
                panic_payload_to_string(payload)
            ))),
        }
    }

    // Build the entity table the cross-product prism fast-path consults
    // for `extrude_params`, but ONLY when the experimental
    // `prism-csg-fast` feature is on — default `csg` builds get `None`
    // and the manifold fold runs unchanged. This is a second linear
    // pass over the buffer (the streaming pass owns + drops its own
    // table internally), confined to the post-stream flush and the
    // feature build; threading the streaming table out is the
    // promotion-time optimisation. `BakeFrame::World` callers must NOT
    // use this — the prism result is near-origin (see
    // `cut_openings::try_prism_cut` frame contract).
    #[cfg(feature = "csg")]
    fn prism_table_for_flush(buf: &[u8]) -> Option<crate::entity_table::EntityTable<'_>> {
        #[cfg(feature = "prism-csg-fast")]
        {
            Some(crate::entity_table::EntityTable::build(buf))
        }
        #[cfg(not(feature = "prism-csg-fast"))]
        {
            let _ = buf;
            None
        }
    }

    // ----- cut_openings stats marshalling (W2) -------------------------
    //
    // Single owner of the FFI key set for the per-pass cut_openings
    // counters. mesh_qto, extract_meshes and write_gltf all surface
    // these via the same Outcome::accumulate path on
    // crate::mesh::cut_stats::CutOpeningsStats — keeping the dict
    // shape consistent across entry points means downstream parquet
    // columns and Python wrappers can pivot once on a stable schema.
    //
    // The legacy `cut_openings_cut` / `_passthrough` / `_fallback`
    // keys stay for back-compat; the new `cut_openings_unsupported_*`
    // keys are added under [W2] (see
    // `docs/plans/2026-06-05_cut-openings-manifold-replacement.md` for
    // the per-reason vocabulary). Bumping `_CACHE_SCHEMA_VERSION`
    // covers consumers that pin against a schema fingerprint.
    //
    // Always emit every counter — csg-off builds carry zeros. The
    // shape stays stable so the parquet substrate and Python wrappers
    // never need to discriminate on feature flags.
    fn set_cut_openings_stats(
        out: &Bound<'_, PyDict>,
        stats: &crate::mesh::cut_stats::CutOpeningsStats,
    ) -> PyResult<()> {
        out.set_item("cut_openings_cut", stats.cut as u64)?;
        out.set_item("cut_openings_passthrough", stats.passthrough as u64)?;
        out.set_item("cut_openings_fallback", stats.fallback as u64)?;
        out.set_item(
            "cut_openings_unsupported_non_manifold_input",
            stats.unsupported_non_manifold_input as u64,
        )?;
        out.set_item(
            "cut_openings_unsupported_self_intersecting_cutter",
            stats.unsupported_self_intersecting_cutter as u64,
        )?;
        out.set_item(
            "cut_openings_unsupported_coplanar_face_degeneracy",
            stats.unsupported_coplanar_face_degeneracy as u64,
        )?;
        out.set_item(
            "cut_openings_unsupported_kernel_internal_error",
            stats.unsupported_kernel_internal_error as u64,
        )?;
        out.set_item(
            "cut_openings_unsupported_curved_surface_approximated",
            stats.unsupported_curved_surface_approximated as u64,
        )?;
        out.set_item(
            "cut_openings_unsupported_intersection_not_implemented",
            stats.unsupported_intersection_not_implemented as u64,
        )?;
        out.set_item(
            "cut_openings_unsupported_union_with_overlap",
            stats.unsupported_union_with_overlap as u64,
        )?;
        out.set_item(
            "cut_openings_unsupported_non_planar_base_surface",
            stats.unsupported_non_planar_base_surface as u64,
        )?;
        out.set_item(
            "cut_openings_unsupported_unhandled_cutter_entity",
            stats.unsupported_unhandled_cutter_entity as u64,
        )?;
        out.set_item(
            "cut_openings_unsupported_malformed_host",
            stats.unsupported_malformed_host as u64,
        )?;
        out.set_item(
            "cut_openings_unsupported_bsp_depth_exceeded",
            stats.unsupported_bsp_depth_exceeded as u64,
        )?;
        out.set_item(
            "cut_openings_unsupported_tight_polygonal_boundary_ignored",
            stats.unsupported_tight_polygonal_boundary_ignored as u64,
        )?;
        out.set_item(
            "cut_openings_unsupported_degenerate_cutter",
            stats.unsupported_degenerate_cutter as u64,
        )?;
        out.set_item(
            "cut_openings_unsupported_host_consumed",
            stats.unsupported_host_consumed as u64,
        )?;
        Ok(())
    }

    // ----- index_ifc ----------------------------------------------------

    #[pyfunction]
    fn index_ifc<'py>(py: Python<'py>, path: &str) -> PyResult<Bound<'py, PyDict>> {
        let (mmap, open_ms) = open_mmap(path)?;

        let t_index = Instant::now();
        let idx = py.detach(|| indexer::index(&mmap));
        let index_ms = t_index.elapsed().as_secs_f64() * 1000.0;

        let t_marshal = Instant::now();

        let dict = PyDict::new(py);
        dict.set_item("schema", &idx.schema)?;
        dict.set_item("project_name", &idx.project_name)?;
        dict.set_item("authoring_app", &idx.authoring_app)?;
        dict.set_item("unit_scale", idx.unit_scale)?;
        dict.set_item("size_bytes", mmap.len() as u64)?;
        dict.set_item("open_ms", open_ms)?;
        dict.set_item("index_ms", index_ms)?;

        let tc = PyDict::new(py);
        for (k, v) in &idx.type_counts {
            tc.set_item(k, v)?;
        }
        dict.set_item("type_counts", tc)?;

        let products = PyDict::new(py);
        products.set_item("step_id", PyList::new(py, &idx.product_step_id)?)?;
        products.set_item("guid", PyList::new(py, &idx.product_guid)?)?;
        products.set_item("entity", PyList::new(py, &idx.product_entity)?)?;
        products.set_item("name", PyList::new(py, &idx.product_name)?)?;
        products.set_item(
            "predefined_type",
            PyList::new(py, &idx.product_predefined_type)?,
        )?;
        products.set_item("object_type", PyList::new(py, &idx.product_object_type)?)?;
        products.set_item("tag", PyList::new(py, &idx.product_tag)?)?;
        dict.set_item("products", products)?;

        let storeys = PyDict::new(py);
        storeys.set_item("step_id", PyList::new(py, &idx.storey_step_id)?)?;
        storeys.set_item("guid", PyList::new(py, &idx.storey_guid)?)?;
        storeys.set_item("name", PyList::new(py, &idx.storey_name)?)?;
        storeys.set_item("elevation", PyList::new(py, &idx.storey_elevation)?)?;
        storeys.set_item(
            "building_step_id",
            PyList::new(py, &idx.storey_building_step_id)?,
        )?;
        dict.set_item("storeys", storeys)?;

        let contained = PyDict::new(py);
        contained.set_item("child", PyList::new(py, &idx.contained_in_child)?)?;
        contained.set_item("structure", PyList::new(py, &idx.contained_in_structure)?)?;
        dict.set_item("contained_in", contained)?;

        let agg = PyDict::new(py);
        agg.set_item("child", PyList::new(py, &idx.aggregates_child)?)?;
        agg.set_item("parent", PyList::new(py, &idx.aggregates_parent)?)?;
        dict.set_item("aggregates", agg)?;

        let sb = PyDict::new(py);
        sb.set_item("storey", PyList::new(py, &idx.storey_building_storey)?)?;
        sb.set_item("building", PyList::new(py, &idx.storey_building_building)?)?;
        dict.set_item("storey_building", sb)?;

        let voids = PyDict::new(py);
        voids.set_item("opening", PyList::new(py, &idx.voids_opening)?)?;
        voids.set_item("host", PyList::new(py, &idx.voids_host)?)?;
        dict.set_item("voids", voids)?;

        // IfcRelDefinesByType: (product_step_id, type_step_id) pairs, plus
        // the IfcTypeObject table that lets Python resolve type_step_id to
        // (type_guid, type_name, type_entity).
        let dbt = PyDict::new(py);
        dbt.set_item("product", PyList::new(py, &idx.defines_by_type_product)?)?;
        dbt.set_item("type", PyList::new(py, &idx.defines_by_type_type)?)?;
        dict.set_item("defines_by_type", dbt)?;

        let types = PyDict::new(py);
        types.set_item("step_id", PyList::new(py, &idx.type_object_step_id)?)?;
        types.set_item("entity", PyList::new(py, &idx.type_object_entity)?)?;
        types.set_item("guid", PyList::new(py, &idx.type_object_guid)?)?;
        types.set_item("name", PyList::new(py, &idx.type_object_name)?)?;
        dict.set_item("type_objects", types)?;

        let site_ids: Vec<u64> = idx.site_step_id_to_guid.keys().copied().collect();
        let site_guids: Vec<&str> = site_ids
            .iter()
            .map(|i| idx.site_step_id_to_guid.get(i).unwrap().as_str())
            .collect();
        let sites = PyDict::new(py);
        sites.set_item("step_id", PyList::new(py, site_ids)?)?;
        sites.set_item("guid", PyList::new(py, site_guids)?)?;
        dict.set_item("sites", sites)?;

        let bldg_ids: Vec<u64> = idx.building_step_id_to_guid.keys().copied().collect();
        let bldg_guids: Vec<&str> = bldg_ids
            .iter()
            .map(|i| idx.building_step_id_to_guid.get(i).unwrap().as_str())
            .collect();
        let buildings = PyDict::new(py);
        buildings.set_item("step_id", PyList::new(py, bldg_ids)?)?;
        buildings.set_item("guid", PyList::new(py, bldg_guids)?)?;
        dict.set_item("buildings", buildings)?;

        let proj_ids: Vec<u64> = idx.project_step_id_to_guid.keys().copied().collect();
        let proj_guids: Vec<&str> = proj_ids
            .iter()
            .map(|i| idx.project_step_id_to_guid.get(i).unwrap().as_str())
            .collect();
        let projects = PyDict::new(py);
        projects.set_item("step_id", PyList::new(py, proj_ids)?)?;
        projects.set_item("guid", PyList::new(py, proj_guids)?)?;
        dict.set_item("projects", projects)?;

        let space_ids: Vec<u64> = idx.space_step_id_to_guid.keys().copied().collect();
        let space_guids: Vec<&str> = space_ids
            .iter()
            .map(|i| idx.space_step_id_to_guid.get(i).unwrap().as_str())
            .collect();
        let spaces = PyDict::new(py);
        spaces.set_item("step_id", PyList::new(py, space_ids)?)?;
        spaces.set_item("guid", PyList::new(py, space_guids)?)?;
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
        let psets = py.detach(|| crate::extractors::psets::build(&table, &step_to_guid));
        let pset_ms = t_psets.elapsed().as_secs_f64() * 1000.0;

        let t_marshal = Instant::now();
        let out = PyDict::new(py);
        out.set_item("guid", PyList::new(py, &psets.guid)?)?;
        out.set_item("pset_name", PyList::new(py, &psets.pset_name)?)?;
        out.set_item("prop_name", PyList::new(py, &psets.prop_name)?)?;
        out.set_item("value", PyList::new(py, &psets.value)?)?;
        out.set_item("value_type", PyList::new(py, &psets.value_type)?)?;
        out.set_item("source", PyList::new(py, &psets.source)?)?;
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
        let qto = py.detach(|| crate::extractors::quantities::build(&table, &step_to_guid));
        let qto_ms = t_qto.elapsed().as_secs_f64() * 1000.0;

        let t_marshal = Instant::now();
        let out = PyDict::new(py);
        out.set_item("guid", PyList::new(py, &qto.guid)?)?;
        out.set_item("qto_name", PyList::new(py, &qto.qto_name)?)?;
        out.set_item("quantity_name", PyList::new(py, &qto.quantity_name)?)?;
        out.set_item("value", PyList::new(py, &qto.value)?)?;
        out.set_item("quantity_type", PyList::new(py, &qto.quantity_type)?)?;
        out.set_item("unit_step_id", PyList::new(py, &qto.unit_step_id)?)?;
        out.set_item("source", PyList::new(py, &qto.source)?)?;
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
        let mats = py.detach(|| {
            let unit_scale = crate::indexer::extract_unit_scale(&table).unwrap_or(1.0);
            crate::extractors::materials::build(&table, &step_to_guid, unit_scale)
        });
        let mat_ms = t_mat.elapsed().as_secs_f64() * 1000.0;

        let t_marshal = Instant::now();
        let out = PyDict::new(py);
        out.set_item("guid", PyList::new(py, &mats.guid)?)?;
        out.set_item("role", PyList::new(py, &mats.role)?)?;
        out.set_item("layer_index", PyList::new(py, &mats.layer_index)?)?;
        out.set_item("material_name", PyList::new(py, &mats.material_name)?)?;
        out.set_item("fraction", PyList::new(py, &mats.fraction)?)?;
        out.set_item(
            "layer_thickness_mm",
            PyList::new(py, &mats.layer_thickness_mm)?,
        )?;
        out.set_item("category", PyList::new(py, &mats.category)?)?;
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
        let cls = py.detach(|| crate::extractors::classifications::build(&table, &step_to_guid));
        let cls_ms = t_cls.elapsed().as_secs_f64() * 1000.0;

        let t_marshal = Instant::now();
        let out = PyDict::new(py);
        out.set_item("guid", PyList::new(py, &cls.guid)?)?;
        out.set_item("system_name", PyList::new(py, &cls.system_name)?)?;
        out.set_item("edition", PyList::new(py, &cls.edition)?)?;
        out.set_item("identification", PyList::new(py, &cls.identification)?)?;
        out.set_item("name", PyList::new(py, &cls.name)?)?;
        out.set_item("location", PyList::new(py, &cls.location)?)?;
        out.set_item("source", PyList::new(py, &cls.source)?)?;
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
            py.detach(|| {
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
        let out = PyDict::new(py);
        {
            let d = PyDict::new(py);
            d.set_item("guid", PyList::new(py, &psets.guid)?)?;
            d.set_item("pset_name", PyList::new(py, &psets.pset_name)?)?;
            d.set_item("prop_name", PyList::new(py, &psets.prop_name)?)?;
            d.set_item("value", PyList::new(py, &psets.value)?)?;
            d.set_item("value_type", PyList::new(py, &psets.value_type)?)?;
            d.set_item("source", PyList::new(py, &psets.source)?)?;
            out.set_item("psets", d)?;
        }
        {
            let d = PyDict::new(py);
            d.set_item("guid", PyList::new(py, &quantities.guid)?)?;
            d.set_item("qto_name", PyList::new(py, &quantities.qto_name)?)?;
            d.set_item("quantity_name", PyList::new(py, &quantities.quantity_name)?)?;
            d.set_item("value", PyList::new(py, &quantities.value)?)?;
            d.set_item("quantity_type", PyList::new(py, &quantities.quantity_type)?)?;
            d.set_item("unit_step_id", PyList::new(py, &quantities.unit_step_id)?)?;
            d.set_item("source", PyList::new(py, &quantities.source)?)?;
            out.set_item("quantities", d)?;
        }
        {
            let d = PyDict::new(py);
            d.set_item("guid", PyList::new(py, &materials.guid)?)?;
            d.set_item("role", PyList::new(py, &materials.role)?)?;
            d.set_item("layer_index", PyList::new(py, &materials.layer_index)?)?;
            d.set_item("material_name", PyList::new(py, &materials.material_name)?)?;
            d.set_item(
                "layer_thickness_mm",
                PyList::new(py, &materials.layer_thickness_mm)?,
            )?;
            d.set_item("category", PyList::new(py, &materials.category)?)?;
            d.set_item("fraction", PyList::new(py, &materials.fraction)?)?;
            out.set_item("materials", d)?;
        }
        {
            let d = PyDict::new(py);
            d.set_item("guid", PyList::new(py, &classifications.guid)?)?;
            d.set_item("system_name", PyList::new(py, &classifications.system_name)?)?;
            d.set_item("edition", PyList::new(py, &classifications.edition)?)?;
            d.set_item("identification", PyList::new(py, &classifications.identification)?)?;
            d.set_item("name", PyList::new(py, &classifications.name)?)?;
            d.set_item("location", PyList::new(py, &classifications.location)?)?;
            d.set_item("source", PyList::new(py, &classifications.source)?)?;
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
    ///     guid, entity, volume_m3 (best estimate), volume_mesh_m3 (raw
    ///     mesh), volume_prism_bound_m3, volume_reliable, volume_method,
    ///     mesh_quality, aabb_volume_m3, surface_area_m2, area_top_m2,
    ///     area_bottom_m2, area_side_m2, area_inclined_m2,
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
    #[pyo3(signature = (path, cut_openings = false))]
    pub fn mesh_qto<'py>(
        py: Python<'py>,
        path: &str,
        cut_openings: bool,
    ) -> PyResult<Bound<'py, PyDict>> {
        catch_panic(|| {
        use crate::mesh::{
            mesh_ifc_streaming_framed, qto, BakeFrame, ProductMesh, ProductSink,
        };

        // cut_openings requires the `csg` feature — surface a clear
        // error rather than silently ignoring the flag (same pattern as
        // extract_meshes).
        #[cfg(not(feature = "csg"))]
        if cut_openings {
            return Err(pyo3::exceptions::PyRuntimeError::new_err(
                "mesh_qto(cut_openings=True) requires the `csg` Cargo feature; \
                 this wheel was built without it.",
            ));
        }

        let t_total = Instant::now();
        let (mmap, _open_ms) = open_mmap(path)?;

        // Project unit-scale (mm files: 0.001; metre files: 1.0).
        // Pulled from the indexer — same source the bundle pre-pass
        // uses. None → assume metres so geometry-derived numbers stay
        // sane on schema-incomplete files.
        let t_idx = Instant::now();
        let idx = py.detach(|| indexer::index(&mmap));
        let idx_ms = t_idx.elapsed().as_secs_f64() * 1000.0;
        let unit_scale = idx.unit_scale.unwrap_or(1.0) as f32;

        // Per-product accumulator sink. One row per meshed product
        // landing in `products`; one row per distinct planar surface
        // landing in `surfaces`. Avoids holding the meshes themselves
        // — drops each ProductMesh after computing its QTO.
        struct QtoSink {
            unit_scale: f32,
            cut_openings: bool,
            guid: Vec<String>,
            entity: Vec<String>,
            volume_m3: Vec<f32>,
            volume_mesh_m3: Vec<f32>,
            volume_prism_bound_m3: Vec<f32>,
            volume_reliable: Vec<bool>,
            volume_method: Vec<&'static str>,
            mesh_quality: Vec<&'static str>,
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
            cut_stats: crate::mesh::cut_stats::CutOpeningsStats,
            // Cross-product IfcRelVoidsElement buffer — same pattern as
            // extract_meshes. Only `Some` when cut_openings && at least
            // one void relation exists; else the hot path is identical
            // to the no-cut version.
            #[cfg(feature = "csg")]
            cross: Option<crate::mesh::cut_openings::CrossProductCut>,
        }
        impl QtoSink {
            /// Compute QTO for a (cut-applied or unchanged) mesh and
            /// push its row + per-surface rows. Shared between the
            /// streaming `on_product` and the post-stream cross-product
            /// flush.
            fn record(&mut self, mesh: ProductMesh) {
                let q = qto::compute(&mesh.vertices, &mesh.indices, self.unit_scale);
                self.guid.push(mesh.guid.clone());
                self.entity.push(mesh.entity.clone());
                self.volume_m3.push(q.volume_best_m3);
                self.volume_mesh_m3.push(q.volume_m3.abs());
                self.volume_prism_bound_m3.push(q.volume_prism_bound_m3);
                self.volume_reliable.push(q.volume_reliable);
                self.volume_method.push(q.volume_method);
                self.mesh_quality.push(q.mesh_quality);
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

            #[cfg(feature = "csg")]
            fn bump_outcome(&mut self, outcome: crate::mesh::cut_stats::Outcome) {
                outcome.accumulate(&mut self.cut_stats);
            }
        }
        impl ProductSink for QtoSink {
            fn on_product(&mut self, mut mesh: ProductMesh) {
                if mesh.indices.is_empty() || mesh.vertices.is_empty() {
                    return;
                }
                #[cfg(feature = "csg")]
                if self.cut_openings {
                    // Cross-product routing first: suppress openings,
                    // hold hosts for flush, pass-through the rest into
                    // the in-rep apply.
                    if let Some(cross) = self.cross.as_mut() {
                        use crate::mesh::cut_openings::Routed;
                        match cross.route(mesh) {
                            Routed::Suppressed | Routed::Held => return,
                            Routed::PassThrough(m) => mesh = m,
                        }
                    }
                    let outcome = crate::mesh::cut_openings::apply(&mut mesh, self.unit_scale);
                    self.bump_outcome(outcome);
                    if mesh.indices.is_empty() || mesh.vertices.is_empty() {
                        return;
                    }
                }
                #[cfg(not(feature = "csg"))]
                let _ = self.cut_openings;

                // No cut ran -> the synthetic half-space stand-in slabs
                // are still in the buffers and would be summed into the
                // signed-tetra volume and the AABB (GH #66). QTO must
                // measure the element, never tool geometry.
                if !(cfg!(feature = "csg") && self.cut_openings) {
                    crate::mesh::strip_synthetic_cutters(&mut mesh);
                    if mesh.indices.is_empty() || mesh.vertices.is_empty() {
                        return;
                    }
                }

                self.record(mesh);
            }
        }
        #[cfg(feature = "csg")]
        let cross = if cut_openings {
            let c = crate::mesh::cut_openings::CrossProductCut::from_indexer(
                &idx.voids_opening,
                &idx.voids_host,
            );
            if c.is_empty() { None } else { Some(c) }
        } else {
            None
        };
        let mut sink = QtoSink {
            unit_scale,
            cut_openings,
            guid: Vec::with_capacity(idx.product_step_id.len()),
            entity: Vec::with_capacity(idx.product_step_id.len()),
            volume_m3: Vec::with_capacity(idx.product_step_id.len()),
            volume_mesh_m3: Vec::with_capacity(idx.product_step_id.len()),
            volume_prism_bound_m3: Vec::with_capacity(idx.product_step_id.len()),
            volume_reliable: Vec::with_capacity(idx.product_step_id.len()),
            volume_method: Vec::with_capacity(idx.product_step_id.len()),
            mesh_quality: Vec::with_capacity(idx.product_step_id.len()),
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
            cut_stats: crate::mesh::cut_stats::CutOpeningsStats::default(),
            #[cfg(feature = "csg")]
            cross,
        };

        let t_mesh = Instant::now();
        // Local frame: QTO is translation-invariant, so meshing the shape
        // near origin gives correct volume/area/orientation AND stays
        // precise for far-from-origin objects (georeferenced MEP) that
        // would otherwise collapse into an f32-quantised point and report
        // surface_count = 0.
        let mesh_stats =
            py.detach(|| mesh_ifc_streaming_framed(&mmap, &mut sink, BakeFrame::Local));

        // Cross-product flush — mirror extract_meshes. Fold every
        // buffered host with its arrived openings and run the folded
        // mesh through the same QTO recorder.
        #[cfg(feature = "csg")]
        if let Some(mut cross) = sink.cross.take() {
            let prism_table = prism_table_for_flush(&mmap);
            for (folded, outcome) in cross.flush(sink.unit_scale, prism_table.as_ref()) {
                sink.bump_outcome(outcome);
                if folded.indices.is_empty() || folded.vertices.is_empty() {
                    continue;
                }
                sink.record(folded);
            }
        }
        let mesh_ms = t_mesh.elapsed().as_secs_f64() * 1000.0;

        let out = PyDict::new(py);
        out.set_item("guid", PyList::new(py, sink.guid)?)?;
        out.set_item("entity", PyList::new(py, sink.entity)?)?;
        out.set_item("volume_m3", PyList::new(py, sink.volume_m3)?)?;
        out.set_item("volume_mesh_m3", PyList::new(py, sink.volume_mesh_m3)?)?;
        out.set_item(
            "volume_prism_bound_m3",
            PyList::new(py, sink.volume_prism_bound_m3)?,
        )?;
        out.set_item("volume_reliable", PyList::new(py, sink.volume_reliable)?)?;
        out.set_item("volume_method", PyList::new(py, sink.volume_method)?)?;
        out.set_item("mesh_quality", PyList::new(py, sink.mesh_quality)?)?;
        out.set_item("aabb_volume_m3", PyList::new(py, sink.aabb_volume_m3)?)?;
        out.set_item("surface_area_m2", PyList::new(py, sink.surface_area_m2)?)?;
        out.set_item("area_top_m2", PyList::new(py, sink.area_top_m2)?)?;
        out.set_item("area_bottom_m2", PyList::new(py, sink.area_bottom_m2)?)?;
        out.set_item("area_side_m2", PyList::new(py, sink.area_side_m2)?)?;
        out.set_item("area_inclined_m2", PyList::new(py, sink.area_inclined_m2)?)?;
        out.set_item("largest_surface_m2", PyList::new(py, sink.largest_surface_m2)?)?;
        out.set_item("smallest_surface_m2", PyList::new(py, sink.smallest_surface_m2)?)?;
        out.set_item("surface_count", PyList::new(py, sink.surface_count)?)?;
        out.set_item("surface_guid", PyList::new(py, sink.s_guid)?)?;
        out.set_item("surface_index", PyList::new(py, sink.s_index)?)?;
        out.set_item("surface_area_m2_long", PyList::new(py, sink.s_area_m2)?)?;
        out.set_item("surface_nx", PyList::new(py, sink.s_nx)?)?;
        out.set_item("surface_ny", PyList::new(py, sink.s_ny)?)?;
        out.set_item("surface_nz", PyList::new(py, sink.s_nz)?)?;
        out.set_item("unit_scale", unit_scale as f64)?;
        out.set_item("indexer_ms", idx_ms)?;
        out.set_item("mesh_ms", mesh_ms)?;
        out.set_item("entity_table_ms", mesh_stats.entity_table_build_ms)?;
        out.set_item("total_ms", t_total.elapsed().as_secs_f64() * 1000.0)?;
        out.set_item("products_meshed", mesh_stats.products_meshed)?;
        out.set_item("size_bytes", mmap.len() as u64)?;
        out.set_item("cut_openings", cut_openings)?;
        set_cut_openings_stats(&out, &sink.cut_stats)?;
        Ok(out)
        })
    }

    #[cfg(feature = "mesh")]
    #[pyfunction]
    pub fn analyse_drift<'py>(py: Python<'py>, path: &str) -> PyResult<Bound<'py, PyDict>> {
        catch_panic(|| {
        let t_total = Instant::now();
        let (mmap, _open_ms) = open_mmap(path)?;
        // Unit scale (metres per model unit) for column rescaling.
        // mm-unit files: 0.001; metre files: 1.0. We index up front
        // because the drift consumer (Python) needs SI columns by
        // contract — see `m.drift` docstring. ~indexer cost is small
        // next to the mesh pass that follows.
        let idx = py.detach(|| indexer::index(&mmap));
        let unit_scale = idx.unit_scale.unwrap_or(1.0) as f32;
        let us_len = unit_scale;
        let us_area = unit_scale * unit_scale;
        let us_vol = unit_scale * unit_scale * unit_scale;
        let (mut meshes, mesh_stats) = py.detach(|| crate::mesh::mesh_ifc(&mmap));
        // Drift is the geometry-validity signal layer: its centroid /
        // AABB / volume columns must describe the ELEMENT. Strip the
        // synthetic half-space stand-in slabs first, or every clipped
        // product reports a foreign-extent bbox (GH #66 — the very
        // "broken mesh" class drift exists to catch).
        for m in &mut meshes {
            crate::mesh::strip_synthetic_cutters(m);
        }
        let prod_stats: Vec<crate::mesh::stats::ProductStats> = meshes
            .iter()
            .map(crate::mesh::stats::ProductStats::from_mesh)
            .collect();
        let file_stats = crate::mesh::stats::FileStats::from_products(&prod_stats);

        let out = PyDict::new(py);
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
        let mut drift_severity: Vec<&'static str> = Vec::with_capacity(n);
        let mut aabb_volume = Vec::with_capacity(n);
        let mut mesh_quality = Vec::with_capacity(n);
        let mut meshed_total = 0u32;
        let mut raw_error_count = 0u32;
        for s in &prod_stats {
            let px_m = s.placement_x * us_len;
            let py_m = s.placement_y * us_len;
            let pz_m = s.placement_z * us_len;
            let cx_m = ((s.xmin + s.xmax) * 0.5) * us_len;
            let cy_m = ((s.ymin + s.ymax) * 0.5) * us_len;
            let cz_m = ((s.zmin + s.zmax) * 0.5) * us_len;
            let drift_m = s.drift_distance * us_len;
            let extent_m = s.max_extent * us_len;
            // Per-row severity, recomputed against SI values so the
            // 10 mm absolute threshold is unit-independent (the old
            // `drift < 10.0` rule against raw model units was 10 mm
            // on mm-files but 10 m on metre-files — over-strict on
            // mm files and over-lenient on metre files).
            let severity: &'static str = if drift_m < 0.010 || s.drift_ratio <= 2.0 {
                "ok"
            } else if s.drift_ratio <= 10.0 {
                "warn"
            } else {
                "error"
            };
            guid.push(s.guid.clone());
            entity.push(s.entity.clone());
            source.push(s.source);
            tri_count.push(s.triangle_count);
            surface_area.push(s.surface_area * us_area);
            volume_abs.push(s.volume.abs() * us_vol);
            px.push(px_m);
            py_v.push(py_m);
            pz.push(pz_m);
            cx.push(cx_m);
            cy.push(cy_m);
            cz.push(cz_m);
            drift_distance.push(drift_m);
            max_extent.push(extent_m);
            drift_ratio.push(s.drift_ratio);
            drift_severity.push(severity);
            aabb_volume.push(s.aabb_volume * us_vol);
            mesh_quality.push(s.mesh_quality);

            meshed_total += 1;
            if severity == "error" {
                raw_error_count += 1;
            }
        }

        // Model-level drift-pattern detector — see
        // `crate::mesh::stats::is_world_coordinate_baked` for the
        // heuristic and the GH #33 rationale.
        let world_coord_baked =
            crate::mesh::stats::is_world_coordinate_baked(meshed_total, raw_error_count);
        if world_coord_baked {
            for sev in drift_severity.iter_mut() {
                if *sev == "error" || *sev == "warn" {
                    *sev = "info";
                }
            }
        }

        // Recount severity buckets post-demotion so file-level
        // counts agree with what `drift_severity` actually emits.
        let mut sev_ok = 0u32;
        let mut sev_warn = 0u32;
        let mut sev_error = 0u32;
        let mut sev_info = 0u32;
        for sev in &drift_severity {
            match *sev {
                "ok" => sev_ok += 1,
                "warn" => sev_warn += 1,
                "error" => sev_error += 1,
                "info" => sev_info += 1,
                _ => {}
            }
        }
        out.set_item("guid", PyList::new(py, guid)?)?;
        out.set_item("entity", PyList::new(py, entity)?)?;
        out.set_item("source", PyList::new(py, source)?)?;
        out.set_item("triangle_count", PyList::new(py, tri_count)?)?;
        out.set_item("surface_area_m2", PyList::new(py, surface_area)?)?;
        out.set_item("volume_abs_m3", PyList::new(py, volume_abs)?)?;
        out.set_item("placement_x_m", PyList::new(py, px)?)?;
        out.set_item("placement_y_m", PyList::new(py, py_v)?)?;
        out.set_item("placement_z_m", PyList::new(py, pz)?)?;
        out.set_item("centroid_x_m", PyList::new(py, cx)?)?;
        out.set_item("centroid_y_m", PyList::new(py, cy)?)?;
        out.set_item("centroid_z_m", PyList::new(py, cz)?)?;
        out.set_item("drift_distance_m", PyList::new(py, drift_distance)?)?;
        out.set_item("max_extent_m", PyList::new(py, max_extent)?)?;
        out.set_item("drift_ratio", PyList::new(py, drift_ratio)?)?;
        out.set_item("drift_severity", PyList::new(py, drift_severity)?)?;
        out.set_item("aabb_volume_m3", PyList::new(py, aabb_volume)?)?;
        out.set_item("mesh_quality", PyList::new(py, mesh_quality)?)?;
        out.set_item("unit_scale", unit_scale as f64)?;
        // Use the SI-recomputed counts so file-level totals agree
        // with the per-row severity actually emitted (after the
        // world-coordinate-baked demotion, if any).
        out.set_item("drift_ok", sev_ok)?;
        out.set_item("drift_warn", sev_warn)?;
        out.set_item("drift_error", sev_error)?;
        out.set_item("drift_info", sev_info)?;
        out.set_item("world_coordinate_baked", world_coord_baked)?;
        // Silence the unused-binding warning on builds where these
        // raw-unit FileStats counters aren't surfaced. They're still
        // populated by `FileStats::from_products` and used by the
        // CLI `analyse_drift` text path elsewhere.
        let _ = (file_stats.drift_ok, file_stats.drift_warn, file_stats.drift_error);

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
        out.set_item("seg_guid", PyList::new(py, seg_guid)?)?;
        out.set_item("seg_product_index", PyList::new(py, seg_product_index)?)?;
        out.set_item("seg_index", PyList::new(py, seg_index)?)?;
        out.set_item("seg_source", PyList::new(py, seg_source)?)?;
        out.set_item("seg_triangle_count", PyList::new(py, seg_triangle_count)?)?;
        out.set_item("seg_index_start", PyList::new(py, seg_index_start)?)?;

        out.set_item("mesh_emission_ms", mesh_stats.elapsed_ms)?;
        out.set_item("entity_table_ms", mesh_stats.entity_table_build_ms)?;
        out.set_item("total_ms", t_total.elapsed().as_secs_f64() * 1000.0)?;
        out.set_item("size_bytes", mmap.len() as u64)?;
        Ok(out)
        })
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
        catch_panic(|| {
        use crate::mesh::{sample::sample as sample_mesh, BakeFrame, ProductMesh, ProductSink};

        let t_total = Instant::now();
        let (mmap, _open_ms) = open_mmap(path)?;
        let t_idx = Instant::now();
        let idx = py.detach(|| indexer::index(&mmap));
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
            fn on_product(&mut self, mut mesh: ProductMesh) {
                // Area-weighted sampling walks every triangle — a
                // synthetic ±20 000-unit half-space slab would soak up
                // nearly all of a product's point budget (GH #66).
                crate::mesh::strip_synthetic_cutters(&mut mesh);
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
        let mesh_stats = py.detach(|| {
            crate::mesh::mesh_ifc_streaming_framed(&mmap, &mut sink, BakeFrame::Local)
        });
        let mesh_ms = t_mesh.elapsed().as_secs_f64() * 1000.0;

        let t_marshal = Instant::now();
        let out = PyDict::new(py);
        out.set_item("guid", PyList::new(py, &sink.guid)?)?;
        out.set_item("entity", PyList::new(py, &sink.entity)?)?;
        out.set_item("x", PyList::new(py, &sink.x)?)?;
        out.set_item("y", PyList::new(py, &sink.y)?)?;
        out.set_item("z", PyList::new(py, &sink.z)?)?;
        out.set_item("nx", PyList::new(py, &sink.nx)?)?;
        out.set_item("ny", PyList::new(py, &sink.ny)?)?;
        out.set_item("nz", PyList::new(py, &sink.nz)?)?;
        out.set_item("unit_scale", unit_scale as f64)?;
        // Global shift in METRES: add this back to (x, y, z) to recover
        // absolute world coordinates. `[0, 0, 0]` when the model has no
        // geometry or already sits near origin.
        let gs = sink.shift.unwrap_or([0.0, 0.0, 0.0]);
        let us = unit_scale as f64;
        out.set_item(
            "global_shift",
            PyList::new(py, [gs[0] * us, gs[1] * us, gs[2] * us])?,
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
        })
    }

    // ----- iter_point_cloud --------------------------------------------

    // Streaming point-cloud generator (GH #23). The single-shot
    // [`sample_point_cloud`] materialises every sampled point into one
    // dict before returning — for 200 MB – 1 GB ARK IFCs the dict
    // doesn't fit in 32 GB RAM and the failure modes (Arrow realloc,
    // Python MemoryError, uncatchable Rust panic) lock the host. The
    // iterator caps peak RAM at `chunk_points` rows by emitting chunks
    // through a bounded mpsc channel from a worker thread.

    /// Per-chunk payload sent over the worker→iterator channel. All
    /// coordinates are in METRES (matching the single-shot
    /// `sample_point_cloud` contract); the Python wrapper applies the
    /// user's output unit factor per chunk.
    #[cfg(feature = "mesh")]
    struct CloudChunk {
        guid: Vec<String>,
        entity: Vec<String>,
        x: Vec<f32>,
        y: Vec<f32>,
        z: Vec<f32>,
        nx: Vec<f32>,
        ny: Vec<f32>,
        nz: Vec<f32>,
        /// Same value across every chunk for a given file — the model-
        /// wide CloudCompare shift, in metres. Carried per chunk so the
        /// Python iterator can set `df.attrs["global_shift"]` without
        /// waiting for the worker to finish.
        global_shift_m: [f64; 3],
    }

    /// Per-call worker→iterator channel item: either a chunk or a
    /// human-readable error message (panic text caught at the worker
    /// boundary, surfaced as `IfcfastError` in `__next__`).
    #[cfg(feature = "mesh")]
    type CloudChunkResult = Result<CloudChunk, String>;

    /// Streaming `ProductSink` that buffers sampled points and flushes
    /// every `chunk_points` rows over a bounded channel. Honours an
    /// `Arc<AtomicBool>` stop flag so dropping the Python iterator
    /// short-circuits the rest of the mesh pass instead of doing full
    /// tessellation only to discard it.
    #[cfg(feature = "mesh")]
    struct StreamingCloudSink {
        per_m2: f32,
        seed: u64,
        area_scale: f32,
        unit_scale: f32,
        chunk_points: usize,
        shift: Option<[f64; 3]>,
        // Working buffer. Flushed (drained) when len >= chunk_points.
        buf_guid: Vec<String>,
        buf_entity: Vec<String>,
        buf_x: Vec<f32>,
        buf_y: Vec<f32>,
        buf_z: Vec<f32>,
        buf_nx: Vec<f32>,
        buf_ny: Vec<f32>,
        buf_nz: Vec<f32>,
        tx: std::sync::mpsc::SyncSender<CloudChunkResult>,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    #[cfg(feature = "mesh")]
    impl StreamingCloudSink {
        #[inline]
        fn buf_len(&self) -> usize {
            self.buf_x.len()
        }

        /// Move the current buffer into a `CloudChunk` and send it. The
        /// shift must already be set before any flush (we only push to
        /// the buffer after `shift.get_or_insert_with`).
        fn flush(&mut self) {
            let shift = self.shift.expect("shift set before any flush");
            let us = self.unit_scale as f64;
            let shift_m = [shift[0] * us, shift[1] * us, shift[2] * us];
            let chunk = CloudChunk {
                guid: std::mem::take(&mut self.buf_guid),
                entity: std::mem::take(&mut self.buf_entity),
                x: std::mem::take(&mut self.buf_x),
                y: std::mem::take(&mut self.buf_y),
                z: std::mem::take(&mut self.buf_z),
                nx: std::mem::take(&mut self.buf_nx),
                ny: std::mem::take(&mut self.buf_ny),
                nz: std::mem::take(&mut self.buf_nz),
                global_shift_m: shift_m,
            };
            if self.tx.send(Ok(chunk)).is_err() {
                // Consumer dropped the iterator — flip the stop flag so
                // the rest of the mesh pass returns early.
                self.stop
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }

        /// Final flush at end-of-stream. Only emits if there's a partial
        /// buffer; no-op for files that produced no points.
        fn flush_final(mut self) {
            if self.buf_len() > 0 && self.shift.is_some() {
                self.flush();
            }
        }
    }

    #[cfg(feature = "mesh")]
    impl crate::mesh::ProductSink for StreamingCloudSink {
        fn on_product(&mut self, mut mesh: crate::mesh::ProductMesh) {
            use crate::mesh::sample::sample as sample_mesh;
            if self
                .stop
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                return;
            }
            // Same contract as the batch CloudSink: never sample the
            // synthetic half-space stand-in slabs (GH #66).
            crate::mesh::strip_synthetic_cutters(&mut mesh);
            // Per-product splitmix64-derived seed — same scheme as the
            // single-shot `sample_point_cloud` so bit-identical output
            // is preserved across the streaming and batch APIs at a
            // given (file, per_m2, seed).
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
            let shift = *self
                .shift
                .get_or_insert_with(|| global_shift_for(&mesh.mesh_anchor, self.unit_scale));
            let off = [
                mesh.mesh_anchor[0] - shift[0],
                mesh.mesh_anchor[1] - shift[1],
                mesh.mesh_anchor[2] - shift[2],
            ];
            let us = self.unit_scale as f64;
            // Reserve to keep `extend` allocations bounded; cap at the
            // remaining-to-chunk-boundary count so a single huge product
            // doesn't briefly balloon the buffer past chunk_points.
            for i in 0..n {
                self.buf_x
                    .push(((cloud.x[i] as f64 + off[0]) * us) as f32);
                self.buf_y
                    .push(((cloud.y[i] as f64 + off[1]) * us) as f32);
                self.buf_z
                    .push(((cloud.z[i] as f64 + off[2]) * us) as f32);
                self.buf_nx.push(cloud.nx[i]);
                self.buf_ny.push(cloud.ny[i]);
                self.buf_nz.push(cloud.nz[i]);
                self.buf_guid.push(mesh.guid.clone());
                self.buf_entity.push(mesh.entity.clone());
                if self.buf_len() >= self.chunk_points {
                    self.flush();
                    if self
                        .stop
                        .load(std::sync::atomic::Ordering::Relaxed)
                    {
                        return;
                    }
                }
            }
        }
    }

    /// Iterator over chunks of a streaming point cloud. Construct via
    /// [`iter_point_cloud`]; each `__next__` returns a Python dict
    /// matching the per-chunk columns of [`sample_point_cloud`], or
    /// `None` (StopIteration) when the worker is drained.
    #[cfg(feature = "mesh")]
    #[pyclass(unsendable)]
    pub struct PointCloudIter {
        rx: Option<std::sync::mpsc::Receiver<CloudChunkResult>>,
        worker: Option<std::thread::JoinHandle<()>>,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        // Echoed back on each chunk dict — handy for Python wrappers
        // building DataFrames + stamping shift on `.attrs`.
        per_m2: f32,
        seed: u64,
    }

    #[cfg(feature = "mesh")]
    impl PointCloudIter {
        /// Signal the worker to stop and detach the channel. Called on
        /// `Drop` and after the worker reports either StopIteration or
        /// a panic, so subsequent `__next__` calls just return None.
        fn close(&mut self) {
            self.stop
                .store(true, std::sync::atomic::Ordering::Relaxed);
            self.rx.take();
        }
    }

    #[cfg(feature = "mesh")]
    impl Drop for PointCloudIter {
        fn drop(&mut self) {
            self.stop
                .store(true, std::sync::atomic::Ordering::Relaxed);
            // Detach the receiver so the worker's next send fails fast
            // and it exits without blocking. Don't join — the worker
            // may still be inside a `mesh_ifc_streaming_framed` phase;
            // letting it run to completion in the background is fine.
            self.rx.take();
            // Discard the JoinHandle; the thread cleans up itself.
            self.worker.take();
        }
    }

    #[cfg(feature = "mesh")]
    #[pymethods]
    impl PointCloudIter {
        fn __iter__(slf: PyRef<Self>) -> PyRef<Self> {
            slf
        }

        fn __next__<'py>(
            &mut self,
            py: Python<'py>,
        ) -> PyResult<Option<Bound<'py, PyDict>>> {
            let Some(rx) = self.rx.take() else {
                return Ok(None);
            };
            // Release the GIL while waiting for the worker; without
            // this, Python is fully blocked through every chunk's
            // tessellation phase and the iterator stops being useful
            // (e.g. interrupts can't fire, parallel Python work stalls).
            //
            // `Receiver` is `Send` but not `Sync`, so we move it into
            // the closure rather than borrowing — pyo3's `Ungil` bound
            // requires `Send` for everything captured under
            // `detach` (formerly `allow_threads`). After the recv returns
            // we put the receiver back so the next `__next__` can reuse it.
            let (rx, received) = py.detach(move || {
                let r = rx.recv();
                (rx, r)
            });
            self.rx = Some(rx);
            match received {
                Ok(Ok(chunk)) => {
                    let out = PyDict::new(py);
                    out.set_item("guid", PyList::new(py, &chunk.guid)?)?;
                    out.set_item("entity", PyList::new(py, &chunk.entity)?)?;
                    out.set_item("x", PyList::new(py, &chunk.x)?)?;
                    out.set_item("y", PyList::new(py, &chunk.y)?)?;
                    out.set_item("z", PyList::new(py, &chunk.z)?)?;
                    out.set_item("nx", PyList::new(py, &chunk.nx)?)?;
                    out.set_item("ny", PyList::new(py, &chunk.ny)?)?;
                    out.set_item("nz", PyList::new(py, &chunk.nz)?)?;
                    out.set_item(
                        "global_shift",
                        PyList::new(py, chunk.global_shift_m)?,
                    )?;
                    out.set_item("per_m2", self.per_m2 as f64)?;
                    out.set_item("seed", self.seed)?;
                    out.set_item("points_in_chunk", chunk.x.len() as u64)?;
                    Ok(Some(out))
                }
                Ok(Err(msg)) => {
                    self.close();
                    Err(PyErr::new::<IfcfastError, _>(msg))
                }
                Err(_) => {
                    // Channel closed — worker finished cleanly.
                    self.close();
                    Ok(None)
                }
            }
        }
    }

    /// Build a streaming point-cloud iterator for `path`. The full
    /// mesh pass runs on a worker thread and pushes chunks of
    /// `chunk_points` rows through a bounded channel — peak RAM is
    /// O(chunk_points), independent of total point count. Each chunk
    /// dict has the same per-column shape as [`sample_point_cloud`]'s
    /// output plus a per-chunk `global_shift` (same value across all
    /// chunks for a given file).
    ///
    /// Panics inside the worker (the GH #23 third failure mode) are
    /// caught and surfaced as `IfcfastError` on the next `__next__`,
    /// not as the uncatchable `pyo3_runtime.PanicException`.
    #[cfg(feature = "mesh")]
    #[pyfunction]
    #[pyo3(signature = (path, per_m2, seed, chunk_points = 1_000_000))]
    pub fn iter_point_cloud(
        path: &str,
        per_m2: f32,
        seed: u64,
        chunk_points: usize,
    ) -> PyResult<PointCloudIter> {
        catch_panic(|| {
            if chunk_points == 0 {
                return Err(PyErr::new::<IfcfastError, _>(
                    "chunk_points must be > 0",
                ));
            }
            // Open the file on the calling thread so I/O errors surface
            // synchronously (matches `sample_point_cloud` / `extract_meshes`
            // behaviour) instead of being smuggled through the channel.
            let (mmap, _open_ms) = open_mmap(path)?;
            // Channel bound = 2 chunks. At 1M points × ~50 B/row ≈ 50 MB
            // per chunk, that's ~100 MB of in-flight backpressure — small
            // enough not to dominate RAM, large enough that the worker
            // never starves when Python is mid-DataFrame-build.
            let (tx, rx) =
                std::sync::mpsc::sync_channel::<CloudChunkResult>(2);
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let worker_tx = tx.clone();
            let worker_stop = stop.clone();
            let worker = std::thread::spawn(move || {
                // Run the entire mesh pass inside catch_unwind; on
                // panic, ship the message back over the channel so the
                // Python `__next__` raises `IfcfastError` instead of
                // the uncatchable `pyo3_runtime.PanicException`.
                let result = catch_unwind(AssertUnwindSafe(|| {
                    let idx = crate::indexer::index(&mmap);
                    let unit_scale = idx.unit_scale.unwrap_or(1.0) as f32;
                    let area_scale = unit_scale * unit_scale;
                    let sink = StreamingCloudSink {
                        per_m2,
                        seed,
                        area_scale,
                        unit_scale,
                        chunk_points,
                        shift: None,
                        buf_guid: Vec::new(),
                        buf_entity: Vec::new(),
                        buf_x: Vec::new(),
                        buf_y: Vec::new(),
                        buf_z: Vec::new(),
                        buf_nx: Vec::new(),
                        buf_ny: Vec::new(),
                        buf_nz: Vec::new(),
                        tx: worker_tx.clone(),
                        stop: worker_stop.clone(),
                    };
                    let mut sink = sink;
                    crate::mesh::mesh_ifc_streaming_framed(
                        &mmap,
                        &mut sink,
                        crate::mesh::BakeFrame::Local,
                    );
                    sink.flush_final();
                }));
                if let Err(payload) = result {
                    let msg = format!(
                        "ifcfast Rust panic (point cloud worker): {}",
                        panic_payload_to_string(payload)
                    );
                    // Best-effort send — if receiver dropped, drop too.
                    let _ = worker_tx.send(Err(msg));
                }
                // tx drops here, closing the channel and signaling
                // StopIteration to the iterator.
                drop(worker_tx);
            });
            Ok(PointCloudIter {
                rx: Some(rx),
                worker: Some(worker),
                stop,
                per_m2,
                seed,
            })
        })
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
    #[pyo3(signature = (path, cut_openings = false, keep_cutters = false))]
    pub fn extract_meshes<'py>(
        py: Python<'py>,
        path: &str,
        cut_openings: bool,
        keep_cutters: bool,
    ) -> PyResult<Bound<'py, PyDict>> {
        catch_panic(|| {
        use crate::mesh::{BakeFrame, ProductMesh, ProductSink};

        // cut_openings requires the `csg` feature — surface a clear
        // error rather than silently ignoring the flag.
        #[cfg(not(feature = "csg"))]
        if cut_openings {
            return Err(pyo3::exceptions::PyRuntimeError::new_err(
                "extract_meshes(cut_openings=True) requires the `csg` Cargo feature; \
                 this wheel was built without it. Build with \
                 `pip install ifcfast[csg]` (once published) or \
                 `maturin develop --features csg` from source.",
            ));
        }

        let t_total = Instant::now();
        let (mmap, _open_ms) = open_mmap(path)?;

        // Linear-unit-to-metres factor — vertices are scaled to metres
        // so the output matches the metres contract (and mesh_qto). The
        // indexer pass is the same source point_cloud uses.
        let idx = py.detach(|| indexer::index(&mmap));
        let unit_scale = idx.unit_scale.unwrap_or(1.0) as f32;

        struct MeshSink {
            unit_scale: f32,
            cut_openings: bool,
            // Reveal-all opt-in (GH #66): keep the synthetic half-space
            // visualisation slabs in the no-cut output. Default `false`
            // — they are tool geometry with foreign extent (±20 000
            // model units), not element geometry.
            keep_cutters: bool,
            cutters_stripped: u64,
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
            cut_stats: crate::mesh::cut_stats::CutOpeningsStats,
            // Cross-product IfcRelVoidsElement buffer. Some(_) only
            // when cut_openings && the file has at least one void
            // relation; otherwise None keeps the hot path identical
            // to the no-cut behaviour. Stored in an Option so the
            // sink wrapper can mem::take it for the flush phase
            // without holding two mutable borrows simultaneously.
            #[cfg(feature = "csg")]
            cross: Option<crate::mesh::cut_openings::CrossProductCut>,
        }

        impl MeshSink {
            /// Take a (presumed cut-applied, non-empty) mesh and
            /// push its scaled+rebased byte buffers onto the output
            /// columns. Shared between the streaming `on_product`
            /// path and the post-stream cross-product flush.
            fn encode(&mut self, mesh: ProductMesh) {
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

            #[cfg(feature = "csg")]
            fn bump_outcome(&mut self, outcome: crate::mesh::cut_stats::Outcome) {
                outcome.accumulate(&mut self.cut_stats);
            }
        }

        impl ProductSink for MeshSink {
            fn on_product(&mut self, mut mesh: ProductMesh) {
                // Skip geometryless products — no triangles to hand back.
                if mesh.indices.is_empty() || mesh.vertices.is_empty() {
                    return;
                }
                #[cfg(feature = "csg")]
                if self.cut_openings {
                    // Cross-product routing first: openings get
                    // suppressed, hosts get held for the flush phase,
                    // everything else continues into the in-rep apply.
                    if let Some(cross) = self.cross.as_mut() {
                        use crate::mesh::cut_openings::Routed;
                        match cross.route(mesh) {
                            Routed::Suppressed | Routed::Held => return,
                            Routed::PassThrough(m) => mesh = m,
                        }
                    }
                    let outcome = crate::mesh::cut_openings::apply(&mut mesh, self.unit_scale);
                    self.bump_outcome(outcome);
                    // The cut may have emptied the mesh (cutter fully
                    // consumed the host); skip in that case.
                    if mesh.indices.is_empty() || mesh.vertices.is_empty() {
                        return;
                    }
                }
                #[cfg(not(feature = "csg"))]
                let _ = self.cut_openings; // silence unused-field warning

                // When the cut did NOT run, the reveal-all fragments
                // still include the synthetic half-space stand-in slabs
                // (±20 000 model units — GH #66). Strip them unless the
                // caller explicitly asked for reveal-all geometry.
                let cut_applied = cfg!(feature = "csg") && self.cut_openings;
                if !cut_applied && !self.keep_cutters {
                    self.cutters_stripped +=
                        crate::mesh::strip_synthetic_cutters(&mut mesh) as u64;
                    if mesh.indices.is_empty() || mesh.vertices.is_empty() {
                        return;
                    }
                }

                self.encode(mesh);
            }
        }

        // Build the cross-product void index from the indexer's
        // parallel arrays. `from_indexer` collapses to an empty
        // CrossProductCut when no IfcRelVoidsElement exists, so we
        // only carry the struct when both the flag is on AND there's
        // actually work for it to do.
        #[cfg(feature = "csg")]
        let cross = if cut_openings {
            let c = crate::mesh::cut_openings::CrossProductCut::from_indexer(
                &idx.voids_opening,
                &idx.voids_host,
            );
            if c.is_empty() { None } else { Some(c) }
        } else {
            None
        };

        let mut sink = MeshSink {
            unit_scale,
            cut_openings,
            keep_cutters,
            cutters_stripped: 0,
            shift: None,
            guid: Vec::new(),
            entity: Vec::new(),
            vertex_count: Vec::new(),
            triangle_count: Vec::new(),
            vertices_le: Vec::new(),
            indices_le: Vec::new(),
            cut_stats: crate::mesh::cut_stats::CutOpeningsStats::default(),
            #[cfg(feature = "csg")]
            cross,
        };
        let t_mesh = Instant::now();
        // Local frame + per-product f64 shift — see sample_point_cloud.
        let mesh_stats = py.detach(|| {
            crate::mesh::mesh_ifc_streaming_framed(&mmap, &mut sink, BakeFrame::Local)
        });

        // Cross-product flush. After the streaming pass, fold every
        // buffered host with its arrived openings and run the result
        // through the same encode path. Stats accumulate per host.
        #[cfg(feature = "csg")]
        if let Some(mut cross) = sink.cross.take() {
            let prism_table = prism_table_for_flush(&mmap);
            for (folded, outcome) in cross.flush(sink.unit_scale, prism_table.as_ref()) {
                sink.bump_outcome(outcome);
                if folded.indices.is_empty() || folded.vertices.is_empty() {
                    continue;
                }
                sink.encode(folded);
            }
        }
        let mesh_ms = t_mesh.elapsed().as_secs_f64() * 1000.0;

        let t_marshal = Instant::now();
        let out = PyDict::new(py);
        out.set_item("guid", PyList::new(py, &sink.guid)?)?;
        out.set_item("entity", PyList::new(py, &sink.entity)?)?;
        out.set_item("vertex_count", PyList::new(py, &sink.vertex_count)?)?;
        out.set_item("triangle_count", PyList::new(py, &sink.triangle_count)?)?;
        let verts: Vec<Bound<'py, PyBytes>> = sink
            .vertices_le
            .iter()
            .map(|b| PyBytes::new(py, b))
            .collect();
        let inds: Vec<Bound<'py, PyBytes>> = sink
            .indices_le
            .iter()
            .map(|b| PyBytes::new(py, b))
            .collect();
        out.set_item("vertices", PyList::new(py, verts)?)?;
        out.set_item("indices", PyList::new(py, inds)?)?;
        // Global shift in METRES — add back to vertices for absolute
        // world coords. `[0, 0, 0]` for near-origin or empty models.
        let gs = sink.shift.unwrap_or([0.0, 0.0, 0.0]);
        let us = unit_scale as f64;
        out.set_item(
            "global_shift",
            PyList::new(py, [gs[0] * us, gs[1] * us, gs[2] * us])?,
        )?;
        out.set_item("products_meshed", mesh_stats.products_meshed as u64)?;
        out.set_item("mesh_ms", mesh_ms)?;
        out.set_item("marshal_ms", t_marshal.elapsed().as_secs_f64() * 1000.0)?;
        out.set_item("total_ms", t_total.elapsed().as_secs_f64() * 1000.0)?;
        out.set_item("size_bytes", mmap.len() as u64)?;
        out.set_item("cut_openings", cut_openings)?;
        out.set_item("keep_cutters", keep_cutters)?;
        out.set_item("cutters_stripped", sink.cutters_stripped)?;
        set_cut_openings_stats(&out, &sink.cut_stats)?;
        Ok(out)
        })
    }

    // ----- write_gltf --------------------------------------------------

    /// Write `path` (an IFC file) as `out_path` (a `.glb`), running the
    /// streaming mesh pass, optionally cutting openings, and emitting
    /// glTF 2.0 binary with `EXT_mesh_gpu_instancing` (where applicable)
    /// + `KHR_mesh_quantization` baked positions.
    ///
    /// `cut_openings=True` applies the manifold-csg net-boolean path
    /// (`m.meshes(cut_openings=True)` semantics): in-rep
    /// `IfcBooleanClippingResult` AND cross-product `IfcRelVoidsElement`.
    /// Instancing is disabled in this mode because the cut produces
    /// per-product geometry that no longer matches the shared rep mesh
    /// — every product gets a baked node instead. Quantization (u16
    /// positions per node) still applies.
    ///
    /// Returns a small dict of stats: `products_meshed`,
    /// `products_emitted`, `cut_openings_*` counts, output file size.
    #[cfg(feature = "mesh")]
    #[pyfunction]
    #[pyo3(signature = (path, out_path, cut_openings = false))]
    pub fn write_gltf<'py>(
        py: Python<'py>,
        path: &str,
        out_path: &str,
        cut_openings: bool,
    ) -> PyResult<Bound<'py, PyDict>> {
        catch_panic(|| {
            use crate::mesh::{BakeFrame, ProductMesh, ProductSink};

            #[cfg(not(feature = "csg"))]
            if cut_openings {
                return Err(pyo3::exceptions::PyRuntimeError::new_err(
                    "write_gltf(cut_openings=True) requires the `csg` Cargo feature; \
                     this wheel was built without it.",
                ));
            }

            let t_total = Instant::now();
            let (mmap, _open_ms) = open_mmap(path)?;
            let idx = py.detach(|| indexer::index(&mmap));
            let unit_scale = idx.unit_scale.unwrap_or(1.0) as f32;

            /// Accumulating sink: collects every `ProductMesh` into a
            /// `Vec`, optionally routing cross-product void hosts and
            /// in-rep cut applies through the same dispatcher
            /// `extract_meshes` uses, then scales world-baked vertices
            /// from model units to metres so the glTF writer sees a
            /// metres-everywhere contract.
            struct GltfSink {
                products: Vec<ProductMesh>,
                cut_openings: bool,
                unit_scale: f32,
                cut_stats: crate::mesh::cut_stats::CutOpeningsStats,
                #[cfg(feature = "csg")]
                cross: Option<crate::mesh::cut_openings::CrossProductCut>,
            }

            impl GltfSink {
                /// Scale a world-baked product's vertices from model
                /// units into metres so the glTF emitter sees the same
                /// metres-everywhere contract `m.meshes()` ships. Also
                /// rebases against the model's f64 mesh_anchor so the
                /// f32 vertex stream stays precise for far-from-origin
                /// (georeferenced) geometry. The output is appended
                /// to `self.products`.
                fn push_scaled(&mut self, mut mesh: ProductMesh) {
                    if mesh.indices.is_empty() || mesh.vertices.is_empty() {
                        return;
                    }
                    let us = self.unit_scale as f64;
                    for chunk in mesh.vertices.chunks_exact_mut(3) {
                        chunk[0] = (chunk[0] as f64 * us) as f32;
                        chunk[1] = (chunk[1] as f64 * us) as f32;
                        chunk[2] = (chunk[2] as f64 * us) as f32;
                    }
                    self.products.push(mesh);
                }

                #[cfg(feature = "csg")]
                fn bump_outcome(&mut self, outcome: crate::mesh::cut_stats::Outcome) {
                    outcome.accumulate(&mut self.cut_stats);
                }
            }

            impl ProductSink for GltfSink {
                fn on_product(&mut self, mut mesh: ProductMesh) {
                    #[cfg(feature = "csg")]
                    if self.cut_openings {
                        if let Some(cross) = self.cross.as_mut() {
                            use crate::mesh::cut_openings::Routed;
                            match cross.route(mesh) {
                                Routed::Suppressed | Routed::Held => return,
                                Routed::PassThrough(m) => mesh = m,
                            }
                        }
                        let outcome = crate::mesh::cut_openings::apply(&mut mesh, self.unit_scale);
                        self.bump_outcome(outcome);
                    }
                    #[cfg(not(feature = "csg"))]
                    let _ = self.cut_openings;
                    // The viewer is the primary victim of the synthetic
                    // half-space stand-in slabs (GH #66) — never ship
                    // them in glTF output when the cut didn't run.
                    if !(cfg!(feature = "csg") && self.cut_openings) {
                        crate::mesh::strip_synthetic_cutters(&mut mesh);
                    }
                    self.push_scaled(mesh);
                }
            }

            #[cfg(feature = "csg")]
            let cross = if cut_openings {
                let c = crate::mesh::cut_openings::CrossProductCut::from_indexer(
                    &idx.voids_opening,
                    &idx.voids_host,
                );
                if c.is_empty() { None } else { Some(c) }
            } else {
                None
            };

            let mut sink = GltfSink {
                products: Vec::new(),
                cut_openings,
                unit_scale,
                cut_stats: crate::mesh::cut_stats::CutOpeningsStats::default(),
                #[cfg(feature = "csg")]
                cross,
            };

            let t_mesh = Instant::now();
            // World frame so the glTF writer can compute per-product
            // AABBs directly from `mesh.vertices`. The kernel already
            // applies the model-wide global shift to prevent far-from-
            // origin f32 collapse internally.
            let mesh_stats = py.detach(|| {
                crate::mesh::mesh_ifc_streaming_framed(
                    &mmap,
                    &mut sink,
                    BakeFrame::World,
                )
            });

            // Cross-product flush — fold buffered hosts with their
            // arrived openings, run the result through `push_scaled`.
            // `None`: this is a `BakeFrame::World` pass, so the host
            // mesh is in world coordinates — the near-origin prism
            // result would not align. Manifold fold only here.
            #[cfg(feature = "csg")]
            if let Some(mut cross) = sink.cross.take() {
                for (folded, outcome) in cross.flush(sink.unit_scale, None) {
                    sink.bump_outcome(outcome);
                    sink.push_scaled(folded);
                }
            }
            let mesh_ms = t_mesh.elapsed().as_secs_f64() * 1000.0;

            // Cut-applied meshes have geometry diverging from any
            // shared rep — disable instancing in that case so each
            // wall keeps its own cut.
            let options = crate::mesh::gltf::WriteOptions {
                instancing: !cut_openings,
            };

            let t_write = Instant::now();
            let file = std::fs::File::create(out_path).map_err(|e| {
                pyo3::exceptions::PyIOError::new_err(format!(
                    "create {out_path}: {e}"
                ))
            })?;
            let mut buf = std::io::BufWriter::with_capacity(1 << 20, file);
            crate::mesh::gltf::write_with_options(&sink.products, &options, &mut buf)
                .map_err(|e| {
                    pyo3::exceptions::PyIOError::new_err(format!(
                        "write {out_path}: {e}"
                    ))
                })?;
            use std::io::Write;
            buf.flush().map_err(|e| {
                pyo3::exceptions::PyIOError::new_err(format!(
                    "flush {out_path}: {e}"
                ))
            })?;
            let write_ms = t_write.elapsed().as_secs_f64() * 1000.0;

            let out_size = std::fs::metadata(out_path).map(|m| m.len()).unwrap_or(0);

            let out = PyDict::new(py);
            out.set_item("products_emitted", sink.products.len() as u64)?;
            out.set_item("products_meshed", mesh_stats.products_meshed as u64)?;
            out.set_item("triangles", mesh_stats.triangles as u64)?;
            out.set_item("mesh_ms", mesh_ms)?;
            out.set_item("write_ms", write_ms)?;
            out.set_item("total_ms", t_total.elapsed().as_secs_f64() * 1000.0)?;
            out.set_item("size_bytes", mmap.len() as u64)?;
            out.set_item("out_size_bytes", out_size)?;
            out.set_item("cut_openings", cut_openings)?;
            set_cut_openings_stats(&out, &sink.cut_stats)?;
            out.set_item("instancing", options.instancing)?;
            Ok(out)
        })
    }

    // ----- bundle -------------------------------------------------------

    /// Build the parquet substrate (`instances.parquet` +
    /// `representations.parquet` + `view.sql`) consumed by
    /// [`clash`]. Mirrors the standalone `ifcfast-bundle` binary,
    /// available in-process so the wheel can ship this without a
    /// separate native executable.
    ///
    /// Returns a dict of (paths, counts, timings) — the caller
    /// re-shapes it into whatever surface they want.
    #[cfg(feature = "bundle")]
    #[pyfunction]
    #[pyo3(signature = (ifc_path, out_dir = None))]
    fn bundle<'py>(
        py: Python<'py>,
        ifc_path: &str,
        out_dir: Option<&str>,
    ) -> PyResult<Bound<'py, PyDict>> {
        use crate::bundle::parquet_sink::ParquetSink;
        use crate::bundle::Bundle;
        use crate::mesh::mesh_ifc_streaming;

        const VIEW_SQL: &str = include_str!("bundle/view.sql");

        let in_path = Path::new(ifc_path).to_path_buf();
        let out_dir_path = match out_dir {
            Some(o) => std::path::PathBuf::from(o),
            None => {
                let stem = in_path
                    .file_stem()
                    .map(|s| s.to_owned())
                    .unwrap_or_default();
                let mut p = in_path.clone();
                p.set_file_name(format!("{}.bundle", stem.to_string_lossy()));
                p
            }
        };
        std::fs::create_dir_all(&out_dir_path).map_err(|e| {
            pyo3::exceptions::PyIOError::new_err(format!(
                "mkdir {}: {e}",
                out_dir_path.display()
            ))
        })?;

        let (src, open_ms) = open_mmap(ifc_path)?;
        let buf: &[u8] = &src;

        let t_bundle = Instant::now();
        let bundle = py.detach(|| Bundle::build(buf));
        let bundle_ms = t_bundle.elapsed().as_secs_f64() * 1000.0;
        let sem = bundle.semantic_stats();

        let mut sink = ParquetSink::create_in_dir(&out_dir_path, &bundle).map_err(|e| {
            pyo3::exceptions::PyIOError::new_err(format!(
                "create sink in {}: {e}",
                out_dir_path.display()
            ))
        })?;

        let t_stream = Instant::now();
        let stats = py.detach(|| mesh_ifc_streaming(buf, &mut sink));
        let stream_ms = t_stream.elapsed().as_secs_f64() * 1000.0;

        let (instances_written, reps_written) = sink.finish().map_err(|e| {
            pyo3::exceptions::PyIOError::new_err(format!(
                "finish substrate write in {}: {e}",
                out_dir_path.display()
            ))
        })?;

        let view_path = out_dir_path.join("view.sql");
        std::fs::write(&view_path, VIEW_SQL).map_err(|e| {
            pyo3::exceptions::PyIOError::new_err(format!(
                "write {}: {e}",
                view_path.display()
            ))
        })?;

        let rep_path = out_dir_path.join("representations.parquet");
        let inst_path = out_dir_path.join("instances.parquet");
        let rep_bytes = std::fs::metadata(&rep_path).map(|m| m.len()).unwrap_or(0);
        let inst_bytes = std::fs::metadata(&inst_path).map(|m| m.len()).unwrap_or(0);

        let out = PyDict::new(py);
        out.set_item("bundle_dir", out_dir_path.to_string_lossy().to_string())?;
        out.set_item("instances_parquet", inst_path.to_string_lossy().to_string())?;
        out.set_item("representations_parquet", rep_path.to_string_lossy().to_string())?;
        out.set_item("view_sql", view_path.to_string_lossy().to_string())?;
        out.set_item("products_indexed", sem.products_indexed as u64)?;
        out.set_item("pset_rows", sem.pset_rows as u64)?;
        out.set_item("material_rows", sem.material_rows as u64)?;
        out.set_item("quantity_rows", sem.quantity_rows as u64)?;
        out.set_item("classification_rows", sem.classification_rows as u64)?;
        out.set_item("products_seen", stats.products_seen as u64)?;
        out.set_item("products_meshed", stats.products_meshed as u64)?;
        out.set_item("products_deferred", stats.products_deferred as u64)?;
        out.set_item("triangles", stats.triangles as u64)?;
        out.set_item("instances_written", instances_written as u64)?;
        out.set_item("unique_reps_written", reps_written as u64)?;
        out.set_item("instances_parquet_bytes", inst_bytes)?;
        out.set_item("representations_parquet_bytes", rep_bytes)?;
        out.set_item("open_ms", open_ms)?;
        out.set_item("bundle_ms", bundle_ms)?;
        out.set_item("entity_table_build_ms", stats.entity_table_build_ms)?;
        out.set_item("stream_ms", stream_ms)?;
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
            .detach(|| run_clash(&bundle_dir, &opts))
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("clash: {e}")))?;
        let clash_ms = t.elapsed().as_secs_f64() * 1000.0;

        let mut written_path: Option<String> = None;
        if write_parquet {
            let out = bundle_dir.join("clashes.parquet");
            py.detach(|| write_clashes_parquet(&out, &report.pairs))
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
        let mut category: Vec<&'static str> = Vec::with_capacity(n);
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
            category.push(p.category.as_str());
            min_distance_m.push(p.min_distance_m);
        }

        let out = PyDict::new(py);
        out.set_item("ifc_id_a", PyList::new(py, &ifc_id_a)?)?;
        out.set_item("ifc_id_b", PyList::new(py, &ifc_id_b)?)?;
        out.set_item("guid_a", PyList::new(py, &guid_a)?)?;
        out.set_item("guid_b", PyList::new(py, &guid_b)?)?;
        out.set_item("class_a", PyList::new(py, &class_a)?)?;
        out.set_item("class_b", PyList::new(py, &class_b)?)?;
        out.set_item("kind", PyList::new(py, &kind)?)?;
        out.set_item("category", PyList::new(py, &category)?)?;
        out.set_item("min_distance_m", PyList::new(py, &min_distance_m)?)?;
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
        m.add("IfcfastError", _py.get_type::<IfcfastError>())?;
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
        m.add_function(wrap_pyfunction!(iter_point_cloud, m)?)?;
        #[cfg(feature = "mesh")]
        m.add_class::<PointCloudIter>()?;
        #[cfg(feature = "mesh")]
        m.add_function(wrap_pyfunction!(extract_meshes, m)?)?;
        #[cfg(feature = "mesh")]
        m.add_function(wrap_pyfunction!(write_gltf, m)?)?;
        #[cfg(feature = "bundle")]
        m.add_function(wrap_pyfunction!(bundle, m)?)?;
        #[cfg(feature = "clash")]
        m.add_function(wrap_pyfunction!(clash, m)?)?;
        m.add("__version__", env!("CARGO_PKG_VERSION"))?;
        Ok(())
    }
}
