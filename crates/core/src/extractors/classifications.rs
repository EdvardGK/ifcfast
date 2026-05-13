//! Classification reference extraction.
//!
//! Walks `IfcRelAssociatesClassification` → `IfcClassificationReference`
//! → `IfcClassification`. Emits one row per (product, classification ref):
//!
//! ```text
//! (guid, system_name, edition, identification, name, location, source)
//! ```
//!
//! Where:
//!   `system_name`     — `IfcClassification.Name` (e.g. "NS 3451", "Uniformat II", "OmniClass")
//!   `edition`         — `IfcClassification.Edition` (e.g. "2022")
//!   `identification`  — `IfcClassificationReference.Identification` (e.g. "232.1")
//!   `name`            — `IfcClassificationReference.Name` (human label, e.g. "Yttervegger")
//!   `location`        — `IfcClassificationReference.Location` (URI to spec, often null)
//!   `source`          — `IfcClassification.Source` (publisher / standards body)
//!
//! Critical for Norwegian projects (NS 3451 → 4-digit / 6-digit building part
//! codes) and Building Smart workflows (OmniClass + Uniformat tables).
//!
//! Phase 1: IfcClassificationReference only. IfcClassificationNotation /
//! IfcClassificationNotationFacet (IFC2X3 legacy) deferred — rare on modern
//! exports.

use std::collections::HashMap;

use crate::entity_table::EntityTable;
use crate::lexer::{parse_field, split_top_level_args, Field};

#[derive(Debug, Default)]
pub struct ClassificationTable {
    pub guid: Vec<String>,
    pub system_name: Vec<Option<String>>,
    pub edition: Vec<Option<String>>,
    pub identification: Vec<Option<String>>,
    pub name: Vec<Option<String>>,
    pub location: Vec<Option<String>>,
    pub source: Vec<Option<String>>,
}

impl ClassificationTable {
    pub fn len(&self) -> usize {
        self.guid.len()
    }
}

pub fn build(
    table: &EntityTable,
    product_step_to_guid: &HashMap<u64, String>,
) -> ClassificationTable {
    // Pass 1: collect classification records.
    // - IfcClassification (id → metadata: source, edition, name)
    // - IfcClassificationReference (id → ref details + parent system id)
    let mut systems: HashMap<u64, SystemRecord> = HashMap::with_capacity(64);
    let mut refs: HashMap<u64, RefRecord> = HashMap::with_capacity(1024);
    let mut rel_pairs: Vec<(u64, u64)> = Vec::with_capacity(4096);

    for (step_id, type_name, args) in table.iter() {
        if type_name.eq_ignore_ascii_case(b"IFCCLASSIFICATION") {
            // IFC2X3: (Source, Edition, EditionDate, Name)
            // IFC4:   (Source, Edition, EditionDate, Name, Description, Location, ReferenceTokens)
            let fields = split_top_level_args(args);
            systems.insert(
                step_id,
                SystemRecord {
                    source: string_at(&fields, 0),
                    edition: string_at(&fields, 1),
                    name: string_at(&fields, 3),
                },
            );
        } else if type_name.eq_ignore_ascii_case(b"IFCCLASSIFICATIONREFERENCE") {
            // IFC2X3: (Location, ItemReference, Name, ReferencedSource)
            // IFC4:   (Location, Identification, Name, ReferencedSource, Description, Sort)
            let fields = split_top_level_args(args);
            let location = string_at(&fields, 0);
            let identification = string_at(&fields, 1);
            let name = string_at(&fields, 2);
            let parent_id = match fields.get(3).copied().map(parse_field) {
                Some(Field::Ref(id)) => Some(id),
                _ => None,
            };
            refs.insert(
                step_id,
                RefRecord {
                    location,
                    identification,
                    name,
                    parent_id,
                },
            );
        } else if type_name.eq_ignore_ascii_case(b"IFCRELASSOCIATESCLASSIFICATION") {
            // (GlobalId, OwnerHistory, Name, Description, RelatedObjects, RelatingClassification)
            let fields = split_top_level_args(args);
            let relating = match fields.get(5).copied().map(parse_field) {
                Some(Field::Ref(id)) => id,
                _ => continue,
            };
            let relateds = match fields.get(4).copied().map(parse_field) {
                Some(Field::List(body)) => parse_ref_list(body),
                Some(Field::Ref(id)) => vec![id],
                _ => continue,
            };
            for obj_id in relateds {
                rel_pairs.push((obj_id, relating));
            }
        }
    }

    let mut out = ClassificationTable::default();
    for (obj_step_id, relating_id) in rel_pairs {
        let guid = match product_step_to_guid.get(&obj_step_id) {
            Some(g) => g,
            None => continue,
        };
        // The relating object can be either:
        //  - IfcClassificationReference (most common — Edvard's pattern)
        //  - IfcClassification directly (rarer)
        if let Some(r) = refs.get(&relating_id) {
            let system = r.parent_id.and_then(|sid| systems.get(&sid));
            out.guid.push(guid.clone());
            out.system_name.push(system.and_then(|s| s.name.clone()));
            out.edition.push(system.and_then(|s| s.edition.clone()));
            out.identification.push(r.identification.clone());
            out.name.push(r.name.clone());
            out.location.push(r.location.clone());
            out.source.push(system.and_then(|s| s.source.clone()));
        } else if let Some(sys) = systems.get(&relating_id) {
            out.guid.push(guid.clone());
            out.system_name.push(sys.name.clone());
            out.edition.push(sys.edition.clone());
            out.identification.push(None);
            out.name.push(None);
            out.location.push(None);
            out.source.push(sys.source.clone());
        }
    }

    out
}

struct SystemRecord {
    source: Option<String>,
    edition: Option<String>,
    name: Option<String>,
}

struct RefRecord {
    location: Option<String>,
    identification: Option<String>,
    name: Option<String>,
    parent_id: Option<u64>,
}

/// String-at-position, matching ifcopenshell's NULL semantics:
/// both STEP `$` and an empty quoted string `''` map to None.
///
/// Issue #9 surfaced this on SM_RIVr where 1,632 IfcClassificationReference
/// records have `Identification = ''`. Our extractor was returning `Some("")`;
/// ifcopenshell returns `None`. Both encodings mean "no value" semantically.
fn string_at(fields: &[&[u8]], idx: usize) -> Option<String> {
    match parse_field(*fields.get(idx)?) {
        Field::String(s) if !s.is_empty() => Some(s),
        _ => None,
    }
}

fn parse_ref_list(body: &[u8]) -> Vec<u64> {
    split_top_level_args(body)
        .into_iter()
        .filter_map(|f| match parse_field(f) {
            Field::Ref(id) => Some(id),
            _ => None,
        })
        .collect()
}
