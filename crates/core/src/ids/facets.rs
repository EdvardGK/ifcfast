//! Facet evaluators over product index masks (IDS 1.0 / IfcTester semantics).

use super::compiled::{Cardinality, CompiledFacet, FacetKind};
use super::context::ValidationContext;
use super::entity_schema::entity_matches_names;
use super::predefined_type::{predefined_type_matches, resolved_predefined_type};
use super::restrictions::{ids_datatype_matches, value_matches};

/// Raw facet evaluation before cardinality (IDS requirement interpretation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FacetPresence {
    /// Constraint satisfied (property present when required, value matches, etc.).
    Satisfied,
    /// Constraint not satisfied (wrong value, wrong type, etc.).
    Violated,
    /// No matching data (e.g. property row missing) — used for optional requirements.
    Absent,
}

pub fn facet_mask(
    ctx: &ValidationContext,
    facet: &CompiledFacet,
    applicable: &[u32],
    applicability: bool,
) -> Vec<bool> {
    applicable
        .iter()
        .map(|&pix| facet_passes(ctx, facet, pix, applicability))
        .collect()
}

fn facet_passes(
    ctx: &ValidationContext,
    facet: &CompiledFacet,
    pix: u32,
    applicability: bool,
) -> bool {
    let presence = match facet.kind {
        FacetKind::Entity => evaluate_entity(ctx, facet, pix, applicability),
        FacetKind::Attribute => evaluate_attribute(ctx, facet, pix),
        FacetKind::Property => evaluate_property(ctx, facet, pix),
        FacetKind::Classification => super::classification::evaluate_classification(ctx, facet, pix),
        FacetKind::Material => super::material::evaluate_material(ctx, facet, pix),
        FacetKind::PartOf => evaluate_partof(ctx, facet, pix),
    };
    apply_cardinality(presence, facet.cardinality)
}

/// IDS cardinality on requirement facets (buildingSMART IDS 1.0 user manual).
fn apply_cardinality(presence: FacetPresence, cardinality: Cardinality) -> bool {
    match (cardinality, presence) {
        (Cardinality::Required, FacetPresence::Satisfied) => true,
        (Cardinality::Required, FacetPresence::Absent | FacetPresence::Violated) => false,
        (Cardinality::Optional, FacetPresence::Satisfied | FacetPresence::Absent) => true,
        (Cardinality::Optional, FacetPresence::Violated) => false,
        (Cardinality::Prohibited, FacetPresence::Satisfied | FacetPresence::Violated) => false,
        (Cardinality::Prohibited, FacetPresence::Absent) => true,
    }
}

fn evaluate_entity(
    ctx: &ValidationContext,
    facet: &CompiledFacet,
    pix: u32,
    applicability: bool,
) -> FacetPresence {
    let ent = &ctx.object_entity_upper[pix as usize];
    let allow_subtypes = applicability;
    if !facet.entity_names.is_empty() {
        if !entity_matches_names(ent, &facet.entity_names, allow_subtypes) {
            return FacetPresence::Violated;
        }
    } else if let Some(ref constraint) = facet.value {
        if !value_matches(Some(ent.as_str()), Some(constraint)) {
            return FacetPresence::Violated;
        }
    }
    if let Some(ref c) = facet.predefined_type_constraint {
        let actual = resolved_predefined_type(ctx, pix);
        return if value_matches(actual.as_deref(), Some(c)) {
            FacetPresence::Satisfied
        } else {
            FacetPresence::Violated
        };
    }
    if let Some(ref pt) = facet.predefined_type {
        return if predefined_type_matches(ctx, pix, pt) {
            FacetPresence::Satisfied
        } else {
            FacetPresence::Violated
        };
    }
    if facet.entity_names.is_empty() && facet.predefined_type.is_none() && facet.value.is_none() {
        FacetPresence::Violated
    } else {
        FacetPresence::Satisfied
    }
}

fn attribute_names_for_facet(ctx: &ValidationContext, facet: &CompiledFacet, pix: u32) -> Vec<String> {
    let ent = &ctx.object_entity_upper[pix as usize];
    super::attribute_read::matching_attribute_names(
        &ctx.schema,
        ent,
        facet.attribute_name.as_deref(),
        &facet.attribute_names,
        facet.attribute_name_constraint.as_ref(),
    )
}

fn evaluate_attribute(ctx: &ValidationContext, facet: &CompiledFacet, pix: u32) -> FacetPresence {
    let names = attribute_names_for_facet(ctx, facet, pix);
    if names.is_empty() {
        return FacetPresence::Absent;
    }

    let mut saw_value = false;
    for name in &names {
        let val = attribute_value(ctx, pix, name);
        if attribute_is_absent(name, &val) {
            continue;
        }
        if attribute_is_falsey(val.as_deref()) {
            saw_value = true;
            continue;
        }
        saw_value = true;
        if facet.value.is_none() {
            return FacetPresence::Satisfied;
        }
        if value_matches(val.as_deref(), facet.value.as_ref()) {
            return FacetPresence::Satisfied;
        }
    }

    if !saw_value {
        FacetPresence::Absent
    } else {
        FacetPresence::Violated
    }
}

/// Attribute not materialised in the validation context (IfcTester: NOVALUE / unsupported).
fn attribute_is_absent(name: &str, val: &Option<String>) -> bool {
    if matches!(
        name,
        "Name" | "Description" | "Tag" | "ObjectType" | "PredefinedType" | "GlobalId"
    ) {
        return false;
    }
    val.is_none()
}

/// Present but empty: null string, UNKNOWN logical, empty tuple rendered as empty (IfcTester FALSEY).
fn attribute_is_falsey(val: Option<&str>) -> bool {
    match val {
        None => true,
        Some(s) if s.is_empty() => true,
        Some("UNKNOWN") | Some("U") => true,
        _ => false,
    }
}

fn attribute_value(ctx: &ValidationContext, pix: u32, name: &str) -> Option<String> {
    let guid = &ctx.object_guid[pix as usize];
    if let Some(m) = ctx.attrs_by_guid.get(guid) {
        if let Some(v) = m.get(name) {
            return Some(v.clone());
        }
    }
    match name {
        "Name" => ctx.object_name[pix as usize].clone(),
        "Description" => ctx.object_description[pix as usize].clone(),
        "Tag" => ctx.object_tag[pix as usize].clone(),
        "ObjectType" => ctx.object_object_type[pix as usize].clone(),
        "PredefinedType" => ctx.object_predefined_type[pix as usize].clone(),
        "GlobalId" => Some(ctx.object_guid[pix as usize].clone()),
        _ => None,
    }
}

fn property_value_usable(value: Option<&str>) -> bool {
    match value {
        None => false,
        Some("") => false,
        Some("UNKNOWN") => false,
        _ => true,
    }
}

fn property_entry_has_value(entry: &super::context::PropEntry) -> bool {
    property_value_usable(entry.value.as_deref())
        || property_value_usable(entry.value_defined.as_deref())
}

fn property_datatype_matches(entry: &super::context::PropEntry, data_type: &str) -> bool {
    ids_datatype_matches(entry.value_type.as_deref(), data_type)
        || ids_datatype_matches(entry.value_type_defined.as_deref(), data_type)
}

fn property_pset_matches(pset: &str, facet: &CompiledFacet) -> bool {
    if let Some(ref c) = facet.property_set_constraint {
        return value_matches(Some(pset), Some(c));
    }
    let pset_names: Vec<&str> = if !facet.property_sets.is_empty() {
        facet.property_sets.iter().map(|s| s.as_str()).collect()
    } else if let Some(ref ps) = facet.property_set {
        vec![ps.as_str()]
    } else {
        vec![]
    };
    if pset_names.is_empty() {
        return true;
    }
    pset_names.iter().any(|n| *n == pset)
}

fn property_prop_name_matches(prop: &str, facet: &CompiledFacet) -> bool {
    if let Some(ref c) = facet.base_name_constraint {
        return value_matches(Some(prop), Some(c));
    }
    let prop_names: Vec<&str> = if !facet.base_names.is_empty() {
        facet.base_names.iter().map(|s| s.as_str()).collect()
    } else if let Some(ref bn) = facet.base_name {
        vec![bn.as_str()]
    } else {
        vec![]
    };
    if prop_names.is_empty() {
        return true;
    }
    prop_names.iter().any(|n| *n == prop)
}

fn evaluate_property(ctx: &ValidationContext, facet: &CompiledFacet, pix: u32) -> FacetPresence {
    use std::collections::{HashMap, HashSet};

    // IfcTester: optional property requirements always pass.
    if facet.cardinality == Cardinality::Optional {
        return FacetPresence::Satisfied;
    }

    let requires_named_prop = facet.base_name_constraint.is_some()
        || !facet.base_names.is_empty()
        || facet.base_name.is_some();

    let mut psets: HashSet<String> = HashSet::new();
    let mut rows_by_pset: HashMap<String, Vec<&super::context::PropEntry>> = HashMap::new();

    for ((row_pix, pset, prop), entry) in ctx.prop_lookup.iter() {
        if *row_pix != pix {
            continue;
        }
        if !property_pset_matches(pset, facet) {
            continue;
        }
        psets.insert(pset.clone());
        if !requires_named_prop || property_prop_name_matches(prop, facet) {
            rows_by_pset.entry(pset.clone()).or_default().push(entry);
        }
    }

    if psets.is_empty() {
        return FacetPresence::Absent;
    }

    for pset in &psets {
        let rows = rows_by_pset.get(pset).map(|v| v.as_slice()).unwrap_or(&[]);
        if requires_named_prop && rows.is_empty() {
            return FacetPresence::Violated;
        }
        for entry in rows {
            if !property_entry_has_value(entry) {
                return FacetPresence::Violated;
            }
            if let Some(ref dt) = facet.data_type {
                if !property_datatype_matches(entry, dt) {
                    return FacetPresence::Violated;
                }
            }
            if let Some(ref vc) = facet.value {
                if !property_value_matches(ctx, entry, vc, facet.data_type.as_deref()) {
                    return FacetPresence::Violated;
                }
            }
        }
    }

    FacetPresence::Satisfied
}

fn evaluate_partof(ctx: &ValidationContext, facet: &CompiledFacet, pix: u32) -> FacetPresence {
    let step_id = ctx.object_step_id[pix as usize];
    let rel = normalize_relation(facet.partof_relation.as_deref());

    let container_steps: Vec<u64> = match rel.as_str() {
        "IFCRELAGGREGATES" => aggregate_ancestor_steps(ctx.indexed, step_id),
        "IFCRELCONTAINEDINSPATIALSTRUCTURE" => container_steps(ctx.indexed, step_id),
        "IFCRELNESTS" => nest_ancestor_steps(ctx.indexed, step_id),
        "IFCRELASSIGNSTOGROUP" => group_parent_steps(ctx.indexed, step_id),
        "IFCRELVOIDSELEMENT IFCRELFILLSELEMENT" => void_host_steps(ctx.indexed, step_id),
        "" => {
            let mut s = aggregate_ancestor_steps(ctx.indexed, step_id);
            s.extend(nest_ancestor_steps(ctx.indexed, step_id));
            s.extend(group_parent_steps(ctx.indexed, step_id));
            s.extend(container_steps(ctx.indexed, step_id));
            s
        }
        _ => Vec::new(),
    };

    if container_steps.is_empty() {
        return FacetPresence::Absent;
    }

    let need_container_match =
        !facet.entity_names.is_empty() || facet.predefined_type.is_some();

    if !need_container_match {
        return FacetPresence::Satisfied;
    }

    for anc in container_steps {
        if let Some(apix) = pix_for_step(ctx, anc) {
            if object_matches_partof_container(ctx, apix, facet) {
                return FacetPresence::Satisfied;
            }
        }
    }
    FacetPresence::Violated
}

fn normalize_relation(rel: Option<&str>) -> String {
    rel.unwrap_or("")
        .to_uppercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn pix_for_step(ctx: &ValidationContext, step: u64) -> Option<u32> {
    ctx.object_step_id
        .iter()
        .position(|&s| s == step)
        .map(|i| i as u32)
}

fn object_matches_partof_container(ctx: &ValidationContext, pix: u32, facet: &CompiledFacet) -> bool {
    let ent = &ctx.object_entity_upper[pix as usize];
    if !facet.entity_names.is_empty() {
        if !entity_matches_names(ent, &facet.entity_names, false) {
            return false;
        }
    } else if let Some(ref constraint) = facet.value {
        if !value_matches(Some(ent.as_str()), Some(constraint)) {
            return false;
        }
    }
    if let Some(ref c) = facet.predefined_type_constraint {
        let actual = resolved_predefined_type(ctx, pix);
        if !value_matches(actual.as_deref(), Some(c)) {
            return false;
        }
    } else if let Some(ref pt) = facet.predefined_type {
        if !predefined_type_matches(ctx, pix, pt) {
            return false;
        }
    }
    true
}

/// IfcTester property value check: list/enumerated/bounded/table members, floats via `is_x`.
fn property_value_matches(
    ctx: &ValidationContext,
    entry: &super::context::PropEntry,
    constraint: &super::compiled::ValueConstraint,
    data_type: Option<&str>,
) -> bool {
    for actual in property_value_strings(entry, data_type) {
        if let Some(members) = split_property_members(&actual) {
            if members
                .iter()
                .any(|m| property_scalar_matches(ctx, m, constraint, data_type))
            {
                return true;
            }
        } else if property_scalar_matches(ctx, &actual, constraint, data_type) {
            return true;
        }
    }
    false
}

/// Values from defining / defined table columns when the IDS `dataType` matches (IfcTester).
fn property_value_strings(entry: &super::context::PropEntry, data_type: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    let dt = data_type.map(|s| s.to_ascii_uppercase());
    let primary_ok = match (&entry.value_type, &dt) {
        (Some(t), Some(d)) => ids_datatype_matches(Some(t), d),
        _ => entry.value.is_some(),
    };
    if primary_ok {
        if let Some(v) = &entry.value {
            out.push(v.clone());
        }
    }
    let defined_ok = match (&entry.value_type_defined, &dt) {
        (Some(t), Some(d)) => ids_datatype_matches(Some(t), d),
        _ => false,
    };
    if defined_ok {
        if let Some(v) = &entry.value_defined {
            out.push(v.clone());
        }
    }
    out
}

fn split_property_members(value: &str) -> Option<Vec<String>> {
    if value.contains("..") || value.contains('@') {
        let mut parts = Vec::new();
        if let Some((base, rest)) = value.split_once('@') {
            let sp = rest.trim();
            if !sp.is_empty() {
                parts.push(sp.to_string());
            }
            let _ = base;
        }
        let base = value.split('@').next().unwrap_or(value);
        for segment in base.split("..") {
            let s = segment.trim();
            if !s.is_empty() {
                parts.push(s.to_string());
            }
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts)
        }
    } else if value.contains(", ") {
        Some(
            value
                .split(", ")
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
        )
    } else {
        None
    }
}

fn property_scalar_matches(
    ctx: &ValidationContext,
    actual: &str,
    constraint: &super::compiled::ValueConstraint,
    data_type: Option<&str>,
) -> bool {
    if matches!(
        data_type,
        Some(dt) if dt.eq_ignore_ascii_case("IFCLENGTHMEASURE")
    ) {
        if let (Ok(n), super::compiled::ValueConstraint::Simple { text }) =
            (actual.parse::<f64>(), constraint)
        {
            if let Ok(req) = text.parse::<f64>() {
                let si = n * ctx.length_unit_scale;
                return super::restrictions::float_eq_ifctester(si, req);
            }
        }
    }
    value_matches(Some(actual), Some(constraint))
}

fn aggregate_ancestor_steps(indexed: &crate::indexer::IndexedFile, child: u64) -> Vec<u64> {
    use std::collections::HashSet;
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for (c, a) in indexed
        .aggregates_transitive_child
        .iter()
        .zip(indexed.aggregates_transitive_ancestor.iter())
    {
        if *c == child && seen.insert(*a) {
            out.push(*a);
        }
    }
    for (c, p) in indexed
        .aggregates_child
        .iter()
        .zip(indexed.aggregates_parent.iter())
    {
        if *c == child && seen.insert(*p) {
            out.push(*p);
        }
    }
    out
}

fn nest_ancestor_steps(indexed: &crate::indexer::IndexedFile, child: u64) -> Vec<u64> {
    use std::collections::{HashMap, HashSet};
    let mut parent_of: HashMap<u64, u64> = HashMap::new();
    for (c, p) in indexed.nests_child.iter().zip(indexed.nests_parent.iter()) {
        parent_of.insert(*c, *p);
    }
    let mut out = Vec::new();
    let mut cur = child;
    let mut visited = HashSet::new();
    while let Some(&parent) = parent_of.get(&cur) {
        if !visited.insert(parent) {
            break;
        }
        out.push(parent);
        cur = parent;
    }
    out
}

fn group_parent_steps(indexed: &crate::indexer::IndexedFile, child: u64) -> Vec<u64> {
    indexed
        .groups_child
        .iter()
        .zip(indexed.groups_parent.iter())
        .filter_map(|(&c, &p)| if c == child { Some(p) } else { None })
        .collect()
}

fn container_steps(indexed: &crate::indexer::IndexedFile, child: u64) -> Vec<u64> {
    let mut out = Vec::new();
    for (c, s) in indexed
        .contained_in_child
        .iter()
        .zip(indexed.contained_in_structure.iter())
    {
        if *c == child {
            out.push(*s);
        }
    }
    for (c, s) in indexed
        .contained_in_space_child
        .iter()
        .zip(indexed.contained_in_space_space.iter())
    {
        if *c == child {
            out.push(*s);
        }
    }
    out
}

fn void_host_steps(indexed: &crate::indexer::IndexedFile, step_id: u64) -> Vec<u64> {
    for (opening, host) in indexed
        .voids_opening
        .iter()
        .zip(indexed.voids_host.iter())
    {
        if *opening == step_id {
            return vec![*host];
        }
        if *host == step_id {
            return vec![*host];
        }
    }
    Vec::new()
}
