//! IDS validation engine on IndexedFile + PsetTable.

use std::time::Instant;

use crate::extractors::classifications::ClassificationTable;
use crate::extractors::materials::MaterialTable;
use crate::extractors::psets::PsetTable;
use crate::extractors::quantities::QuantityTable;
use crate::indexer::IndexedFile;

use super::compiled::{CompiledIds, CompiledSpec};
use super::context::ValidationContext;
use crate::entity_table::EntityTable;
use super::facets::facet_mask;
use super::report::{SpecResult, ValidationReport};

pub fn validate(
    indexed: &IndexedFile,
    psets: &PsetTable,
    quantities: &QuantityTable,
    classifications: &ClassificationTable,
    materials: &MaterialTable,
    ids: &CompiledIds,
    table: &EntityTable,
    ifc_path: &str,
) -> ValidationReport {
    let t0 = Instant::now();
    let length_unit_scale =
        crate::indexer::extract_unit_scale(table).unwrap_or(1.0);
    let ctx = ValidationContext::build(
        indexed,
        psets,
        quantities,
        classifications,
        materials,
        ids,
        table,
        length_unit_scale,
    );
    let mut specifications = Vec::with_capacity(ids.specifications.len());

    for spec in &ids.specifications {
        specifications.push(validate_spec(&ctx, spec));
    }

    let validate_ms = t0.elapsed().as_secs_f64() * 1000.0;

    ValidationReport {
        ids_path: ids.ids_path.clone().unwrap_or_default(),
        ifc_path: ifc_path.to_string(),
        schema: ctx.schema.clone(),
        engine: "rust".into(),
        open_ms: 0.0,
        index_ms: 0.0,
        pset_extract_ms: 0.0,
        validate_ms,
        specifications,
    }
}

pub(crate) fn validate_spec(ctx: &ValidationContext, spec: &CompiledSpec) -> SpecResult {
    let ifc_version_ok = spec.ifc_versions.is_empty()
        || ctx.schema_matches_spec(&spec.ifc_versions);

    let n = ctx.object_count();
    let mut applicable: Vec<u32> = (0..n as u32).collect();

    for facet in &spec.applicability {
        if applicable.is_empty() {
            break;
        }
        let mask = facet_mask(ctx, facet, &applicable, true);
        applicable = applicable
            .into_iter()
            .zip(mask)
            .filter_map(|(pix, ok)| if ok { Some(pix) } else { None })
            .collect();
    }

    let mut still_passing: std::collections::HashSet<u32> =
        applicable.iter().copied().collect();
    let allows_req = spec.max_occurs.allows_requirements();

    if allows_req {
        for facet in &spec.requirements {
            if applicable.is_empty() {
                break;
            }
            let mask = facet_mask(ctx, facet, &applicable, false);
            let mut next = std::collections::HashSet::new();
            for (&pix, ok) in applicable.iter().zip(mask) {
                if still_passing.contains(&pix) && ok {
                    next.insert(pix);
                }
            }
            still_passing = next;
        }
    } else {
        still_passing.clear();
    }

    let passed_entities = still_passing;
    let failed_entities: std::collections::HashSet<u32> = applicable
        .iter()
        .copied()
        .filter(|pix| !passed_entities.contains(pix))
        .collect();

    let mut status = true;
    if spec.min_occurs > 0 && applicable.is_empty() {
        status = false;
    }
    if spec.max_occurs.is_prohibited() {
        if !applicable.is_empty() && spec.requirements.is_empty() {
            status = false;
        }
    }
    if !failed_entities.is_empty() {
        status = false;
    }

    let failed_guids: Vec<String> = failed_entities
        .iter()
        .map(|&pix| ctx.object_guid[pix as usize].clone())
        .collect();

    SpecResult {
        name: spec.name.clone(),
        status,
        ifc_version_ok,
        applicable_count: applicable.len(),
        passed_count: passed_entities.len(),
        failed_count: failed_entities.len(),
        failed_guids,
    }
}
