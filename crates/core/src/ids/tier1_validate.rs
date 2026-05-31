//! Fast IDS validation on tier-1 product columns only (no EntityTable / object pool).

use std::collections::HashSet;

use crate::indexer::IndexedFile;

use super::compiled::{Cardinality, CompiledFacet, CompiledIds, CompiledSpec, FacetKind};
use super::entity_schema::entity_matches_names;
use super::report::{SpecResult, ValidationReport};
use super::restrictions::value_matches;

pub fn validate_tier1(indexed: &IndexedFile, ids: &CompiledIds, ifc_path: &str) -> ValidationReport {
    let mut specifications = Vec::with_capacity(ids.specifications.len());
    for spec in &ids.specifications {
        specifications.push(validate_spec_tier1(indexed, spec));
    }

    ValidationReport {
        ids_path: ids.ids_path.clone().unwrap_or_default(),
        ifc_path: ifc_path.to_string(),
        schema: indexed.schema.clone(),
        engine: "rust".into(),
        open_ms: 0.0,
        index_ms: 0.0,
        pset_extract_ms: 0.0,
        validate_ms: 0.0,
        specifications,
    }
}

fn validate_spec_tier1(indexed: &IndexedFile, spec: &CompiledSpec) -> SpecResult {
    let ifc_version_ok = schema_matches(indexed, &spec.ifc_versions);
    let applicable = applicable_product_indices(indexed, &spec.applicability);
    let applicable_count = applicable.len();

    let mut still_passing: HashSet<usize> = applicable.iter().copied().collect();
    if spec.max_occurs.allows_requirements() {
        for facet in &spec.requirements {
            if applicable.is_empty() {
                break;
            }
            let mut next = HashSet::new();
            for &ix in &applicable {
                if still_passing.contains(&ix) && requirement_passes(indexed, facet, ix) {
                    next.insert(ix);
                }
            }
            still_passing = next;
        }
    } else {
        still_passing.clear();
    }

    let failed: Vec<String> = applicable
        .iter()
        .filter(|ix| !still_passing.contains(ix))
        .map(|&ix| indexed.product_guid[ix].clone())
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
    if !failed.is_empty() {
        status = false;
    }

    SpecResult {
        name: spec.name.clone(),
        status,
        ifc_version_ok,
        applicable_count,
        passed_count: still_passing.len(),
        failed_count: failed.len(),
        failed_guids: failed,
    }
}

fn schema_matches(indexed: &IndexedFile, ifc_versions: &[String]) -> bool {
    if ifc_versions.is_empty() {
        return true;
    }
    let schema = indexed.schema.to_uppercase();
    ifc_versions.iter().any(|v| {
        let v = v.to_uppercase();
        v == schema
            || schema.starts_with(&v)
            || v.starts_with(&schema)
            || (v == "IFC4" && schema.starts_with("IFC4"))
            || (v == "IFC2X3" && schema == "IFC2X3")
    })
}

fn applicable_product_indices(indexed: &IndexedFile, facets: &[CompiledFacet]) -> Vec<usize> {
    let n = indexed.product_guid.len();
    let mut current: Vec<usize> = (0..n).collect();
    for facet in facets {
        if current.is_empty() {
            break;
        }
        if facet.kind != FacetKind::Entity {
            continue;
        }
        current.retain(|&ix| {
            let ent = &indexed.product_entity[ix];
            if !facet.entity_names.is_empty() {
                entity_matches_names(ent, &facet.entity_names, true)
            } else if let Some(ref c) = facet.value {
                value_matches(Some(ent.as_str()), Some(c))
            } else {
                true
            }
        });
    }
    current
}

fn requirement_passes(indexed: &IndexedFile, facet: &CompiledFacet, ix: usize) -> bool {
    match facet.kind {
        FacetKind::Attribute => attribute_requirement_passes(indexed, facet, ix),
        FacetKind::Entity => entity_requirement_passes(indexed, facet, ix),
        _ => false,
    }
}

fn entity_requirement_passes(indexed: &IndexedFile, facet: &CompiledFacet, ix: usize) -> bool {
    let ent = indexed.product_entity[ix].as_str();
    if !facet.entity_names.is_empty() {
        if !entity_matches_names(ent, &facet.entity_names, false) {
            return false;
        }
    } else if let Some(ref c) = facet.value {
        if !value_matches(Some(ent), Some(c)) {
            return false;
        }
    }
    if let Some(ref c) = facet.predefined_type_constraint {
        let actual = indexed
            .product_predefined_type
            .get(ix)
            .and_then(|v| v.as_deref());
        return value_matches(actual, Some(c));
    }
    true
}

fn attribute_requirement_passes(indexed: &IndexedFile, facet: &CompiledFacet, ix: usize) -> bool {
    let presence = attribute_presence(indexed, facet, ix);
    match (facet.cardinality, presence) {
        (Cardinality::Required, AttributePresence::Satisfied) => true,
        (Cardinality::Required, _) => false,
        (Cardinality::Optional, AttributePresence::Satisfied | AttributePresence::Absent) => true,
        (Cardinality::Optional, AttributePresence::Violated) => false,
        (Cardinality::Prohibited, AttributePresence::Satisfied | AttributePresence::Violated) => {
            false
        }
        (Cardinality::Prohibited, AttributePresence::Absent) => true,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AttributePresence {
    Satisfied,
    Violated,
    Absent,
}

fn attribute_presence(indexed: &IndexedFile, facet: &CompiledFacet, ix: usize) -> AttributePresence {
    let names = attribute_names(facet);
    if names.is_empty() {
        return AttributePresence::Absent;
    }
    let mut saw_value = false;
    for name in names {
        let val = product_attribute(indexed, ix, name);
        if attribute_is_absent(name, &val) {
            continue;
        }
        if attribute_is_falsey(val.as_deref()) {
            saw_value = true;
            continue;
        }
        saw_value = true;
        if facet.value.is_none() {
            return AttributePresence::Satisfied;
        }
        if value_matches(val.as_deref(), facet.value.as_ref()) {
            return AttributePresence::Satisfied;
        }
    }
    if !saw_value {
        AttributePresence::Absent
    } else {
        AttributePresence::Violated
    }
}

fn attribute_names(facet: &CompiledFacet) -> Vec<&str> {
    if !facet.attribute_names.is_empty() {
        facet.attribute_names.iter().map(|s| s.as_str()).collect()
    } else if let Some(ref n) = facet.attribute_name {
        vec![n.as_str()]
    } else {
        Vec::new()
    }
}

fn product_attribute(indexed: &IndexedFile, ix: usize, name: &str) -> Option<String> {
    match name {
        "Name" => indexed.product_name.get(ix).and_then(|v| v.clone()),
        "Description" => indexed.product_description.get(ix).and_then(|v| v.clone()),
        "Tag" => indexed.product_tag.get(ix).and_then(|v| v.clone()),
        "ObjectType" => indexed.product_object_type.get(ix).and_then(|v| v.clone()),
        "PredefinedType" => indexed
            .product_predefined_type
            .get(ix)
            .and_then(|v| v.clone()),
        "GlobalId" => Some(indexed.product_guid[ix].clone()),
        _ => None,
    }
}

fn attribute_is_absent(name: &str, val: &Option<String>) -> bool {
    if matches!(
        name,
        "Name" | "Description" | "Tag" | "ObjectType" | "PredefinedType" | "GlobalId"
    ) {
        return false;
    }
    val.is_none()
}

fn attribute_is_falsey(val: Option<&str>) -> bool {
    match val {
        None => true,
        Some(s) if s.is_empty() => true,
        Some("UNKNOWN") | Some("U") => true,
        _ => false,
    }
}
