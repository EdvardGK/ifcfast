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

#[cfg(feature = "mesh")]
pub mod mesh;

#[cfg(feature = "bundle")]
pub mod bundle;

#[cfg(feature = "python")]
mod python {
    use std::fs::File;
    use std::time::Instant;

    use memmap2::Mmap;
    use pyo3::prelude::*;
    use pyo3::types::{PyDict, PyList};

    use crate::indexer;

    // ----- index_ifc ----------------------------------------------------

    #[pyfunction]
    fn index_ifc<'py>(py: Python<'py>, path: &str) -> PyResult<Bound<'py, PyDict>> {
        let t_open = Instant::now();
        let file = File::open(path)
            .map_err(|e| pyo3::exceptions::PyIOError::new_err(format!("open {path}: {e}")))?;
        let mmap = unsafe {
            Mmap::map(&file).map_err(|e| {
                pyo3::exceptions::PyIOError::new_err(format!("mmap {path}: {e}"))
            })?
        };
        let open_ms = t_open.elapsed().as_secs_f64() * 1000.0;

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

    fn open_mmap(path: &str) -> PyResult<(Mmap, f64)> {
        let t_open = Instant::now();
        let file = File::open(path)
            .map_err(|e| pyo3::exceptions::PyIOError::new_err(format!("open {path}: {e}")))?;
        let mmap = unsafe {
            Mmap::map(&file).map_err(|e| {
                pyo3::exceptions::PyIOError::new_err(format!("mmap {path}: {e}"))
            })?
        };
        Ok((mmap, t_open.elapsed().as_secs_f64() * 1000.0))
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
        let mats = py.allow_threads(|| crate::extractors::materials::build(&table, &step_to_guid));
        let mat_ms = t_mat.elapsed().as_secs_f64() * 1000.0;

        let t_marshal = Instant::now();
        let out = PyDict::new_bound(py);
        out.set_item("guid", PyList::new_bound(py, &mats.guid))?;
        out.set_item("role", PyList::new_bound(py, &mats.role))?;
        out.set_item("layer_index", PyList::new_bound(py, &mats.layer_index))?;
        out.set_item("material_name", PyList::new_bound(py, &mats.material_name))?;
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
                let t = Instant::now();
                let p = crate::extractors::psets::build(&table, &step_to_guid);
                let pt = t.elapsed().as_secs_f64() * 1000.0;
                let t = Instant::now();
                let q = crate::extractors::quantities::build(&table, &step_to_guid);
                let qt = t.elapsed().as_secs_f64() * 1000.0;
                let t = Instant::now();
                let m = crate::extractors::materials::build(&table, &step_to_guid);
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
        m.add("__version__", env!("CARGO_PKG_VERSION"))?;
        Ok(())
    }
}
