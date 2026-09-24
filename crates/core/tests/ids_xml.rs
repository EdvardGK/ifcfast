//! IDS 1.0 parse front-end: `parse_ids` → IR, strict rejections, the
//! schema-free audit, and the buildingSMART conformance fixtures.
#![cfg(feature = "ids")]

use std::path::{Path, PathBuf};

use _core::ids::ir::{
    EntityFacet, Facet, FacetCardinality, IdsDocument, Relation, Schema, SpecCardinality, Val,
    XsdBase,
};
use _core::ids::{parse_ids, IdsError};

const HEAD: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<ids xmlns="http://standards.buildingsmart.org/IDS" xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xsi:schemaLocation="http://standards.buildingsmart.org/IDS http://standards.buildingsmart.org/IDS/1.0/ids.xsd">
  <info><title>T</title></info>
  <specifications>
"#;
const TAIL: &str = "  </specifications>\n</ids>\n";

/// Wrap one `<specification>` body.
fn doc(spec: &str) -> String {
    format!("{HEAD}{spec}{TAIL}")
}

/// A spec with the given applicability attributes/body and requirements body.
fn spec(app_attrs: &str, app: &str, req: &str) -> String {
    doc(&format!(
        r#"<specification name="S" ifcVersion="IFC4"><applicability {app_attrs}>{app}</applicability><requirements>{req}</requirements></specification>"#
    ))
}

const WALL: &str = "<entity><name><simpleValue>IFCWALL</simpleValue></name></entity>";
const REQ: &str = r#"minOccurs="1" maxOccurs="unbounded""#;

fn parse(s: &str) -> Result<IdsDocument, IdsError> {
    parse_ids(s.as_bytes())
}

fn invalid(s: &str) -> (String, Option<u32>, String) {
    match parse(s) {
        Err(IdsError::InvalidIds { path, line, msg }) => (path, line, msg),
        other => panic!("expected InvalidIds, got {other:?}\n{s}"),
    }
}

fn assert_invalid(s: &str, needle: &str) {
    let (path, _, msg) = invalid(s);
    assert!(
        msg.contains(needle) || path.contains(needle),
        "error should mention {needle:?}: path={path} msg={msg}"
    );
}

#[test]
fn ids_minimal_document_to_expected_ir() {
    let x = doc(r#"<specification name="Walls" ifcVersion="IFC2X3 IFC4X3_ADD2" identifier="W-1" description="d" instructions="i">
      <applicability minOccurs="1" maxOccurs="unbounded">
        <entity><name><simpleValue>IFCWALL</simpleValue></name><predefinedType><simpleValue>SOLIDWALL</simpleValue></predefinedType></entity>
        <partOf relation="IFCRELVOIDSELEMENT IFCRELFILLSELEMENT"><entity><name><simpleValue>IFCWALL</simpleValue></name></entity></partOf>
        <property dataType="IFCBOOLEAN"><propertySet><simpleValue>Pset_WallCommon</simpleValue></propertySet><baseName><simpleValue>IsExternal</simpleValue></baseName><value><simpleValue>true</simpleValue></value></property>
      </applicability>
      <requirements description="rd">
        <material cardinality="optional" uri="https://x/y"><value><simpleValue>Concrete</simpleValue></value></material>
        <attribute cardinality="prohibited" instructions="no"><name><simpleValue>Description</simpleValue></name></attribute>
        <classification><value><xs:restriction base="xs:string"><xs:pattern value="21.*"/></xs:restriction></value><system><simpleValue>NS 3451</simpleValue></system></classification>
        <property dataType="IFCLENGTHMEASURE"><propertySet><simpleValue>Qto</simpleValue></propertySet><baseName><simpleValue>Width</simpleValue></baseName>
          <value><xs:restriction base="xs:double"><xs:minInclusive value="0.1"/><xs:maxExclusive value="1"/></xs:restriction></value></property>
        <partOf relation="IFCRELCONTAINEDINSPATIALSTRUCTURE" cardinality="prohibited"><entity><name><simpleValue>IFCSPACE</simpleValue></name></entity></partOf>
      </requirements>
    </specification>
"#);
    let d = parse(&x).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(d.info.title, "T");
    assert_eq!(d.specs.len(), 1);
    let s = &d.specs[0];
    assert_eq!(s.idx, 0);
    assert_eq!(s.name, "Walls");
    assert_eq!(s.identifier.as_deref(), Some("W-1"));
    assert_eq!(s.instructions.as_deref(), Some("i"));
    assert_eq!(s.ifc_versions, vec![Schema::Ifc2x3, Schema::Ifc4x3]);
    assert_eq!(s.cardinality, SpecCardinality::Required);
    assert_eq!(s.requirements_description.as_deref(), Some("rd"));
    assert_eq!(
        s.applicability[0],
        Facet::Entity(EntityFacet {
            name: Val::Simple("IFCWALL".into()),
            predefined_type: Some(Val::Simple("SOLIDWALL".into())),
        })
    );
    match &s.applicability[1] {
        Facet::PartOf { relation, .. } => assert_eq!(*relation, Some(Relation::VoidsElementFillsElement)),
        f => panic!("{f:?}"),
    }
    match &s.applicability[2] {
        Facet::Property { data_type, uri, .. } => {
            assert_eq!(data_type.as_deref(), Some("IFCBOOLEAN"));
            assert_eq!(*uri, None);
        }
        f => panic!("{f:?}"),
    }
    let kinds: Vec<(&str, FacetCardinality)> = s.requirements.iter().map(|r| (r.facet.kind(), r.cardinality)).collect();
    assert_eq!(
        kinds,
        vec![
            ("material", FacetCardinality::Optional),
            ("attribute", FacetCardinality::Prohibited),
            ("classification", FacetCardinality::Required),
            ("property", FacetCardinality::Required),
            ("part_of", FacetCardinality::Prohibited),
        ]
    );
    assert_eq!(s.requirements[1].instructions.as_deref(), Some("no"));
    match &s.requirements[0].facet {
        Facet::Material { uri, .. } => assert_eq!(uri.as_deref(), Some("https://x/y")),
        f => panic!("{f:?}"),
    }
    match &s.requirements[3].facet {
        Facet::Property { value: Some(Val::Restriction(r)), .. } => {
            assert_eq!(r.base, XsdBase::Double);
            assert_eq!(r.min_inclusive.as_deref(), Some("0.1"));
            assert_eq!(r.max_exclusive.as_deref(), Some("1"));
        }
        f => panic!("{f:?}"),
    }
    // Canonical projection is deterministic and carries the schema id.
    let j = d.to_canonical_json();
    assert!(j.contains(r#""schema":"ifcfast.ids.canonical/1""#));
    assert_eq!(j, parse(&x).map(|d| d.to_canonical_json()).unwrap_or_default());
}

#[test]
fn ids_spec_cardinality_combinations() {
    let card = |attrs: &str| parse(&spec(attrs, WALL, "")).map(|d| d.specs[0].cardinality);
    assert_eq!(card(REQ).ok(), Some(SpecCardinality::Required));
    assert_eq!(card(r#"minOccurs="0" maxOccurs="unbounded""#).ok(), Some(SpecCardinality::Optional));
    assert_eq!(card(r#"minOccurs="0" maxOccurs="0""#).ok(), Some(SpecCardinality::Prohibited));
    // Absent attributes: xs:occurs defaults 1 / 1 (xmlschema fills them for IfcTester too).
    assert_eq!(card("").ok(), Some(SpecCardinality::Required));
    assert_eq!(card(r#"maxOccurs="unbounded""#).ok(), Some(SpecCardinality::Required));
    assert_eq!(card(r#"minOccurs="0""#).ok(), Some(SpecCardinality::Optional));
    let raw = parse(&spec("", WALL, "")).map(|d| (d.specs[0].min_occurs, d.specs[0].max_occurs));
    assert_eq!(raw.ok(), Some((1, Some(1))));
    for bad in [
        r#"minOccurs="2" maxOccurs="unbounded""#,
        r#"minOccurs="1" maxOccurs="0""#,
        r#"maxOccurs="0""#, // min defaults to 1
        r#"minOccurs="0" maxOccurs="3""#,
        r#"minOccurs="2""#,
    ] {
        assert_invalid(&spec(bad, WALL, ""), "is not an IDS 1.0 specification cardinality");
    }
    assert_invalid(&spec(r#"minOccurs="one""#, WALL, ""), "minOccurs");
    assert_invalid(&spec(r#"maxOccurs="-1""#, WALL, ""), "maxOccurs");
}

#[test]
fn ids_rejects_prohibited_spec_with_requirements() {
    let attr = "<attribute><name><simpleValue>Name</simpleValue></name></attribute>";
    assert_invalid(&spec(r#"minOccurs="0" maxOccurs="0""#, WALL, attr), "prohibited specification");
    // An empty <requirements/> on a prohibited spec is fine.
    assert!(parse(&spec(r#"minOccurs="0" maxOccurs="0""#, WALL, "")).is_ok());
}

#[test]
fn ids_rejects_bad_namespace_and_root() {
    let no_ns = doc("").replace(r#"xmlns="http://standards.buildingsmart.org/IDS""#, "");
    assert_invalid(&no_ns, "root element must be <ids>");
    let wrong_ns = doc("").replace(
        "http://standards.buildingsmart.org/IDS\" xmlns:xs",
        "http://standards.buildingsmart.org/IDS/0.9\" xmlns:xs",
    );
    assert_invalid(&wrong_ns, "root element must be <ids>");
    assert_invalid("<ids", "not well-formed XML");
    assert_invalid(
        r#"<!DOCTYPE ids [<!ENTITY a "b">]><ids xmlns="http://standards.buildingsmart.org/IDS"/>"#,
        "not well-formed XML",
    );
}

#[test]
fn ids_rejects_structure_violations() {
    let ok_spec = r#"<specification name="S" ifcVersion="IFC4"><applicability>"#.to_string() + WALL + "</applicability></specification>";
    // unknown element
    assert_invalid(&doc(&ok_spec.replace("</applicability>", "<foo/></applicability>")), "unexpected element <foo>");
    // unknown attribute
    assert_invalid(&doc(&ok_spec.replace("<applicability>", r#"<applicability color="red">"#)), "unexpected attribute 'color'");
    assert_invalid(&doc(&ok_spec.replace("<entity>", r#"<entity instructions="x">"#)), "unexpected attribute 'instructions'");
    assert_invalid(&doc(&ok_spec.replace("<entity>", r#"<entity cardinality="optional">"#)), "unexpected attribute 'cardinality'");
    assert_invalid(&doc(&ok_spec.replace("<applicability>", r#"<applicability xsi:type="x">"#)), "xsi:type");
    // missing required element / attribute
    assert_invalid(&doc(""), "missing required element <specification>");
    assert_invalid(&doc(r#"<specification name="S" ifcVersion="IFC4"></specification>"#), "missing required element <applicability>");
    assert_invalid(&doc(&ok_spec.replace(r#" name="S""#, "")), "'name'");
    assert_invalid(&doc(&ok_spec.replace(r#" ifcVersion="IFC4""#, "")), "'ifcVersion'");
    assert_invalid(&(HEAD.replace("<info><title>T</title></info>", "<info></info>") + TAIL), "missing required element <title>");
    assert_invalid(&(HEAD.replace("<title>T</title>", "") + TAIL), "missing required element <title>");
    // out of XSD order in applicability (entity must come first)
    let prop = "<property><propertySet><simpleValue>P</simpleValue></propertySet><baseName><simpleValue>B</simpleValue></baseName></property>";
    assert_invalid(&spec(REQ, &format!("{prop}{WALL}"), ""), "expected, in this order");
    // two entities in applicability
    assert_invalid(&spec(REQ, &format!("{WALL}{WALL}"), ""), "at most 1");
    // classification without the required <system>
    assert_invalid(&spec(REQ, WALL, "<classification><value><simpleValue>X</simpleValue></value></classification>"), "missing required element <system>");
    // idsValue with both / neither child
    assert_invalid(&spec(REQ, "<entity><name></name></entity>", ""), "exactly one");
    assert_invalid(
        &spec(REQ, "<entity><name><simpleValue>IFCWALL</simpleValue><simpleValue>IFCSLAB</simpleValue></name></entity>", ""),
        "exactly one",
    );
    // stray text in element-only content
    assert_invalid(&spec(REQ, &format!("hello{WALL}"), ""), "unexpected text 'hello'");
    // element inside simpleValue
    assert_invalid(&spec(REQ, "<entity><name><simpleValue><b/>IFCWALL</simpleValue></name></entity>", ""), "text only");
}

#[test]
fn ids_rejects_bad_tokens() {
    let v = |ver: &str| doc(&format!(r#"<specification name="S" ifcVersion="{ver}"><applicability>{WALL}</applicability></specification>"#));
    assert_invalid(&v("IFC4X3"), "IFC4X3");
    assert_invalid(&v("ifc4"), "ifc4");
    assert_invalid(&v(""), "ifcVersion is empty");
    assert!(parse(&v(" IFC2X3\tIFC4 ")).is_ok());
    let part = |attr: &str| spec(REQ, WALL, &format!("<partOf {attr}><entity><name><simpleValue>IFCBUILDING</simpleValue></name></entity></partOf>"));
    assert_invalid(&part(r#"relation="IFCRELVOIDSELEMENT""#), "relation 'IFCRELVOIDSELEMENT'");
    assert_invalid(&part(r#"relation="ifcrelaggregates""#), "relation");
    assert_invalid(&part(r#"cardinality="optional""#), "cardinality 'optional' is not allowed on <partOf>");
    assert!(parse(&part(r#"relation="IFCRELNESTS" cardinality="prohibited""#)).is_ok());
    let attr_card = |c: &str| spec(REQ, WALL, &format!(r#"<attribute cardinality="{c}"><name><simpleValue>Name</simpleValue></name></attribute>"#));
    assert_invalid(&attr_card("Required"), "cardinality 'Required'");
    assert_invalid(&attr_card("mandatory"), "cardinality 'mandatory'");
    let dt = |d: &str| spec(REQ, WALL, &format!(r#"<property dataType="{d}"><propertySet><simpleValue>P</simpleValue></propertySet><baseName><simpleValue>B</simpleValue></baseName></property>"#));
    assert_invalid(&dt("IfcLabel"), "dataType 'IfcLabel'");
    assert_invalid(&dt("IFC LABEL"), "dataType");
    assert_invalid(&dt(""), "dataType");
    assert!(parse(&dt("IFCLABEL")).is_ok());
    // info author / date formats
    let info = |inner: &str| HEAD.replace("<info><title>T</title></info>", &format!("<info><title>T</title>{inner}</info>")) + &spec_body() + TAIL;
    assert_invalid(&info("<author>not-an-email</author>"), "author");
    assert_invalid(&info("<date>24.09.2026</date>"), "xs:date");
    assert!(parse(&info("<author>a@b.no</author><date>2026-09-24</date>")).is_ok());
    // info out of order
    assert_invalid(&info("<date>2026-09-24</date><author>a@b.no</author>"), "expected, in this order");
}

fn spec_body() -> String {
    format!(r#"<specification name="S" ifcVersion="IFC4"><applicability>{WALL}</applicability></specification>"#)
}

fn restr(body: &str) -> String {
    spec(REQ, WALL, &format!("<attribute><name><simpleValue>Name</simpleValue></name><value>{body}</value></attribute>"))
}

#[test]
fn ids_restriction_parsing_and_rejections() {
    let d = parse(&restr(
        r#"<xs:restriction base="xs:string"><xs:annotation><xs:documentation>x</xs:documentation></xs:annotation>
           <xs:enumeration value="A"/><xs:enumeration value="B"/><xs:pattern value="[AB]"/><xs:pattern value="C"/>
           <xs:minLength value="1"/><xs:maxLength value="+2"/></xs:restriction>"#,
    ))
    .unwrap_or_else(|e| panic!("{e}"));
    match &d.specs[0].requirements[0].facet {
        Facet::Attribute { value: Some(Val::Restriction(r)), .. } => {
            assert_eq!(r.base, XsdBase::String);
            assert_eq!(r.enumeration, Some(vec!["A".to_string(), "B".to_string()]));
            assert_eq!(r.patterns, Some(vec!["[AB]".to_string(), "C".to_string()]));
            assert_eq!((r.min_length, r.max_length, r.length), (Some(1), Some(2), None));
        }
        f => panic!("{f:?}"),
    }
    // A different prefix bound to the XSD namespace is fine.
    let other_prefix = restr(r#"<q:restriction xmlns:q="http://www.w3.org/2001/XMLSchema" base="q:integer"><q:minInclusive value="3"/></q:restriction>"#);
    assert!(parse(&other_prefix).is_ok());

    assert_invalid(&restr(r#"<xs:restriction base="xs:string"/>"#), "has no facets");
    assert_invalid(&restr(r#"<xs:restriction base="xs:decimal"><xs:minInclusive value="1"/></xs:restriction>"#), "not an IDS 1.0 restriction base");
    assert_invalid(&restr(r#"<xs:restriction base="foo:string"><xs:length value="1"/></xs:restriction>"#), "@base");
    assert_invalid(&restr(r#"<xs:restriction base="xs:string"><xs:length value="1"/><xs:length value="2"/></xs:restriction>"#), "at most once");
    assert_invalid(&restr(r#"<xs:restriction base="xs:double"><xs:minInclusive value="1"/><xs:minInclusive value="2"/></xs:restriction>"#), "at most once");
    assert_invalid(&restr(r#"<xs:restriction base="xs:string"><xs:length value="-1"/></xs:restriction>"#), "non-negative integer");
    assert_invalid(&restr(r#"<xs:restriction base="xs:string"><xs:length/></xs:restriction>"#), "'value'");
    assert_invalid(&restr(r#"<xs:restriction base="xs:string"><xs:length value="1" fixed="true"/></xs:restriction>"#), "'fixed'");
    assert_invalid(&restr(r#"<xs:restriction base="xs:string"><xs:foo value="1"/></xs:restriction>"#), "not an xs:restriction facet");
    assert_invalid(&restr(r#"<xs:restriction base="xs:double"><xs:minInclusive value="1,5"/></xs:restriction>"#), "not a valid xs:double literal");
    assert_invalid(&restr(r#"<xs:restriction base="xs:integer"><xs:enumeration value="42.0"/></xs:restriction>"#), "not a valid xs:integer literal");
    assert_invalid(&restr(r#"<xs:restriction base="xs:string"><xs:minInclusive value="1"/></xs:restriction>"#), "not allowed on base xs:string");
    assert_invalid(&restr(r#"<xs:restriction base="xs:double"><xs:length value="1"/></xs:restriction>"#), "not allowed on base xs:double");
    assert_invalid(&restr(r#"<xs:restriction base="xs:boolean"><xs:enumeration value="true"/></xs:restriction>"#), "not allowed on base xs:boolean");
    assert_invalid(&restr(r#"<xs:restriction base="xs:string"><xs:pattern value="a*?"/></xs:restriction>"#), "quantifier");
    assert_invalid(&restr(r#"<xs:restriction base="xs:string"><xs:pattern value="(a)\1"/></xs:restriction>"#), "not an XSD escape");
    // Valid-but-unimplemented constructs.
    assert!(matches!(
        parse(&restr(r#"<xs:restriction base="xs:double"><xs:totalDigits value="3"/></xs:restriction>"#)),
        Err(IdsError::Unsupported { ref feature, .. }) if feature == "xsd-restriction:totalDigits"
    ));
    // An untranslated Unicode block parses; `compile` reports it per spec.
    assert!(parse(&restr(r#"<xs:restriction base="xs:string"><xs:pattern value="\p{IsThai}+"/></xs:restriction>"#)).is_ok());
    // Bounds on temporal bases parse (evaluation is Unsupported for now).
    assert!(parse(&restr(r#"<xs:restriction base="xs:date"><xs:minInclusive value="2020-01-01"/></xs:restriction>"#)).is_ok());
    assert_invalid(&restr(r#"<xs:restriction base="xs:date"><xs:minInclusive value="01.01.2020"/></xs:restriction>"#), "xs:date");
}

#[test]
fn ids_audit_rules_via_parse() {
    let prop = |dt: &str, v: &str| {
        spec(REQ, WALL, &format!(r#"<property dataType="{dt}"><propertySet><simpleValue>P</simpleValue></propertySet><baseName><simpleValue>B</simpleValue></baseName><value>{v}</value></property>"#))
    };
    let sv = |s: &str| format!("<simpleValue>{s}</simpleValue>");
    assert_invalid(&prop("IFCBOOLEAN", &sv("TRUE")), "not a valid IFCBOOLEAN literal");
    assert!(parse(&prop("IFCBOOLEAN", &sv("true"))).is_ok());
    assert_invalid(&prop("IFCINTEGER", &sv("42.")), "IFCINTEGER");
    assert_invalid(&prop("IFCCOUNTMEASURE", &sv("1e3")), "IFCCOUNTMEASURE");
    assert_invalid(&prop("IFCREAL", &sv("42,3")), "IFCREAL");
    assert!(parse(&prop("IFCREAL", &sv("1.2345E3"))).is_ok());
    assert!(parse(&prop("IFCLABEL", &sv("42,3"))).is_ok());
    assert_invalid(
        &prop("IFCREAL", r#"<xs:restriction base="xs:string"><xs:pattern value=".*"/></xs:restriction>"#),
        "does not match dataType IFCREAL",
    );
    // Entity names are uppercase.
    assert_invalid(&spec(REQ, "<entity><name><simpleValue>IfcWall</simpleValue></name></entity>", ""), "uppercase");
    // Requirement entity must be able to match the applicability entity.
    let req_ent = |n: &str| spec(REQ, WALL, &format!("<entity><name>{n}</name></entity>"));
    assert_invalid(&req_ent(&sv("IFCSLAB")), "can never match the applicability entity 'IFCWALL'");
    assert!(parse(&req_ent(&sv("IFCWALL"))).is_ok());
    assert!(parse(&req_ent(r#"<xs:restriction base="xs:string"><xs:pattern value="IFCWALL.*"/></xs:restriction>"#)).is_ok());
}

#[test]
fn ids_encodings() {
    let x = doc(&spec_body());
    // UTF-8 BOM
    let mut bom = vec![0xEF, 0xBB, 0xBF];
    bom.extend_from_slice(x.as_bytes());
    assert!(parse_ids(&bom).is_ok());
    // UTF-16 LE with BOM (declaration still says utf-8; roxmltree ignores it)
    let mut le = vec![0xFF, 0xFE];
    for u in x.encode_utf16() {
        le.extend_from_slice(&u.to_le_bytes());
    }
    assert!(parse_ids(&le).is_ok());
    // Latin-1 bytes with a Latin-1 declaration: legal XML we do not decode.
    let latin = x
        .replace(r#"encoding="utf-8""#, r#"encoding="ISO-8859-1""#)
        .replace("<title>T</title>", "<title>Bl\u{e5}</title>");
    let bytes: Vec<u8> = latin.chars().map(|c| c as u32 as u8).collect();
    assert!(matches!(parse_ids(&bytes), Err(IdsError::Unsupported { ref feature, .. }) if feature == "xml-encoding:ISO-8859-1"));
    // Norwegian text round-trips.
    let no = x.replace("<title>T</title>", "<title>Bæreevne på dør</title>");
    assert_eq!(parse(&no).map(|d| d.info.title).ok().as_deref(), Some("Bæreevne på dør"));
}

#[test]
fn ids_error_carries_line_number() {
    let x = spec(REQ, WALL, "<attribute cardinality=\"bogus\"><name><simpleValue>Name</simpleValue></name></attribute>");
    let (_, line, _) = invalid(&x);
    assert_eq!(line, Some(5), "the <specification> line of the fixture");
}

// --------------------------------------------------------------------------
// buildingSMART conformance fixtures (tests/fixtures/ids, verbatim, CC BY-ND 4.0)
// --------------------------------------------------------------------------

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ids")
}

/// `invalid-*` cases the parser rejects without IFC schema knowledge.
/// The other 14 (`attribute/*`, `partof/*`, `restriction/*`) need the
/// schema and belong to `compile`.
const SCHEMA_FREE_INVALID: &[&str] = &[
    "entity/invalid-an_entity_not_matching_the_specified_class_should_fail.ids",
    "entity/invalid-entities_can_be_specified_as_a_xsd_regex_pattern_1_2.ids",
    "entity/invalid-entities_can_be_specified_as_an_enumeration_3_3.ids",
    "entity/invalid-entities_must_be_specified_as_uppercase_strings.ids",
    "entity/invalid-invalid_entities_always_fail.ids",
    "entity/invalid-subclasses_are_not_considered_as_matching.ids",
    "ids/invalid-prohibited_specifications_invalid_if_requirements_are_specified.ids",
    "property/invalid-booleans_must_be_specified_as_lowercase_strings_3_3.ids",
    "property/invalid-integer_values_are_checked_using_type_casting_4_4.ids",
    "property/invalid-integer_values_cannot_be_stored_with_decimal_2_4.ids",
    "property/invalid-integer_values_cannot_be_stored_with_decimal_3_4.ids",
    "property/invalid-only_specifically_formatted_numbers_are_allowed_1_4.ids",
    "property/invalid-only_specifically_formatted_numbers_are_allowed_2_4.ids",
];

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "ids") {
            out.push(p);
        }
    }
}

/// Check every `.ids` under `root`: schema-free invalid cases must be
/// `InvalidIds`, everything else must parse. Returns (checked, rejected).
fn check_suite(root: &Path) -> (usize, usize) {
    let mut files = Vec::new();
    walk(root, &mut files);
    files.sort();
    let mut rejected = 0;
    for f in &files {
        let rel = f.strip_prefix(root).map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_default();
        let bytes = std::fs::read(f).unwrap_or_else(|e| panic!("read {rel}: {e}"));
        let res = parse_ids(&bytes);
        // Dev aid for the parse differential: dump the canonical JSON.
        if let (Some(dir), Ok(d)) = (std::env::var_os("IFCFAST_IDS_CANON_DUMP"), &res) {
            let out = PathBuf::from(dir).join(format!("{rel}.json"));
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent).unwrap_or_else(|e| panic!("{e}"));
            }
            std::fs::write(&out, d.to_canonical_json()).unwrap_or_else(|e| panic!("{e}"));
        }
        if SCHEMA_FREE_INVALID.contains(&rel.as_str()) {
            match res {
                Err(IdsError::InvalidIds { .. }) => rejected += 1,
                other => panic!("{rel}: expected InvalidIds, got {other:?}"),
            }
        } else if let Err(e) = res {
            panic!("{rel}: should parse, got {e}");
        }
    }
    (files.len(), rejected)
}

#[test]
fn ids_conformance_fixtures() {
    let (n, rejected) = check_suite(&fixtures());
    assert_eq!(n, 27, "vendored fixture count");
    assert_eq!(rejected, SCHEMA_FREE_INVALID.len());
}

/// The whole pinned suite, when it is on disk (`scripts/fetch_ids_testcases.py`).
#[test]
fn ids_conformance_full_suite_if_present() {
    let root = std::env::var_os("IFCFAST_IDS_TESTCASES").map(PathBuf::from).or_else(|| {
        std::env::var_os("HOME").map(|h| {
            PathBuf::from(h).join(".cache/ifcfast/ids-testcases/a67047736aa93586d723329fce3aab1b9ac056af")
        })
    });
    let Some(root) = root.filter(|r| r.is_dir()) else {
        eprintln!("ids conformance suite not on disk; skipping full-suite parse check");
        return;
    };
    let (n, rejected) = check_suite(&root);
    eprintln!("ids full suite: {n} .ids parsed, {rejected} schema-free invalid rejected");
    assert_eq!(n, 334);
    assert_eq!(rejected, SCHEMA_FREE_INVALID.len());
}
