//! Per-IFC IDS validation session: prepare once, validate many.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use crate::entity_table::EntityTable;
use crate::extractors::classifications::ClassificationTable;
use crate::extractors::materials::MaterialTable;
use crate::extractors::psets::PsetTable;
use crate::extractors::quantities::QuantityTable;
use crate::ids::ExtractNeeds;
use crate::ids::{
    CompiledIds, IfcValidationBase, ValidationContext, ValidationPlan, ValidationReport,
    validate_spec,
};
use crate::ids::validate_tier1;
use crate::indexer::IndexedFile;
use crate::object_guid;
use crate::scan;
use crate::source::IfcSource;

pub struct IdsSession {
    pub ifc_path: String,
    pub source: Arc<IfcSource>,
    pub indexed: IndexedFile,
    pub table: Option<EntityTable>,
    pub psets: PsetTable,
    pub quantities: QuantityTable,
    pub classifications: ClassificationTable,
    pub materials: MaterialTable,
    pub base: Option<IfcValidationBase>,
    pub plan: ValidationPlan,
    pub open_ms: f64,
    pub scan_ms: f64,
    pub index_ms: f64,
    pub table_ms: f64,
    pub extract_ms: f64,
    pub object_map_ms: f64,
    pub base_ms: f64,
}

fn empty_psets() -> PsetTable {
    PsetTable {
        guid: Vec::new(),
        pset_name: Vec::new(),
        prop_name: Vec::new(),
        value: Vec::new(),
        value_type: Vec::new(),
        value_defined: Vec::new(),
        value_type_defined: Vec::new(),
    }
}

fn empty_quantities() -> QuantityTable {
    QuantityTable {
        guid: Vec::new(),
        qto_name: Vec::new(),
        quantity_name: Vec::new(),
        value: Vec::new(),
        quantity_type: Vec::new(),
        unit_step_id: Vec::new(),
    }
}

fn empty_classifications() -> ClassificationTable {
    ClassificationTable {
        guid: Vec::new(),
        system_name: Vec::new(),
        edition: Vec::new(),
        identification: Vec::new(),
        name: Vec::new(),
        location: Vec::new(),
        source: Vec::new(),
    }
}

fn empty_materials() -> MaterialTable {
    MaterialTable::default()
}

impl IdsSession {
    /// Full prepare (all extractors). Prefer [`Self::prepare_for_plan`] when the IDS is known.
    pub fn prepare(ifc_path: &str) -> Result<Self, String> {
        Self::prepare_for(ifc_path, ExtractNeeds::all())
    }

    /// Prepare with facet-scoped extractors (faster cold start when IDS is known upfront).
    pub fn prepare_for(ifc_path: &str, needs: ExtractNeeds) -> Result<Self, String> {
        let plan = ValidationPlan {
            extract: needs,
            needs_entity_table: needs.any(),
            needs_full_base: true,
            tier1_fast_path: false,
        };
        Self::prepare_for_plan(ifc_path, plan)
    }

    /// Prepare using a [`ValidationPlan`] derived from compiled IDS.
    pub fn prepare_for_compiled(ifc_path: &str, compiled: &CompiledIds) -> Result<Self, String> {
        Self::prepare_for_plan(ifc_path, ValidationPlan::from_compiled(compiled))
    }

    /// Prepare from an already-open IFC (reuses mmap; optional pre-built index).
    pub fn prepare_from_source(
        ifc_path: &str,
        source: Arc<IfcSource>,
        indexed: Option<IndexedFile>,
        plan: ValidationPlan,
    ) -> Result<Self, String> {
        if source.is_empty() {
            return Err(format!("empty IFC: {ifc_path}"));
        }
        Self::prepare_inner(ifc_path, source, indexed, plan, 0.0)
    }

    fn prepare_for_plan(ifc_path: &str, plan: ValidationPlan) -> Result<Self, String> {
        let path = Path::new(ifc_path);
        let t_open = Instant::now();
        let source = Arc::new(
            crate::source::open(path).map_err(|e| format!("open {ifc_path}: {e}"))?,
        );
        let open_ms = t_open.elapsed().as_secs_f64() * 1000.0;
        Self::prepare_inner(ifc_path, source, None, plan, open_ms)
    }

    fn prepare_inner(
        ifc_path: &str,
        source: Arc<IfcSource>,
        indexed: Option<IndexedFile>,
        plan: ValidationPlan,
        open_ms: f64,
    ) -> Result<Self, String> {
        let needs = plan.extract;
        let build_table = plan.needs_entity_table;

        let (indexed, mut table, scan_ms, index_ms, table_ms) = if let Some(idx) = indexed {
            let (tbl, ms) = if build_table {
                let t0 = Instant::now();
                let tbl = Some(EntityTable::build(Arc::clone(&source)));
                (tbl, t0.elapsed().as_secs_f64() * 1000.0)
            } else {
                (None, 0.0)
            };
            (idx, tbl, ms, 0.0, ms)
        } else {
            let profile = if plan.tier1_fast_path {
                crate::indexer::IndexProfile::Tier1Validate
            } else {
                crate::indexer::IndexProfile::Full
            };
            let scan = scan::scan_ifc(Arc::clone(&source), build_table, profile);
            let table_ms = if build_table { scan.scan_ms } else { 0.0 };
            (
                scan.indexed,
                scan.table,
                scan.scan_ms,
                scan.scan_ms,
                table_ms,
            )
        };

        if plan.tier1_fast_path {
            return Ok(Self {
                ifc_path: ifc_path.to_string(),
                source,
                indexed,
                table: None,
                psets: empty_psets(),
                quantities: empty_quantities(),
                classifications: empty_classifications(),
                materials: empty_materials(),
                base: None,
                plan,
                open_ms,
                scan_ms,
                index_ms,
                table_ms,
                extract_ms: 0.0,
                object_map_ms: 0.0,
                base_ms: 0.0,
            });
        }

        if table.is_none() {
            let t0 = Instant::now();
            table = Some(EntityTable::build(Arc::clone(&source)));
            let built_ms = t0.elapsed().as_secs_f64() * 1000.0;
            let _ = built_ms;
        }
        let table_ref = table.as_ref().expect("entity table");
        let t_map = Instant::now();
        let scan_materials = needs.any();
        let object_step_to_guid =
            object_guid::build_extractor_object_map(&indexed, table_ref, scan_materials);
        let object_map_ms = t_map.elapsed().as_secs_f64() * 1000.0;

        let unit_scale = crate::indexer::extract_unit_scale(table_ref)
            .or(indexed.unit_scale)
            .unwrap_or(1.0);

        let t_extract = Instant::now();
        let (psets, quantities, classifications, materials) =
            extract_tables(&needs, table_ref, &object_step_to_guid, &indexed, unit_scale);
        let extract_ms = t_extract.elapsed().as_secs_f64() * 1000.0;

        let t_base = Instant::now();
        let base = IfcValidationBase::build(
            &indexed,
            &psets,
            &quantities,
            &classifications,
            &materials,
            unit_scale,
        );
        let base_ms = t_base.elapsed().as_secs_f64() * 1000.0;

        let table_owned = table.expect("entity table built");
        Ok(Self {
            ifc_path: ifc_path.to_string(),
            source,
            indexed,
            table: Some(table_owned),
            psets,
            quantities,
            classifications,
            materials,
            base: Some(base),
            plan,
            open_ms,
            scan_ms,
            index_ms,
            table_ms,
            extract_ms,
            object_map_ms,
            base_ms,
        })
    }

    pub fn prepare_ms(&self) -> f64 {
        self.open_ms
            + self.scan_ms
            + self.extract_ms
            + self.object_map_ms
            + self.base_ms
    }

    /// Prepare IFC substrate and run one validation (bench cold path).
    pub fn prepare_and_validate(
        ifc_path: &str,
        compiled: &CompiledIds,
    ) -> Result<(Self, ValidationReport), String> {
        let session = Self::prepare_for_compiled(ifc_path, compiled)?;
        let report = session.validate(compiled);
        Ok((session, report))
    }

    pub fn validate(&self, compiled: &CompiledIds) -> ValidationReport {
        let t0 = Instant::now();

        let mut report = if self.plan.tier1_fast_path {
            validate_tier1(&self.indexed, compiled, &self.ifc_path)
        } else {
            let table = self.table.as_ref().expect("table for full validation");
            let base = self
                .base
                .as_ref()
                .expect("validation base for full validation");
            let ctx = ValidationContext::from_base(
                &self.indexed,
                &self.psets,
                compiled,
                table,
                base,
            );

            let mut specifications = Vec::with_capacity(compiled.specifications.len());
            for spec in &compiled.specifications {
                specifications.push(validate_spec(&ctx, spec));
            }

            ValidationReport {
                ids_path: compiled.ids_path.clone().unwrap_or_default(),
                ifc_path: self.ifc_path.clone(),
                schema: ctx.schema.clone(),
                engine: "rust".into(),
                open_ms: 0.0,
                index_ms: 0.0,
                pset_extract_ms: 0.0,
                validate_ms: 0.0,
                specifications,
            }
        };

        report.validate_ms = t0.elapsed().as_secs_f64() * 1000.0;
        report
    }
}

fn extract_tables(
    needs: &ExtractNeeds,
    table: &EntityTable,
    object_step_to_guid: &std::collections::HashMap<u64, String>,
    indexed: &IndexedFile,
    unit_scale: f64,
) -> (PsetTable, QuantityTable, ClassificationTable, MaterialTable) {
    if !needs.any() {
        return (
            empty_psets(),
            empty_quantities(),
            empty_classifications(),
            empty_materials(),
        );
    }

    let run_psets = needs.psets;
    let run_quantities = needs.quantities;
    let run_classifications = needs.classifications;
    let run_materials = needs.materials;

    std::thread::scope(|scope| {
        let table_p = table;
        let map = object_step_to_guid;
        let idx = indexed;

        let h_psets = run_psets.then(|| {
            let t = table_p;
            let m = map;
            scope.spawn(move || crate::extractors::psets::build(t, m))
        });

        let h_quantities = run_quantities.then(|| {
            let t = table_p;
            let m = map;
            scope.spawn(move || crate::extractors::quantities::build(t, m))
        });

        let h_classifications = run_classifications.then(|| {
            let t = table_p;
            let m = map;
            let i = idx;
            scope.spawn(move || {
                let mut cls = crate::extractors::classifications::build(t, m);
                crate::extractors::classifications::expand_for_ids(t, &mut cls, i);
                cls
            })
        });

        let h_materials = run_materials.then(|| {
            let t = table_p;
            let m = map;
            let scale = unit_scale;
            scope.spawn(move || crate::extractors::materials::build(t, m, scale))
        });

        let psets = h_psets
            .map(|h| h.join().expect("psets"))
            .unwrap_or_else(empty_psets);
        let quantities = h_quantities
            .map(|h| h.join().expect("quantities"))
            .unwrap_or_else(empty_quantities);
        let classifications = h_classifications
            .map(|h| h.join().expect("classifications"))
            .unwrap_or_else(empty_classifications);
        let materials = h_materials
            .map(|h| h.join().expect("materials"))
            .unwrap_or_else(empty_materials);

        (psets, quantities, classifications, materials)
    })
}
