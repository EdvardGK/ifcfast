//! IDS evaluation against the buildingSMART conformance suite (slices 1–2:
//! Entity, Attribute, Property, Classification and Material facets) plus
//! report-shape checks.
//!
//! Suite: `$IFCFAST_IDS_TESTCASES` or
//! `~/.cache/ifcfast/ids-testcases/a67047736aa93586d723329fce3aab1b9ac056af/`
//! (fetch with `python scripts/fetch_ids_testcases.py`). Skipped with a
//! message when absent.
//!
//! Truth from the filename (TestCases/scripts.md, design §4):
//! `pass-` → report ok; `fail-` → not ok; `invalid-` → `InvalidIds` or
//! not ok. A case whose IDS uses PartOf (slice 3) must raise
//! `Unsupported` (an `invalid-` one may also be refused as `InvalidIds` by
//! the parse-time audit).
#![cfg(feature = "ids")]

use std::path::PathBuf;

use _core::entity_table::EntityTable;
use _core::ids::ir::Facet;
use _core::ids::{
    parse_ids, schema_from_header, validate, validate_with, IdsError, OnUnsupported,
    ValidateOptions,
};

const SUITE_SHA: &str = "a67047736aa93586d723329fce3aab1b9ac056af";
/// Slice-1 and slice-2 folders (real outcomes), then `partof`, whose
/// facet arrives in slice 3 (every case there must be `Unsupported`, or
/// `InvalidIds` for an `invalid-` case refused by the parse-time audit).
const FOLDERS: &[&str] = &[
    "entity",
    "attribute",
    "restriction",
    "ids",
    "tolerance",
    "property",
    "classification",
    "material",
    "partof",
];

fn suite_root() -> Option<PathBuf> {
    let p = match std::env::var_os("IFCFAST_IDS_TESTCASES") {
        Some(p) => PathBuf::from(p),
        None => PathBuf::from(std::env::var_os("HOME")?)
            .join(".cache/ifcfast/ids-testcases")
            .join(SUITE_SHA),
    };
    p.is_dir().then_some(p)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Got {
    Pass,
    Fail,
    Invalid,
    Unsupported,
    Error,
}

fn run_case(ids: &[u8], ifc: &[u8]) -> (Got, String) {
    let schema = match schema_from_header(ifc) {
        Ok(s) => s,
        Err(e) => return (Got::Error, e.to_string()),
    };
    let table = EntityTable::build(ifc);
    match validate(ids, &table, schema, OnUnsupported::Raise) {
        Ok(r) if r.ok() => (Got::Pass, String::new()),
        Ok(r) => {
            let failed: Vec<String> = r
                .specs
                .status
                .iter()
                .zip(&r.specs.name)
                .filter(|(s, _)| **s != "pass")
                .map(|(s, n)| format!("{n} [{s}]"))
                .collect();
            let why: Vec<String> = r
                .failures
                .reason_code
                .iter()
                .zip(&r.failures.actual)
                .map(|(c, a)| format!("{c}({})", a.as_deref().unwrap_or("-")))
                .collect();
            (
                Got::Fail,
                format!("{} :: {}", failed.join("; "), why.join(", ")),
            )
        }
        Err(e @ IdsError::InvalidIds { .. }) => (Got::Invalid, e.to_string()),
        Err(e @ IdsError::Unsupported { .. }) => (Got::Unsupported, e.to_string()),
        Err(e) => (Got::Error, e.to_string()),
    }
}

/// Does the IDS use a facet this engine does not implement yet (PartOf)?
/// From the IR when it parses, else from the raw text.
fn uses_unsupported_facet(ids: &[u8]) -> bool {
    match parse_ids(ids) {
        Ok(doc) => doc.specs.iter().any(|s| {
            s.applicability
                .iter()
                .chain(s.requirements.iter().map(|r| &r.facet))
                .any(|f| matches!(f, Facet::PartOf { .. }))
        }),
        Err(_) => {
            let t = String::from_utf8_lossy(ids);
            t.contains("<partOf") || t.contains(":partOf")
        }
    }
}

fn agrees(expected: &str, unsupported: bool, got: Got) -> bool {
    if unsupported {
        return got == Got::Unsupported || (expected == "invalid" && got == Got::Invalid);
    }
    match expected {
        "pass" => got == Got::Pass,
        "fail" => got == Got::Fail,
        "invalid" => matches!(got, Got::Invalid | Got::Fail),
        _ => false,
    }
}

#[test]
fn ids_conformance_suite() {
    let Some(root) = suite_root() else {
        eprintln!(
            "SKIP ids_conformance_suite: IDS test cases not found \
             (set IFCFAST_IDS_TESTCASES or run scripts/fetch_ids_testcases.py)"
        );
        return;
    };
    let mut mismatches: Vec<String> = Vec::new();
    println!(
        "\n{:<15}{:>6}{:>6}{:>6}{:>6}{:>7}{:>7}{:>7}{:>7}",
        "folder", "total", "pass", "fail", "inval", "rejct", "unsup", "agree", "MISM"
    );
    let mut tot = [0usize; 8];
    for folder in FOLDERS {
        let dir = root.join(folder);
        let mut cases: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "ids"))
            .collect();
        cases.sort();
        let mut row = [0usize; 8];
        for ids_path in &cases {
            let stem = ids_path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let expected = stem.split('-').next().unwrap_or("");
            let ids = std::fs::read(ids_path).unwrap_or_else(|e| panic!("{e}"));
            let ifc_path = ids_path.with_extension("ifc");
            let ifc =
                std::fs::read(&ifc_path).unwrap_or_else(|e| panic!("{}: {e}", ifc_path.display()));
            let unsupported = uses_unsupported_facet(&ids);
            let (got, detail) = run_case(&ids, &ifc);
            row[0] += 1;
            match expected {
                "pass" => row[1] += 1,
                "fail" => row[2] += 1,
                _ => row[3] += 1,
            }
            if expected == "invalid" && got == Got::Invalid {
                row[4] += 1;
                if std::env::var_os("IFCFAST_IDS_VERBOSE").is_some() {
                    println!("  rejected {folder}/{stem}: {detail}");
                }
            }
            if got == Got::Unsupported {
                row[5] += 1;
            }
            if got == Got::Fail && std::env::var_os("IFCFAST_IDS_VERBOSE").is_some() {
                println!("  failed {folder}/{stem}: {detail}");
            }
            if agrees(expected, unsupported, got) {
                row[6] += 1;
            } else {
                row[7] += 1;
                mismatches.push(format!(
                    "{folder}/{stem}: expected {expected}{}, got {got:?} — {detail}",
                    if unsupported {
                        " (unsupported facet)"
                    } else {
                        ""
                    }
                ));
            }
        }
        println!(
            "{:<15}{:>6}{:>6}{:>6}{:>6}{:>7}{:>7}{:>7}{:>7}",
            folder, row[0], row[1], row[2], row[3], row[4], row[5], row[6], row[7]
        );
        for (t, r) in tot.iter_mut().zip(row) {
            *t += r;
        }
    }
    println!(
        "{:<15}{:>6}{:>6}{:>6}{:>6}{:>7}{:>7}{:>7}{:>7}",
        "TOTAL", tot[0], tot[1], tot[2], tot[3], tot[4], tot[5], tot[6], tot[7]
    );
    for m in &mismatches {
        println!("MISMATCH {m}");
    }
    assert!(
        mismatches.is_empty(),
        "{} conformance mismatches (listed above)",
        mismatches.len()
    );
}

// --------------------------------------------------------------------------
// Report shape on hand-written fixtures
// --------------------------------------------------------------------------

const IFC: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [CoordinationView]'),'2;1');
FILE_NAME('t.ifc','2026-09-24T00:00:00',(''),(''),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCWALL('1hqIFTRjfV6AWq_bMtnZwI',$,'W-1',$,$,$,$,$,.SOLIDWALL.);
#2=IFCWALL('0eA6m4fELI9QBIhP3wiLAp',$,$,$,$,$,$,'T2',$);
#3=IFCWALLTYPE('05rScmOVzMoQXOfbYdtLYj',$,'WT',$,$,$,$,$,'X',.USERDEFINED.);
#4=IFCRELDEFINESBYTYPE('2cocz3LlfB0wld0Eq66x$S',$,$,$,(#2),#3);
#5=IFCSLAB('3IaY0uquH6XPmXmLftbg4a',$,'S',$,$,$,$,$,$);
ENDSEC;
END-ISO-10303-21;
"#;

fn ids(specs: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<ids xmlns="http://standards.buildingsmart.org/IDS" xmlns:xs="http://www.w3.org/2001/XMLSchema">
<info><title>T</title></info><specifications>{specs}</specifications></ids>"#
    )
}

const WALL_NAME: &str = r#"<specification name="walls named" ifcVersion="IFC4"><applicability minOccurs="1" maxOccurs="unbounded"><entity><name><simpleValue>IFCWALL</simpleValue></name></entity></applicability><requirements><attribute><name><simpleValue>Name</simpleValue></name></attribute><entity><name><simpleValue>IFCWALL</simpleValue></name><predefinedType><simpleValue>SOLIDWALL</simpleValue></predefinedType></entity></requirements></specification>"#;
const DOORS: &str = r#"<specification name="doors" ifcVersion="IFC4"><applicability minOccurs="1" maxOccurs="unbounded"><entity><name><simpleValue>IFCDOOR</simpleValue></name></entity></applicability></specification>"#;
const NO_SLABS: &str = r#"<specification name="no slabs" ifcVersion="IFC4"><applicability minOccurs="0" maxOccurs="0"><entity><name><simpleValue>IFCSLAB</simpleValue></name></entity></applicability></specification>"#;
const PART_OF: &str = r#"<specification name="partof" ifcVersion="IFC4"><applicability minOccurs="1" maxOccurs="unbounded"><entity><name><simpleValue>IFCWALL</simpleValue></name></entity></applicability><requirements><partOf><entity><name><simpleValue>IFCBUILDINGSTOREY</simpleValue></name></entity></partOf></requirements></specification>"#;

#[test]
fn ids_report_columns_and_reason_codes() {
    let buf = IFC.as_bytes();
    let table = EntityTable::build(buf);
    let schema = schema_from_header(buf).unwrap_or_else(|e| panic!("{e}"));
    let x = ids(&format!("{WALL_NAME}{DOORS}{NO_SLABS}"));
    let r = validate(x.as_bytes(), &table, schema, OnUnsupported::Raise)
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(!r.ok());
    let s = &r.specs;
    assert_eq!(s.spec_index, vec![0, 1, 2]);
    assert_eq!(s.status, vec!["fail", "fail", "fail"]);
    assert_eq!(
        s.reason_code,
        vec![
            None,
            Some("SPEC_NO_APPLICABLE"),
            Some("SPEC_PROHIBITED_APPLICABLE")
        ]
    );
    assert_eq!(s.cardinality, vec!["required", "required", "prohibited"]);
    assert_eq!(s.applicable, vec![2, 0, 1]);
    assert_eq!(s.passed, vec![1, 0, 0]);
    assert_eq!(s.failed, vec![1, 0, 1]);
    assert_eq!(s.applicability_label[0], "All IFCWALL data");
    assert_eq!(
        s.requirement_labels[0],
        vec![
            "The Name shall be provided",
            "Shall be IFCWALL data of type SOLIDWALL"
        ]
    );

    let e = &r.elements;
    assert_eq!(e.step_id, vec![1, 2, 5]);
    assert_eq!(e.spec_index, vec![0, 0, 2]);
    assert_eq!(e.status, vec!["pass", "fail", "fail"]);
    assert_eq!(e.n_failed, vec![0, 2, 0]);
    assert_eq!(e.guid[0].as_deref(), Some("1hqIFTRjfV6AWq_bMtnZwI"));
    assert_eq!(e.entity[0], "IFCWALL");
    assert_eq!(
        e.predefined_type,
        vec![Some("SOLIDWALL".into()), Some("X".into()), None]
    );
    assert_eq!(e.type_step_id, vec![None, Some(3), None]);
    assert_eq!(e.tag[1].as_deref(), Some("T2"));
    assert_eq!(e.name[0].as_deref(), Some("W-1"));

    let f = &r.failures;
    assert_eq!(f.step_id, vec![2, 2]);
    assert_eq!(f.requirement_index, vec![0, 1]);
    assert_eq!(f.facet_type, vec!["attribute", "entity"]);
    assert_eq!(f.reason_code, vec!["ATTR_MISSING", "PREDEFINED_MISMATCH"]);
    assert_eq!(f.actual, vec![Some("None".into()), Some("X".into())]);
    assert_eq!(f.value_source, vec![Some("instance"), Some("type")]);
    assert_eq!(f.expected[1], "Shall be IFCWALL data of type SOLIDWALL");
}

#[test]
fn ids_unsupported_raise_vs_mark() {
    let buf = IFC.as_bytes();
    let table = EntityTable::build(buf);
    let x = ids(&format!("{WALL_NAME}{PART_OF}"));
    match validate(
        x.as_bytes(),
        &table,
        _core::ids::Schema::Ifc4,
        OnUnsupported::Raise,
    ) {
        Err(IdsError::Unsupported {
            feature,
            spec_index,
        }) => {
            assert_eq!(feature, "facet:part_of");
            assert_eq!(spec_index, Some(1));
        }
        other => panic!("{other:?}"),
    }
    let r = validate(
        x.as_bytes(),
        &table,
        _core::ids::Schema::Ifc4,
        OnUnsupported::Mark,
    )
    .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(r.specs.status, vec!["fail", "unsupported"]);
    assert_eq!(
        r.specs.unsupported_feature[1].as_deref(),
        Some("facet:part_of")
    );
    assert!(
        r.elements.spec_index.iter().all(|&i| i == 0),
        "no rows for the unsupported spec"
    );
    assert!(!r.ok(), "an unsupported spec is never ok");
}

#[test]
fn ids_many_documents_share_one_table_and_version_filter_is_opt_in() {
    let buf = IFC.as_bytes();
    let table = EntityTable::build(buf);
    let a = ids(WALL_NAME);
    let b = ids(&NO_SLABS.replace("IFC4\"", "IFC2X3\""));
    let docs: Vec<&[u8]> = vec![a.as_bytes(), b.as_bytes()];
    let r = validate_with(
        &docs,
        &table,
        _core::ids::Schema::Ifc4,
        ValidateOptions::default(),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(r.specs.ids_index, vec![0, 1]);
    assert_eq!(r.specs.spec_index, vec![0, 1]);
    assert_eq!(r.specs.status[1], "fail");
    let r = validate_with(
        &docs,
        &table,
        _core::ids::Schema::Ifc4,
        ValidateOptions {
            on_unsupported: OnUnsupported::Raise,
            filter_ifc_version: true,
        },
    )
    .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(r.specs.status[1], "skipped_ifc_version");
}

#[test]
fn ids_truncated_and_unknown_schema_are_refused() {
    let table = EntityTable::build(b"ISO-10303-21;\nHEADER;\nENDSEC;\nDATA;\n#1=IFCWALL('x',$");
    let x = ids(DOORS);
    assert!(matches!(
        validate(
            x.as_bytes(),
            &table,
            _core::ids::Schema::Ifc4,
            OnUnsupported::Raise
        ),
        Err(IdsError::IfcInput { .. })
    ));
    let hdr = b"ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC2X2_FINAL'));\nENDSEC;\nDATA;\nENDSEC;\n";
    assert!(matches!(
        schema_from_header(hdr),
        Err(IdsError::IfcInput { .. })
    ));
}

// --------------------------------------------------------------------------
// Slice 2: property / classification / material semantics that the suite
// does not pin (docs/ids/ambiguities.md A14, A16, A18, A26, D8) plus the
// ones it pins, on one hand-written file.
// --------------------------------------------------------------------------

const IFC2: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('s2.ifc','2026-09-25T00:00:00',(''),(''),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCPROJECT('0Test000000000000000001',$,'p',$,$,$,$,$,#2);
#2=IFCUNITASSIGNMENT((#3));
#3=IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.);
#10=IFCWALL('1hqIFTRjfV6AWq_bMtnZwI',$,'W',$,$,$,$,$,$);
#11=IFCWALLTYPE('05rScmOVzMoQXOfbYdtLYj',$,'WT',$,$,(#32),$,$,$,.STANDARD.);
#12=IFCRELDEFINESBYTYPE('2cocz3LlfB0wld0Eq66x$S',$,$,$,(#10),#11);
#20=IFCPROPERTYSINGLEVALUE('Width',$,IFCLENGTHMEASURE(2000.),$);
#21=IFCPROPERTYLISTVALUE('Heights',$,(IFCLENGTHMEASURE(1000.),IFCLENGTHMEASURE(3000.)),$);
#22=IFCPROPERTYSINGLEVALUE('TypeLabel',$,IFCTEXT('x'),$);
#23=IFCCOMPLEXPROPERTY('Cx',$,'u',(#25));
#24=IFCPROPERTYSINGLEVALUE('Temp',$,IFCTHERMODYNAMICTEMPERATUREMEASURE(20.),$);
#25=IFCPROPERTYSINGLEVALUE('Inner',$,IFCLABEL('i'),$);
#30=IFCPROPERTYSET('3IaY0uquH6XPmXmLftbg4a',$,'P',$,(#20,#21,#23,#24));
#31=IFCRELDEFINESBYPROPERTIES('0eA6m4fELI9QBIhP3wiLAp',$,$,$,(#10),#30);
#32=IFCPROPERTYSET('1Pset0000000000000000x',$,'T',$,(#22));
#40=IFCCLASSIFICATION($,$,$,'Sys',$,$,$);
#41=IFCCLASSIFICATIONREFERENCE($,'2','Parent',#40,$,$);
#42=IFCCLASSIFICATIONREFERENCE($,'22','Leaf',#41,$,$);
#43=IFCRELASSOCIATESCLASSIFICATION('2Cls000000000000000001',$,$,$,(#10),#42);
#50=IFCMATERIAL('Concrete',$,'CAT');
#51=IFCMATERIALLAYER(#50,200.,$,'L1',$,$,$);
#52=IFCMATERIALLAYERSET((#51),'Set A',$);
#53=IFCMATERIALLAYERSETUSAGE(#52,.AXIS2.,.POSITIVE.,0.,$);
#54=IFCRELASSOCIATESMATERIAL('2Rel00000000000000001',$,$,$,(#11),#53);
#60=IFCDOORPANELPROPERTIES('1Door0000000000000000x',$,'Panel',$,$,.SWINGING.,$,.LEFT.,$);
#61=IFCRELDEFINESBYPROPERTIES('1Rdp0000000000000000x',$,$,$,(#10),#60);
ENDSEC;
END-ISO-10303-21;
"#;

fn wall_spec(name: &str, req: &str) -> String {
    format!(
        r#"<specification name="{name}" ifcVersion="IFC4"><applicability minOccurs="1" maxOccurs="unbounded"><entity><name><simpleValue>IFCWALL</simpleValue></name></entity></applicability><requirements>{req}</requirements></specification>"#
    )
}

fn prop(card: &str, dt: &str, pset: &str, name: &str, value: &str) -> String {
    let dt = if dt.is_empty() {
        String::new()
    } else {
        format!(r#" dataType="{dt}""#)
    };
    format!(
        r#"<property cardinality="{card}"{dt}><propertySet><simpleValue>{pset}</simpleValue></propertySet><baseName><simpleValue>{name}</simpleValue></baseName>{value}</property>"#
    )
}

fn sv(v: &str) -> String {
    format!("<value><simpleValue>{v}</simpleValue></value>")
}

/// (spec name, status, first failure's reason code / actual / value_source).
fn outcome(
    r: &_core::ids::IdsReport,
    name: &str,
) -> (String, Option<String>, Option<String>, Option<String>) {
    let i = r
        .specs
        .name
        .iter()
        .position(|n| n == name)
        .unwrap_or_else(|| panic!("no spec {name}"));
    let gi = r.specs.spec_index[i];
    let f = r.failures.spec_index.iter().position(|s| *s == gi);
    (
        r.specs.status[i].to_string(),
        f.map(|j| r.failures.reason_code[j].to_string()),
        f.and_then(|j| r.failures.actual[j].clone()),
        f.and_then(|j| r.failures.value_source[j].map(str::to_string)),
    )
}

#[test]
fn ids_slice2_semantics_on_fixture() {
    let buf = IFC2.as_bytes();
    let table = EntityTable::build(buf);
    let restr = r#"<value><xs:restriction base="xs:double"><xs:minInclusive value="0.5"/><xs:maxInclusive value="2"/></xs:restriction></value>"#;
    let enum_r = r#"<value><xs:restriction base="xs:double"><xs:enumeration value="3"/><xs:enumeration value="7"/></xs:restriction></value>"#;
    let cls = |sys: &str, v: &str| {
        format!(
            r#"<classification><value><simpleValue>{v}</simpleValue></value><system><simpleValue>{sys}</simpleValue></system></classification>"#
        )
    };
    let mat = |v: &str| format!("<material>{}</material>", sv(v));
    let specs = [
        // P13: 2000 mm is 2 m.
        wall_spec(
            "si",
            &prop("required", "IFCLENGTHMEASURE", "P", "Width", &sv("2")),
        ),
        // D8: a bounded restriction must hold for every list value (3 m > 2).
        wall_spec(
            "d8_bounds_all",
            &prop("required", "", "P", "Heights", restr),
        ),
        // Enumeration restrictions stay any-of.
        wall_spec("d8_enum_any", &prop("required", "", "P", "Heights", enum_r)),
        // A14: the type-inherited value is dataType-checked too.
        wall_spec("a14", &prop("required", "IFCLABEL", "T", "TypeLabel", "")),
        // A16: complex = absent.
        wall_spec("a16_req", &prop("required", "", "P", "Cx", "")),
        wall_spec("a16_opt", &prop("optional", "", "P", "Cx", "")),
        wall_spec("a16_proh", &prop("prohibited", "", "P", "Cx", "")),
        // A18 / P16: predefined-set attributes carry their declared type.
        wall_spec(
            "a18_ok",
            &prop(
                "required",
                "IFCDOORPANELOPERATIONENUM",
                "Panel",
                "PanelOperation",
                &sv("SWINGING"),
            ),
        ),
        wall_spec(
            "a18_bad",
            &prop("required", "IFCLABEL", "Panel", "PanelOperation", ""),
        ),
        wall_spec("pset_missing", &prop("required", "", "Nope", "Width", "")),
        wall_spec("prop_missing", &prop("required", "", "P", "Nope", "")),
        // C5: an ancestor reference's code counts; no prefix matching.
        wall_spec("c5_parent", &cls("Sys", "2")),
        wall_spec("c4_exact", &cls("Sys", "3")),
        wall_spec("c2_system", &cls("Other", "22")),
        // M1–M3: usage → set, type-inherited, any of set / layer / material strings.
        wall_spec("m_set", &mat("Set A")),
        wall_spec("m_layer", &mat("L1")),
        wall_spec("m_cat", &mat("CAT")),
        wall_spec("m_bad", &mat("Nope")),
    ];
    let x = ids(&specs.concat());
    let r = validate(
        x.as_bytes(),
        &table,
        _core::ids::Schema::Ifc4,
        OnUnsupported::Raise,
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let pass = |n: &str| assert_eq!(outcome(&r, n).0, "pass", "{n}: {:?}", outcome(&r, n));
    let fail = |n: &str, code: &str, actual: Option<&str>, src: Option<&str>| {
        let o = outcome(&r, n);
        assert_eq!(o.0, "fail", "{n}");
        assert_eq!(o.1.as_deref(), Some(code), "{n}: {o:?}");
        assert_eq!(o.2.as_deref(), actual, "{n}: {o:?}");
        assert_eq!(o.3.as_deref(), src, "{n}: {o:?}");
    };
    pass("si");
    fail(
        "d8_bounds_all",
        "PROP_VALUE_MISMATCH",
        Some("[1.0, 3.0]"),
        Some("instance"),
    );
    pass("d8_enum_any");
    fail(
        "a14",
        "PROP_DATATYPE_MISMATCH",
        Some("IFCTEXT"),
        Some("type"),
    );
    fail(
        "a16_req",
        "PROP_UNSUPPORTED",
        Some("IFCCOMPLEXPROPERTY"),
        Some("instance"),
    );
    pass("a16_opt");
    pass("a16_proh");
    pass("a18_ok");
    fail(
        "a18_bad",
        "PROP_DATATYPE_MISMATCH",
        Some("IFCDOORPANELOPERATIONENUM"),
        Some("instance"),
    );
    fail("pset_missing", "PSET_MISSING", None, None);
    fail("prop_missing", "PROP_MISSING", None, None);
    pass("c5_parent");
    fail(
        "c4_exact",
        "CLASS_VALUE_MISMATCH",
        Some("['2', '22']"),
        Some("instance"),
    );
    fail(
        "c2_system",
        "CLASS_SYSTEM_MISMATCH",
        Some("['Sys', 'Sys']"),
        Some("instance"),
    );
    pass("m_set");
    pass("m_layer");
    pass("m_cat");
    fail(
        "m_bad",
        "MATERIAL_VALUE_MISMATCH",
        Some("{'CAT', 'Concrete', 'L1', 'Set A'}"),
        Some("type"),
    );
    assert_eq!(
        r.specs.requirement_labels[0],
        vec!["Width data shall be 2 and in the dataset P"]
    );
    let c5 = r
        .specs
        .name
        .iter()
        .position(|n| n == "c5_parent")
        .unwrap_or(0);
    assert_eq!(
        r.specs.requirement_labels[c5],
        vec!["Shall have a Sys reference of 2"]
    );
    let m = r.specs.name.iter().position(|n| n == "m_bad").unwrap_or(0);
    assert_eq!(
        r.specs.requirement_labels[m],
        vec!["Shall have a material of Nope"]
    );
}

/// A26: a comparison that needs an undeclared unit is `UnresolvedUnit`
/// under `raise`, and an `unsupported` spec (`unit:<TYPE>`, no element
/// rows) under `mark`; the other specs keep their results.
#[test]
fn ids_unresolved_unit_raise_vs_mark() {
    let buf = IFC2.as_bytes();
    let table = EntityTable::build(buf);
    let temp = wall_spec("temp", &prop("required", "", "P", "Temp", &sv("293.15")));
    let width = wall_spec("si", &prop("required", "", "P", "Width", &sv("2")));
    let x = ids(&format!("{width}{temp}"));
    match validate(
        x.as_bytes(),
        &table,
        _core::ids::Schema::Ifc4,
        OnUnsupported::Raise,
    ) {
        Err(IdsError::UnresolvedUnit { unit_type }) => {
            assert_eq!(unit_type, "THERMODYNAMICTEMPERATUREUNIT")
        }
        other => panic!("{other:?}"),
    }
    let r = validate(
        x.as_bytes(),
        &table,
        _core::ids::Schema::Ifc4,
        OnUnsupported::Mark,
    )
    .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(r.specs.status, vec!["pass", "unsupported"]);
    assert_eq!(
        r.specs.unsupported_feature[1].as_deref(),
        Some("unit:THERMODYNAMICTEMPERATUREUNIT")
    );
    assert_eq!(r.specs.applicable[1], 0);
    assert!(r.elements.spec_index.iter().all(|&i| i == 0));
    assert!(r.failures.spec_index.iter().all(|&i| i == 0));
    // A presence-only check needs no unit.
    let name_only = wall_spec("temp_name", &prop("required", "", "P", "Temp", ""));
    let x = ids(&name_only);
    let r = validate(
        x.as_bytes(),
        &table,
        _core::ids::Schema::Ifc4,
        OnUnsupported::Raise,
    )
    .unwrap_or_else(|e| panic!("{e}"));
    assert!(r.ok());
}
