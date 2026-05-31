//! Material facet evaluation against per-object material rows.

use super::compiled::{CompiledFacet, ValueConstraint};
use super::context::{MatEntry, ValidationContext};
use super::facets::FacetPresence;
use super::restrictions::value_matches;

pub fn evaluate_material(
    ctx: &ValidationContext,
    facet: &CompiledFacet,
    pix: u32,
) -> FacetPresence {
    let Some(rows) = ctx.mat_by_pix.get(&pix) else {
        return FacetPresence::Absent;
    };
    if rows.is_empty() {
        return FacetPresence::Absent;
    }

    let value_c = material_value_constraint(facet);

    if value_c.is_none() {
        let has_name = rows.iter().any(|row| {
            row.material_name
                .as_ref()
                .is_some_and(|s| !s.is_empty())
        });
        return if has_name {
            FacetPresence::Satisfied
        } else {
            FacetPresence::Violated
        };
    }

    if rows
        .iter()
        .any(|row| material_row_matches_value(row, value_c.as_ref()))
    {
        FacetPresence::Satisfied
    } else {
        FacetPresence::Violated
    }
}

fn material_row_matches_value(row: &MatEntry, constraint: Option<&ValueConstraint>) -> bool {
    let Some(c) = constraint else {
        return true;
    };
    for candidate in [
        row.material_name.as_deref(),
        row.linked_material_name.as_deref(),
        row.category.as_deref(),
        row.linked_material_category.as_deref(),
        row.layer_set_name.as_deref(),
    ] {
        if value_matches(candidate, Some(c)) {
            return true;
        }
    }
    false
}

fn material_value_constraint(facet: &CompiledFacet) -> Option<ValueConstraint> {
    if facet.value.is_some() {
        return facet.value.clone();
    }
    facet
        .material_value
        .as_ref()
        .map(|text| ValueConstraint::Simple { text: text.clone() })
}
