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

use std::collections::{HashMap, HashSet};

use crate::entity_table::EntityTable;
use crate::indexer::IndexedFile;
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

    pub fn is_empty(&self) -> bool {
        self.guid.is_empty()
    }
}

/// Build classification rows. Resolves related objects through
/// `object_step_to_guid` (products, types, spatial, etc.).
pub fn build(
    table: &EntityTable,
    object_step_to_guid: &HashMap<u64, String>,
) -> ClassificationTable {
    let (systems, refs, rel_pairs) = collect_classification_maps(table);

    let mut out = ClassificationTable::default();
    for (obj_step_id, relating_id) in rel_pairs {
        let guid = match object_step_to_guid.get(&obj_step_id) {
            Some(g) => g,
            None => continue,
        };
        // The relating object can be either:
        //  - IfcClassificationReference (most common — Edvard's pattern)
        //  - IfcClassification directly (rarer)
        if let Some(r) = refs.get(&relating_id) {
            let system = r.parent_id.and_then(|sid| systems.get(&sid));
            push_ref_row(&mut out, guid, r, system);
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

/// IDS-oriented expansion: inherited `IfcClassificationReference` chains and
/// type-object classifications propagated to products via `IfcRelDefinesByType`.
pub fn expand_for_ids(
    table: &EntityTable,
    cls: &mut ClassificationTable,
    indexed: &IndexedFile,
) {
    let object_step_to_guid = crate::object_guid::build_extractor_object_map(indexed, table, true);
    let (systems, refs, rel_pairs) = collect_classification_maps(table);

    let mut seen = row_keys(cls);

    // 1. Inherited reference rows — walk parent ref chains on each association.
    for (obj_step_id, relating_id) in rel_pairs {
        let guid = match object_step_to_guid.get(&obj_step_id) {
            Some(g) => g,
            None => continue,
        };
        let Some(leaf) = refs.get(&relating_id) else {
            continue;
        };
        let mut current = leaf.parent_id;
        while let Some(pid) = current {
            let Some(r) = refs.get(&pid) else {
                break;
            };
            let system = resolve_system(r, &systems, &refs);
            let key = row_key(guid, &system.0, &r.identification);
            if seen.insert(key) {
                push_ref_row_with_system(cls, guid, r, &system);
            }
            current = r.parent_id.filter(|p| refs.contains_key(p));
        }
    }

    // 2. Type → product propagation when the product has no row for that system.
    let type_guids: HashSet<&str> = indexed
        .type_object_guid
        .iter()
        .map(|s| s.as_str())
        .collect();

    let mut product_systems: HashSet<(String, Option<String>)> = HashSet::new();
    for i in 0..cls.len() {
        if indexed.product_guid.iter().any(|g| g == &cls.guid[i]) {
            product_systems.insert((cls.guid[i].clone(), cls.system_name[i].clone()));
        }
    }

    for (prod_sid, type_sid) in indexed
        .defines_by_type_product
        .iter()
        .zip(indexed.defines_by_type_type.iter())
    {
        let (Some(prod_guid), Some(type_guid)) = (
            object_step_to_guid.get(prod_sid),
            object_step_to_guid.get(type_sid),
        ) else {
            continue;
        };
        if !type_guids.contains(type_guid.as_str()) {
            continue;
        }
        for i in 0..cls.len() {
            if cls.guid[i] != *type_guid {
                continue;
            }
            let system = cls.system_name[i].clone();
            if product_systems.contains(&(prod_guid.clone(), system.clone())) {
                continue;
            }
            let key = row_key(prod_guid, &system, &cls.identification[i]);
            if !seen.insert(key) {
                continue;
            }
            cls.guid.push(prod_guid.clone());
            cls.system_name.push(system.clone());
            cls.edition.push(cls.edition[i].clone());
            cls.identification.push(cls.identification[i].clone());
            cls.name.push(cls.name[i].clone());
            cls.location.push(cls.location[i].clone());
            cls.source.push(cls.source[i].clone());
            product_systems.insert((prod_guid.clone(), system));
        }
    }
}

fn collect_classification_maps(
    table: &EntityTable,
) -> (
    HashMap<u64, SystemRecord>,
    HashMap<u64, RefRecord>,
    Vec<(u64, u64)>,
) {
    let mut systems: HashMap<u64, SystemRecord> = HashMap::with_capacity(64);
    let mut refs: HashMap<u64, RefRecord> = HashMap::with_capacity(1024);
    let mut rel_pairs: Vec<(u64, u64)> = Vec::with_capacity(4096);

    for (step_id, type_name, args) in table.iter() {
        if type_name.eq_ignore_ascii_case(b"IFCCLASSIFICATION") {
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
        } else if type_name.eq_ignore_ascii_case(b"IFCEXTERNALREFERENCERELATIONSHIP") {
            // IFC4: (Name, Description, RelatingReference, RelatedResourceObjects)
            let fields = split_top_level_args(args);
            let relating = match fields.get(2).copied().map(parse_field) {
                Some(Field::Ref(id)) => id,
                _ => continue,
            };
            let relateds = match fields.get(3).copied().map(parse_field) {
                Some(Field::List(body)) => parse_ref_list(body),
                Some(Field::Ref(id)) => vec![id],
                _ => continue,
            };
            for obj_id in relateds {
                rel_pairs.push((obj_id, relating));
            }
        }
    }

    (systems, refs, rel_pairs)
}

fn resolve_system(
    r: &RefRecord,
    systems: &HashMap<u64, SystemRecord>,
    refs: &HashMap<u64, RefRecord>,
) -> (Option<String>, Option<String>, Option<String>) {
    let mut current = r.parent_id;
    while let Some(pid) = current {
        if let Some(sys) = systems.get(&pid) {
            return (
                sys.name.clone(),
                sys.edition.clone(),
                sys.source.clone(),
            );
        }
        if let Some(parent_ref) = refs.get(&pid) {
            current = parent_ref.parent_id;
        } else {
            break;
        }
    }
    (None, None, None)
}

fn push_ref_row(
    out: &mut ClassificationTable,
    guid: &str,
    r: &RefRecord,
    system: Option<&SystemRecord>,
) {
    out.guid.push(guid.to_string());
    out.system_name.push(system.and_then(|s| s.name.clone()));
    out.edition.push(system.and_then(|s| s.edition.clone()));
    out.identification.push(r.identification.clone());
    out.name.push(r.name.clone());
    out.location.push(r.location.clone());
    out.source.push(system.and_then(|s| s.source.clone()));
}

fn push_ref_row_with_system(
    out: &mut ClassificationTable,
    guid: &str,
    r: &RefRecord,
    system: &(Option<String>, Option<String>, Option<String>),
) {
    out.guid.push(guid.to_string());
    out.system_name.push(system.0.clone());
    out.edition.push(system.1.clone());
    out.identification.push(r.identification.clone());
    out.name.push(r.name.clone());
    out.location.push(r.location.clone());
    out.source.push(system.2.clone());
}

fn row_key(guid: &str, system: &Option<String>, identification: &Option<String>) -> (String, Option<String>, Option<String>) {
    (guid.to_string(), system.clone(), identification.clone())
}

fn row_keys(cls: &ClassificationTable) -> HashSet<(String, Option<String>, Option<String>)> {
    (0..cls.len())
        .map(|i| row_key(&cls.guid[i], &cls.system_name[i], &cls.identification[i]))
        .collect()
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
    match parse_field(fields.get(idx)?) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_buf(extra_data: &str) -> String {
        format!(
            r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('cls_test.ifc','2026-05-26T00:00:00',('test'),('test'),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCPROJECT('0Test000000000000000001',$,'p',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#3));
#3=IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.);
#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);
#6=IFCAXIS2PLACEMENT3D(#7,$,$);
#7=IFCCARTESIANPOINT((0.,0.,0.));
#10=IFCWALL('1Wall00000000000000001',$,'W',$,$,$,$,'t',.STANDARD.);
{extra_data}
ENDSEC;
END-ISO-10303-21;
"#
        )
    }

    fn run(buf: &str) -> ClassificationTable {
        let table = crate::entity_table::EntityTable::build_from_slice(buf.as_bytes());
        let mut step_to_guid: HashMap<u64, String> = HashMap::new();
        for (sid, _t, args) in table.iter() {
            let fields = split_top_level_args(args);
            if let Some(first) = fields.first() {
                if let Field::String(s) = parse_field(first) {
                    if s.len() == 22 {
                        step_to_guid.insert(sid, s);
                    }
                }
            }
        }
        build(&table, &step_to_guid)
    }

    fn run_indexed(buf: &str) -> ClassificationTable {
        let indexed = crate::indexer::index(buf.as_bytes());
        let table = crate::entity_table::EntityTable::build_from_slice(buf.as_bytes());
        let map = crate::object_guid::build_object_step_to_guid(&indexed);
        let mut cls = build(&table, &map);
        expand_for_ids(&table, &mut cls, &indexed);
        cls
    }

    #[test]
    fn ns_3451_chain_resolves_all_six_fields() {
        let buf = make_buf(
            r#"
#30=IFCCLASSIFICATION('Standard Norge','2022',$,'NS 3451');
#31=IFCCLASSIFICATIONREFERENCE($,'232.1','Yttervegger',#30);
#32=IFCRELASSOCIATESCLASSIFICATION('2Cls000000000000000001',$,$,$,(#10),#31);
"#,
        );
        let t = run(&buf);
        assert_eq!(t.len(), 1);
        assert_eq!(t.guid[0], "1Wall00000000000000001");
        assert_eq!(t.system_name[0].as_deref(), Some("NS 3451"));
        assert_eq!(t.edition[0].as_deref(), Some("2022"));
        assert_eq!(t.identification[0].as_deref(), Some("232.1"));
        assert_eq!(t.name[0].as_deref(), Some("Yttervegger"));
        assert_eq!(t.source[0].as_deref(), Some("Standard Norge"));
    }

    #[test]
    fn missing_parent_classification_still_emits_row() {
        let buf = make_buf(
            r#"
#31=IFCCLASSIFICATIONREFERENCE('https://example/codes/A1','A1','Test class',$);
#32=IFCRELASSOCIATESCLASSIFICATION('2Cls000000000000000001',$,$,$,(#10),#31);
"#,
        );
        let t = run(&buf);
        assert_eq!(t.len(), 1);
        assert_eq!(t.identification[0].as_deref(), Some("A1"));
        assert_eq!(t.name[0].as_deref(), Some("Test class"));
        assert_eq!(t.location[0].as_deref(), Some("https://example/codes/A1"));
        assert!(t.system_name[0].is_none());
        assert!(t.edition[0].is_none());
        assert!(t.source[0].is_none());
    }

    #[test]
    fn one_product_with_multiple_classifications_emits_a_row_each() {
        let buf = make_buf(
            r#"
#30=IFCCLASSIFICATION('Standard Norge','2022',$,'NS 3451');
#31=IFCCLASSIFICATIONREFERENCE($,'232.1','Yttervegger',#30);
#40=IFCCLASSIFICATION('OmniClass','2015',$,'OmniClass');
#41=IFCCLASSIFICATIONREFERENCE($,'21-01 10 10','Exterior Wall',#40);
#50=IFCRELASSOCIATESCLASSIFICATION('2Cls000000000000000001',$,$,$,(#10),#31);
#51=IFCRELASSOCIATESCLASSIFICATION('3Cls000000000000000001',$,$,$,(#10),#41);
"#,
        );
        let t = run(&buf);
        assert_eq!(t.len(), 2);
        let by_system: std::collections::HashMap<&str, &str> = (0..t.len())
            .filter_map(|i| {
                let sys = t.system_name[i].as_deref()?;
                let ident = t.identification[i].as_deref()?;
                Some((sys, ident))
            })
            .collect();
        assert_eq!(by_system.get("NS 3451"), Some(&"232.1"));
        assert_eq!(by_system.get("OmniClass"), Some(&"21-01 10 10"));
    }

    #[test]
    fn inherited_parent_reference_emits_extra_row() {
        let buf = make_buf(
            r#"
#30=IFCCLASSIFICATION('Standard Norge','2022',$,'NS 3451');
#31=IFCCLASSIFICATIONREFERENCE($,'23','Chapter 23',#30);
#32=IFCCLASSIFICATIONREFERENCE($,'232.1','Yttervegger',#31);
#33=IFCRELASSOCIATESCLASSIFICATION('2Cls000000000000000001',$,$,$,(#10),#32);
"#,
        );
        let indexed = crate::indexer::index(buf.as_bytes());
        let table = crate::entity_table::EntityTable::build_from_slice(buf.as_bytes());
        let map = crate::object_guid::build_object_step_to_guid(&indexed);
        let mut t = build(&table, &map);
        expand_for_ids(&table, &mut t, &indexed);
        let idents: Vec<_> = t
            .identification
            .iter()
            .filter_map(|i| i.as_deref())
            .collect();
        assert!(idents.contains(&"232.1"));
        assert!(idents.contains(&"23"));
    }

    #[test]
    fn external_reference_relationship_on_material() {
        let buf = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('extref.ifc','2026-05-26T00:00:00',('test'),('test'),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCPROJECT('0Test000000000000000001',$,'p',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#3));
#3=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);
#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);
#6=IFCAXIS2PLACEMENT3D(#7,$,$);
#7=IFCCARTESIANPOINT((0.,0.,0.));
#30=IFCCLASSIFICATION($,$,$,'Foobar',$,$,$);
#31=IFCCLASSIFICATIONREFERENCE($,'1','Label',#30);
#16=IFCMATERIAL('Material',$,$);
#17=IFCEXTERNALREFERENCERELATIONSHIP($,$,#31,(#16));
ENDSEC;
END-ISO-10303-21;
"#;
        let indexed = crate::indexer::index(buf.as_bytes());
        let table = crate::entity_table::EntityTable::build_from_slice(buf.as_bytes());
        let map = crate::object_guid::build_extractor_object_map(&indexed, &table, true);
        let t = build(&table, &map);
        assert_eq!(t.len(), 1);
        assert_eq!(t.guid[0], crate::object_guid::material_step_guid(16));
        assert_eq!(t.identification[0].as_deref(), Some("1"));
        assert_eq!(t.system_name[0].as_deref(), Some("Foobar"));
    }

    #[test]
    fn type_classification_propagates_to_product() {
        let buf = make_buf(
            r#"
#20=IFCWALLTYPE('1Typ000000000000000001',$,'WT',$,$,$,$,$,$,.STANDARD.);
#30=IFCCLASSIFICATION('Standard Norge','2022',$,'NS 3451');
#31=IFCCLASSIFICATIONREFERENCE($,'232.1','Yttervegger',#30);
#32=IFCRELASSOCIATESCLASSIFICATION('2Cls000000000000000001',$,$,$,(#20),#31);
#40=IFCRELDEFINESBYTYPE('3Typ000000000000000001',$,$,$,(#10),#20);
"#,
        );
        let t = run_indexed(&buf);
        let wall_rows: Vec<_> = t
            .guid
            .iter()
            .zip(t.identification.iter())
            .filter(|(g, _)| g.as_str() == "1Wall00000000000000001")
            .collect();
        assert_eq!(wall_rows.len(), 1);
        assert_eq!(wall_rows[0].1.as_deref(), Some("232.1"));
    }
}
