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

    pub fn is_empty(&self) -> bool {
        self.guid.is_empty()
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
        let table = crate::entity_table::EntityTable::build(buf.as_bytes());
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

    #[test]
    fn ns_3451_chain_resolves_all_six_fields() {
        // The canonical Norwegian classification chain — verifies the
        // ClassificationReference → Classification (via ReferencedSource)
        // walk, which is the trickier part of this extractor.
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
        // ClassificationReference with no ReferencedSource — every
        // system-level field should be None but the identification +
        // name (carried directly on the reference) must survive. Some
        // exports do this when they ship a reference URL without
        // declaring a parent IfcClassification.
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
        // A wall classified under both NS 3451 and OmniClass — both
        // refs must appear, properly attributed to their parent system.
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
}
