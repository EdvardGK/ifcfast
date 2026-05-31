//! Static plan for which IFC substrate to build before IDS validation.

use super::compiled::{CompiledFacet, CompiledIds, CompiledSpec, FacetKind};
use super::extract_needs::ExtractNeeds;
use super::native_expand::collect_ids_entity_types;

/// Attributes materialised on tier-1 product/spatial columns or `attribute_value`.
const TIER1_ATTRIBUTE_NAMES: &[&str] = &[
    "Name",
    "Description",
    "Tag",
    "ObjectType",
    "PredefinedType",
    "GlobalId",
];

/// Spatial / type / group entities use the full object pool, not product columns alone.
const NON_PRODUCT_ENTITY: &[&str] = &[
    "IFCBUILDINGSTOREY",
    "IFCSPACE",
    "IFCBUILDING",
    "IFCSITE",
    "IFCPROJECT",
    "IFCTYPEOBJECT",
    "IFCGROUP",
];

#[derive(Debug, Clone, Copy, Default)]
pub struct ValidationPlan {
    pub extract: ExtractNeeds,
    /// Build [`crate::entity_table::EntityTable`] (STEP byte index).
    pub needs_entity_table: bool,
    /// Build full [`super::IfcValidationBase`] object pool.
    pub needs_full_base: bool,
    /// Validate directly on [`crate::indexer::IndexedFile`] product columns.
    pub tier1_fast_path: bool,
}

impl ValidationPlan {
    pub fn from_compiled(ids: &CompiledIds) -> Self {
        let extract = ExtractNeeds::from_compiled(ids);
        let mut needs_entity_table = extract.any();
        let mut needs_full_base = false;
        let mut tier1_fast_path = false;

        for spec in &ids.specifications {
            Self::merge_spec(spec, &mut needs_entity_table, &mut needs_full_base);
        }

        if !needs_entity_table && !needs_full_base && Self::can_tier1_fast(ids) {
            tier1_fast_path = true;
            needs_entity_table = false;
            needs_full_base = false;
        } else if !needs_entity_table && !needs_full_base {
            // Entity+attribute IDS still needs the validation object pool.
            needs_full_base = true;
        }

        Self {
            extract,
            needs_entity_table,
            needs_full_base,
            tier1_fast_path,
        }
    }

    fn merge_spec(
        spec: &CompiledSpec,
        needs_entity_table: &mut bool,
        needs_full_base: &mut bool,
    ) {
        for facet in spec
            .applicability
            .iter()
            .chain(spec.requirements.iter())
        {
            Self::merge_facet(facet, needs_entity_table, needs_full_base);
        }
    }

    fn merge_facet(
        facet: &CompiledFacet,
        needs_entity_table: &mut bool,
        needs_full_base: &mut bool,
    ) {
        match facet.kind {
            FacetKind::Property | FacetKind::Classification | FacetKind::Material => {
                *needs_entity_table = true;
                *needs_full_base = true;
            }
            FacetKind::PartOf => {
                *needs_full_base = true;
            }
            FacetKind::Entity => {
                if facet.predefined_type_constraint.is_some() || facet.predefined_type.is_some() {
                    *needs_full_base = true;
                }
                for name in &facet.entity_names {
                    if NON_PRODUCT_ENTITY
                        .iter()
                        .any(|s| name.eq_ignore_ascii_case(s))
                    {
                        *needs_full_base = true;
                    }
                }
                if facet.entity_names.is_empty() && facet.value.is_some() {
                    *needs_full_base = true;
                }
            }
            FacetKind::Attribute => {
                if !Self::attribute_is_tier1(facet) {
                    *needs_entity_table = true;
                    *needs_full_base = true;
                }
                if facet.value.is_some() && facet.attribute_name_constraint.is_some() {
                    // Pattern/restriction on attribute name — keep full path.
                    *needs_full_base = true;
                }
            }
        }
    }

    fn attribute_is_tier1(facet: &CompiledFacet) -> bool {
        let names: Vec<&str> = if !facet.attribute_names.is_empty() {
            facet.attribute_names.iter().map(|s| s.as_str()).collect()
        } else if let Some(ref n) = facet.attribute_name {
            vec![n.as_str()]
        } else {
            return false;
        };
        names
            .iter()
            .all(|n| TIER1_ATTRIBUTE_NAMES.iter().any(|t| t.eq_ignore_ascii_case(n)))
    }

    fn can_tier1_fast(ids: &CompiledIds) -> bool {
        for spec in &ids.specifications {
            for facet in spec
                .applicability
                .iter()
                .chain(spec.requirements.iter())
            {
                match facet.kind {
                    FacetKind::Entity | FacetKind::Attribute => {}
                    _ => return false,
                }
            }
            for facet in spec.applicability.iter().chain(spec.requirements.iter()) {
                if let FacetKind::Entity = facet.kind {
                    if facet.predefined_type_constraint.is_some() || facet.predefined_type.is_some()
                    {
                        return false;
                    }
                    for name in &facet.entity_names {
                        if NON_PRODUCT_ENTITY
                            .iter()
                            .any(|s| name.eq_ignore_ascii_case(s))
                        {
                            return false;
                        }
                    }
                }
                if let FacetKind::Attribute = facet.kind {
                    if !Self::attribute_is_tier1(facet) {
                        return false;
                    }
                }
            }
        }

        let types = collect_ids_entity_types(ids);
        types.is_empty() || types.iter().all(|t| {
            !NON_PRODUCT_ENTITY.iter().any(|s| s == t)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::xml::parse_ids_xml;

    #[test]
    fn bench_large_models_uses_tier1_fast() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../scripts/fixtures/bench_large_models.ids"
        );
        let xml = std::fs::read_to_string(path).expect("read ids");
        let ids = parse_ids_xml(path, &xml).expect("parse");
        let plan = ValidationPlan::from_compiled(&ids);
        assert!(!plan.extract.any());
        assert!(!plan.needs_entity_table);
        assert!(!plan.needs_full_base);
        assert!(plan.tier1_fast_path);
    }
}
