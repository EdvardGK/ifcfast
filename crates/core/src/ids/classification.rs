//! Classification facet evaluation against per-object classification rows.

use super::compiled::CompiledFacet;
use super::context::{ClsEntry, ValidationContext};
use super::facets::FacetPresence;
use super::restrictions::value_matches;

pub fn evaluate_classification(
    ctx: &ValidationContext,
    facet: &CompiledFacet,
    pix: u32,
) -> FacetPresence {
    let Some(rows) = ctx.cls_by_pix.get(&pix) else {
        return FacetPresence::Absent;
    };
    if rows.is_empty() {
        return FacetPresence::Absent;
    }

    let system_c = facet.classification_system.as_ref();
    let value_c = facet.value.as_ref();

    if system_c.is_none() && value_c.is_none() {
        return FacetPresence::Satisfied;
    }

    if rows.iter().any(|row| row_matches(row, system_c, value_c)) {
        FacetPresence::Satisfied
    } else {
        FacetPresence::Violated
    }
}

fn row_matches(
    row: &ClsEntry,
    system_c: Option<&super::compiled::ValueConstraint>,
    value_c: Option<&super::compiled::ValueConstraint>,
) -> bool {
    if let Some(sc) = system_c {
        if !value_matches(row.system_name.as_deref(), Some(sc)) {
            return false;
        }
    }
    if let Some(vc) = value_c {
        if !value_matches(row.identification.as_deref(), Some(vc)) {
            return false;
        }
    }
    true
}
