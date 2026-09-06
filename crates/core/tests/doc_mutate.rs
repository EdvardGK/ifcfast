//! GH #133: attribute mutation on the owned STEP document.
//!
//! The synthetic fixture is deliberately built around SHARING — the
//! failure mode that matters: a pset applying to two walls through one
//! rel, a pset shared by two rels, a property record shared by two
//! psets (arises mid-test via CoW), and a LocalPlacement shared by two
//! products. Every test asserts the *other* element's view is
//! untouched, and the no-dangling-ref invariant holds on the output.

use std::collections::HashSet;

use _core::doc::{forward_refs, mutate, Doc, MutateOp, PropValue};
use _core::lexer::{decode_string, parse_field, parse_record_span, split_top_level_args, Field};

const GUID_A: &str = "WallAGuid00000000000A";
const GUID_B: &str = "WallBGuid00000000000B";
const GUID_C: &str = "WallCGuid00000000000C";

fn fixture() -> String {
    let mut s = String::new();
    s.push_str("ISO-10303-21;\nHEADER;\n");
    s.push_str("FILE_DESCRIPTION((''),'2;1');\n");
    s.push_str("FILE_NAME('t','',(''),(''),'','','');\n");
    s.push_str("FILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n");
    // Placement tree: #12 is the shared parent; walls A/B have their own
    // leaf placements; wall C SHARES wall B's LocalPlacement #32.
    s.push_str("#10=IFCCARTESIANPOINT((0.,0.,0.));\n");
    s.push_str("#11=IFCAXIS2PLACEMENT3D(#10,$,$);\n");
    s.push_str("#12=IFCLOCALPLACEMENT($,#11);\n");
    s.push_str("#20=IFCCARTESIANPOINT((1.,2.,3.));\n");
    s.push_str("#21=IFCAXIS2PLACEMENT3D(#20,$,$);\n");
    s.push_str("#22=IFCLOCALPLACEMENT(#12,#21);\n");
    s.push_str("#30=IFCCARTESIANPOINT((5.,0.,0.));\n");
    s.push_str("#31=IFCAXIS2PLACEMENT3D(#30,$,$);\n");
    s.push_str("#32=IFCLOCALPLACEMENT(#12,#31);\n");
    s.push_str(&format!(
        "#40=IFCWALL('{GUID_A}',$,'Wall A',$,$,#22,$,$,$);\n"
    ));
    s.push_str(&format!(
        "#41=IFCWALL('{GUID_B}',$,'Wall B',$,$,#32,$,$,$);\n"
    ));
    s.push_str(&format!(
        "#42=IFCWALL('{GUID_C}',$,'Wall C',$,$,#32,$,$,$);\n"
    ));
    // Pset_WallCommon: ONE rel anchoring BOTH walls (anchor-shared).
    s.push_str("#50=IFCPROPERTYSINGLEVALUE('FireRating',$,IFCLABEL('REI30'),$);\n");
    s.push_str("#51=IFCPROPERTYSINGLEVALUE('LoadBearing',$,IFCBOOLEAN(.T.),$);\n");
    s.push_str("#52=IFCPROPERTYSET('PsetWCGuid00000000000',$,'Pset_WallCommon',$,(#50,#51));\n");
    s.push_str("#53=IFCRELDEFINESBYPROPERTIES('RelWCGuid000000000000',$,$,$,(#40,#41),#52);\n");
    // Pset_Custom: ONE pset, TWO rels (pset-record-shared).
    s.push_str("#60=IFCPROPERTYSINGLEVALUE('Comment',$,IFCTEXT('hei'),$);\n");
    s.push_str("#61=IFCPROPERTYSET('PsetCuGuid00000000000',$,'Pset_Custom',$,(#60));\n");
    s.push_str("#62=IFCRELDEFINESBYPROPERTIES('RelCuAGuid00000000000',$,$,$,(#40),#61);\n");
    s.push_str("#63=IFCRELDEFINESBYPROPERTIES('RelCuBGuid00000000000',$,$,$,(#41),#61);\n");
    // Pset_Solo: unshared — the minimal-diff in-place path.
    s.push_str("#64=IFCPROPERTYSINGLEVALUE('Solo',$,IFCREAL(1.),$);\n");
    s.push_str("#65=IFCPROPERTYSET('PsetSoGuid00000000000',$,'Pset_Solo',$,(#64));\n");
    s.push_str("#66=IFCRELDEFINESBYPROPERTIES('RelSoGuid000000000000',$,$,$,(#40),#65);\n");
    // A quantity set with a colliding-style name: the type guard target.
    s.push_str("#70=IFCQUANTITYLENGTH('Length',$,$,4.);\n");
    s.push_str("#71=IFCELEMENTQUANTITY('QtoGuid00000000000000',$,'Qto_WallBase',$,$,(#70));\n");
    s.push_str("#72=IFCRELDEFINESBYPROPERTIES('RelQtGuid000000000000',$,$,$,(#40),#71);\n");
    s.push_str("ENDSEC;\nEND-ISO-10303-21;\n");
    s
}

fn open() -> Doc {
    Doc::from_bytes(fixture().into_bytes())
}

// ---- traversal helpers (read the OUTPUT like a consumer would) --------

fn field_raw(doc: &Doc, id: u64, idx: usize) -> Vec<u8> {
    let span = doc.record_bytes(id).expect("record");
    let (_i, _t, args) = parse_record_span(span).expect("parse");
    split_top_level_args(args)[idx].to_vec()
}

fn field_str(doc: &Doc, id: u64, idx: usize) -> String {
    match parse_field(&field_raw(doc, id, idx)) {
        Field::String(s) => s,
        f => panic!("field {idx} of #{id} not a string: {f:?}"),
    }
}

fn type_of(doc: &Doc, id: u64) -> String {
    let span = doc.record_bytes(id).expect("record");
    let (_i, t, _a) = parse_record_span(span).expect("parse");
    String::from_utf8(t.to_ascii_uppercase()).unwrap()
}

fn refs_of_field(doc: &Doc, id: u64, idx: usize) -> Vec<u64> {
    _core::lexer::scan_ref_tokens(&field_raw(doc, id, idx))
}

/// The psets attached to `elem` (by step id) as `(pset_id, name)`.
fn psets_of(doc: &Doc, elem: u64) -> Vec<(u64, String)> {
    let mut out = Vec::new();
    for &id in doc.ids() {
        if type_of(doc, id) != "IFCRELDEFINESBYPROPERTIES" {
            continue;
        }
        if !refs_of_field(doc, id, 4).contains(&elem) {
            continue;
        }
        for pd in refs_of_field(doc, id, 5) {
            if type_of(doc, pd) == "IFCPROPERTYSET" {
                out.push((pd, field_str(doc, pd, 2)));
            }
        }
    }
    out
}

/// The raw NominalValue bytes of property `name` in pset `pset`.
fn prop_value(doc: &Doc, pset: u64, name: &str) -> String {
    for p in refs_of_field(doc, pset, 4) {
        if field_str(doc, p, 0) == name {
            return String::from_utf8(field_raw(doc, p, 2)).unwrap();
        }
    }
    panic!("property {name} not in #{pset}");
}

fn resolve(doc: &Doc, guid: &str) -> u64 {
    let (found, missing) = doc.resolve_guids(&[guid.to_string()]);
    assert!(missing.is_empty(), "unknown guid {guid}");
    found[0]
}

fn assert_no_dangling(doc: &Doc) {
    for &id in doc.ids() {
        for r in forward_refs(doc, id) {
            assert!(doc.contains(r), "dangling ref #{r} from #{id}");
        }
    }
}

fn location_of(doc: &Doc, guid: &str) -> [f64; 3] {
    let elem = resolve(doc, guid);
    let lp = refs_of_field(doc, elem, 5)[0];
    let a2p = refs_of_field(doc, lp, 1)[0];
    let pt = refs_of_field(doc, a2p, 0)[0];
    let raw = field_raw(doc, pt, 0);
    let Field::List(body) = parse_field(&raw) else {
        panic!("not a list")
    };
    let parts = split_top_level_args(body);
    let mut out = [0.0; 3];
    for (i, p) in parts.iter().enumerate() {
        let Field::Number(n) = parse_field(p) else {
            panic!("coord")
        };
        out[i] = n;
    }
    out
}

// ---- tests -------------------------------------------------------------

#[test]
fn empty_batch_is_byte_identical() {
    let doc = open();
    let (bytes, stats) = mutate(&doc, &[], Some(1)).expect("empty batch");
    assert_eq!(bytes, fixture().into_bytes());
    assert_eq!(stats.records_minted, 0);
}

#[test]
fn rename_norwegian_roundtrip_and_bytes_identical_elsewhere() {
    let doc = open();
    let ops = [MutateOp::Rename {
        guid: GUID_A.to_string(),
        name: Some("Vegg Æ blåbær".to_string()),
        description: Some("søylefri sone".to_string()),
    }];
    let (bytes, stats) = mutate(&doc, &ops, Some(1)).expect("rename");
    assert_eq!(stats.renamed, 1);

    let out = Doc::from_bytes(bytes);
    assert_no_dangling(&out);
    let a = resolve(&out, GUID_A);
    assert_eq!(field_str(&out, a, 2), "Vegg Æ blåbær");
    assert_eq!(field_str(&out, a, 3), "søylefri sone");
    // Every OTHER record byte-identical to the source.
    for &id in out.ids() {
        if id == a {
            continue;
        }
        assert_eq!(
            out.record_bytes(id),
            doc.record_bytes(id),
            "#{id} changed by a rename of #{a}"
        );
    }
}

#[test]
fn set_property_unshared_pset_edits_in_place() {
    let doc = open();
    let ops = [MutateOp::SetProperty {
        guid: GUID_A.to_string(),
        pset: "Pset_Solo".to_string(),
        prop: "Solo".to_string(),
        value: PropValue::Real(2.5),
        ifc_type: None,
    }];
    let (bytes, stats) = mutate(&doc, &ops, Some(1)).expect("set");
    assert_eq!(stats.props_set, 1);
    assert_eq!(stats.psets_cloned, 0, "unshared pset must not clone");
    assert_eq!(stats.records_minted, 0, "in-place edit mints nothing");

    let out = Doc::from_bytes(bytes);
    assert_no_dangling(&out);
    assert_eq!(prop_value(&out, 65, "Solo"), "IFCREAL(2.5)");
}

#[test]
fn set_property_anchor_shared_pset_cows_and_other_wall_keeps_value() {
    let doc = open();
    let ops = [MutateOp::SetProperty {
        guid: GUID_A.to_string(),
        pset: "Pset_WallCommon".to_string(),
        prop: "FireRating".to_string(),
        value: PropValue::Str("REI60".to_string()),
        ifc_type: None,
    }];
    let (bytes, stats) = mutate(&doc, &ops, Some(1)).expect("set");
    assert_eq!(stats.psets_cloned, 1);
    assert_eq!(stats.rels_cloned, 1);

    let out = Doc::from_bytes(bytes);
    assert_no_dangling(&out);

    // Wall A sees the new value through its own (cloned) pset…
    let a = resolve(&out, GUID_A);
    let a_psets = psets_of(&out, a);
    let (a_pset, _) = a_psets
        .iter()
        .find(|(_, n)| n == "Pset_WallCommon")
        .expect("wall A kept Pset_WallCommon");
    assert_eq!(prop_value(&out, *a_pset, "FireRating"), "IFCLABEL('REI60')");
    assert_ne!(*a_pset, 52, "wall A must be on a clone, not the original");
    // …and untouched siblings ride along on the clone.
    assert_eq!(prop_value(&out, *a_pset, "LoadBearing"), "IFCBOOLEAN(.T.)");

    // Wall B still sees the ORIGINAL pset with the original value.
    let b = resolve(&out, GUID_B);
    let b_psets = psets_of(&out, b);
    let (b_pset, _) = b_psets
        .iter()
        .find(|(_, n)| n == "Pset_WallCommon")
        .expect("wall B kept Pset_WallCommon");
    assert_eq!(*b_pset, 52);
    assert_eq!(prop_value(&out, *b_pset, "FireRating"), "IFCLABEL('REI30')");

    // The clone's GlobalId is fresh (not the original's) and valid.
    let clone_guid = field_str(&out, *a_pset, 0);
    assert_ne!(clone_guid, field_str(&out, 52, 0));
    assert!(_core::guid::decode_guid(&clone_guid).is_some());
}

#[test]
fn set_property_rel_shared_pset_cows_without_rel_clone() {
    let doc = open();
    let ops = [MutateOp::SetProperty {
        guid: GUID_A.to_string(),
        pset: "Pset_Custom".to_string(),
        prop: "Comment".to_string(),
        value: PropValue::Str("ha det".to_string()),
        ifc_type: None,
    }];
    let (bytes, stats) = mutate(&doc, &ops, Some(1)).expect("set");
    assert_eq!(stats.psets_cloned, 1);
    assert_eq!(stats.rels_cloned, 0, "anchor was already just wall A");

    let out = Doc::from_bytes(bytes);
    assert_no_dangling(&out);
    let a = resolve(&out, GUID_A);
    let (a_pset, _) = psets_of(&out, a)
        .into_iter()
        .find(|(_, n)| n == "Pset_Custom")
        .expect("wall A kept Pset_Custom");
    assert_eq!(prop_value(&out, a_pset, "Comment"), "IFCTEXT('ha det')");
    // Wall B still routes to the original #61 with the original value.
    let b = resolve(&out, GUID_B);
    let (b_pset, _) = psets_of(&out, b)
        .into_iter()
        .find(|(_, n)| n == "Pset_Custom")
        .expect("wall B kept Pset_Custom");
    assert_eq!(b_pset, 61);
    assert_eq!(prop_value(&out, b_pset, "Comment"), "IFCTEXT('hei')");
}

#[test]
fn quantity_set_is_guarded_not_corrupted() {
    let doc = open();
    let ops = [MutateOp::SetProperty {
        guid: GUID_A.to_string(),
        pset: "Qto_WallBase".to_string(),
        prop: "Length".to_string(),
        value: PropValue::Real(9.0),
        ifc_type: None,
    }];
    let err = mutate(&doc, &ops, Some(1)).expect_err("must refuse quantity sets");
    assert!(
        err.failures[0].1.contains("IfcElementQuantity"),
        "error must name the type guard: {}",
        err.failures[0].1
    );
}

#[test]
fn add_property_requires_ifc_type_then_appends() {
    let doc = open();
    let no_type = [MutateOp::SetProperty {
        guid: GUID_A.to_string(),
        pset: "Pset_Solo".to_string(),
        prop: "U-verdi".to_string(),
        value: PropValue::Real(0.18),
        ifc_type: None,
    }];
    let err = mutate(&doc, &no_type, Some(1)).expect_err("new property needs ifc_type");
    assert!(err.failures[0].1.contains("ifc_type"));

    let with_type = [MutateOp::SetProperty {
        guid: GUID_A.to_string(),
        pset: "Pset_Solo".to_string(),
        prop: "U-verdi".to_string(),
        value: PropValue::Real(0.18),
        ifc_type: Some("IFCTHERMALTRANSMITTANCEMEASURE".to_string()),
    }];
    let (bytes, stats) = mutate(&doc, &with_type, Some(1)).expect("add");
    assert_eq!(stats.props_added, 1);
    let out = Doc::from_bytes(bytes);
    assert_no_dangling(&out);
    assert_eq!(
        prop_value(&out, 65, "U-verdi"),
        "IFCTHERMALTRANSMITTANCEMEASURE(0.18)"
    );
    // The existing property is untouched.
    assert_eq!(prop_value(&out, 65, "Solo"), "IFCREAL(1.)");
}

#[test]
fn translate_moves_only_target_and_gcs_old_placement_geometry() {
    let doc = open();
    let ops = [MutateOp::Translate {
        guid: GUID_A.to_string(),
        delta: [10.0, 0.0, -1.0],
    }];
    let (bytes, stats) = mutate(&doc, &ops, Some(1)).expect("translate");
    assert_eq!(stats.translated, 1);
    assert_eq!(stats.placements_cloned, 0, "wall A owns its placement");
    // Old A2P3D #21 + point #20 are uniquely owned → reclaimed.
    assert_eq!(stats.records_gc, 2);

    let out = Doc::from_bytes(bytes);
    assert_no_dangling(&out);
    assert!(!out.contains(21), "#21 must be GC'd");
    assert!(!out.contains(20), "#20 must be GC'd");
    assert_eq!(location_of(&out, GUID_A), [11.0, 2.0, 2.0]);
    assert_eq!(location_of(&out, GUID_B), [5.0, 0.0, 0.0]);
}

#[test]
fn translate_shared_local_placement_cows_and_sibling_stays_put() {
    let doc = open();
    // Walls B and C share LocalPlacement #32.
    let ops = [MutateOp::Translate {
        guid: GUID_C.to_string(),
        delta: [0.0, 7.0, 0.0],
    }];
    let (bytes, stats) = mutate(&doc, &ops, Some(1)).expect("translate");
    assert_eq!(stats.placements_cloned, 1);

    let out = Doc::from_bytes(bytes);
    assert_no_dangling(&out);
    assert_eq!(location_of(&out, GUID_C), [5.0, 7.0, 0.0]);
    assert_eq!(location_of(&out, GUID_B), [5.0, 0.0, 0.0]);
    // The shared originals must survive for wall B.
    assert!(out.contains(31), "#31 still used by wall B's placement");
    assert!(out.contains(30), "#30 still used by #31");
}

#[test]
fn rotate_composes_axes_about_own_location() {
    let doc = open();
    let ops = [MutateOp::Rotate {
        guid: GUID_A.to_string(),
        axis: [0.0, 0.0, 2.0], // non-unit on purpose — must normalize
        degrees: 90.0,
    }];
    let (bytes, stats) = mutate(&doc, &ops, Some(1)).expect("rotate");
    assert_eq!(stats.rotated, 1);

    let out = Doc::from_bytes(bytes);
    assert_no_dangling(&out);
    // Location unchanged; axes minted: refdir (1,0,0) → (0,1,0).
    assert_eq!(location_of(&out, GUID_A), [1.0, 2.0, 3.0]);
    let a = resolve(&out, GUID_A);
    let lp = refs_of_field(&out, a, 5)[0];
    let a2p = refs_of_field(&out, lp, 1)[0];
    let rd = refs_of_field(&out, a2p, 2)[0];
    assert_eq!(type_of(&out, rd), "IFCDIRECTION");
    let raw = field_raw(&out, rd, 0);
    let Field::List(body) = parse_field(&raw) else {
        panic!("ratios")
    };
    let parts = split_top_level_args(body);
    let vals: Vec<f64> = parts
        .iter()
        .map(|p| match parse_field(p) {
            Field::Number(n) => n,
            _ => panic!("ratio"),
        })
        .collect();
    assert!(
        vals[0].abs() < 1e-12 && (vals[1] - 1.0).abs() < 1e-12 && vals[2].abs() < 1e-12,
        "refdir after 90° about z must be (0,1,0), got {vals:?}"
    );
}

#[test]
fn translate_then_rotate_compose_in_one_batch() {
    let doc = open();
    let ops = [
        MutateOp::Translate {
            guid: GUID_A.to_string(),
            delta: [1.0, 0.0, 0.0],
        },
        MutateOp::Rotate {
            guid: GUID_A.to_string(),
            axis: [0.0, 0.0, 1.0],
            degrees: 180.0,
        },
    ];
    let (bytes, stats) = mutate(&doc, &ops, Some(1)).expect("compose");
    assert_eq!(stats.translated, 1);
    assert_eq!(stats.rotated, 1);
    let out = Doc::from_bytes(bytes);
    assert_no_dangling(&out);
    // Rotation preserved the translated location (it rotates in place).
    assert_eq!(location_of(&out, GUID_A), [2.0, 2.0, 3.0]);
}

#[test]
fn batch_is_atomic_and_collects_all_failures() {
    let doc = open();
    let ops = [
        MutateOp::Rename {
            guid: GUID_A.to_string(),
            name: Some("ok".to_string()),
            description: None,
        },
        MutateOp::Rename {
            guid: "NoSuchGuid00000000000".to_string(),
            name: Some("x".to_string()),
            description: None,
        },
        MutateOp::SetProperty {
            guid: GUID_A.to_string(),
            pset: "Pset_Missing".to_string(),
            prop: "X".to_string(),
            value: PropValue::Null,
            ifc_type: None,
        },
    ];
    let err = mutate(&doc, &ops, Some(1)).expect_err("two ops fail");
    let idxs: Vec<usize> = err.failures.iter().map(|(i, _)| *i).collect();
    assert_eq!(idxs, vec![1, 2], "both failures reported with op indices");
}

#[test]
fn seeded_mutation_is_deterministic() {
    let doc = open();
    let ops = [MutateOp::SetProperty {
        guid: GUID_A.to_string(),
        pset: "Pset_WallCommon".to_string(),
        prop: "FireRating".to_string(),
        value: PropValue::Str("REI90".to_string()),
        ifc_type: None,
    }];
    let (b1, _) = mutate(&doc, &ops, Some(7)).unwrap();
    let (b2, _) = mutate(&doc, &ops, Some(7)).unwrap();
    assert_eq!(b1, b2);
    let (b3, _) = mutate(&doc, &ops, Some(8)).unwrap();
    assert_ne!(b1, b3, "different seed, different minted GlobalIds");
}

#[test]
fn output_reopens_and_all_guids_unique() {
    // A mixed batch; the output must have no duplicate GlobalIds.
    let doc = open();
    let ops = [
        MutateOp::SetProperty {
            guid: GUID_A.to_string(),
            pset: "Pset_WallCommon".to_string(),
            prop: "FireRating".to_string(),
            value: PropValue::Str("REI120".to_string()),
            ifc_type: None,
        },
        MutateOp::SetProperty {
            guid: GUID_B.to_string(),
            pset: "Pset_Custom".to_string(),
            prop: "Comment".to_string(),
            value: PropValue::Str("takk".to_string()),
            ifc_type: None,
        },
        MutateOp::Translate {
            guid: GUID_B.to_string(),
            delta: [0.0, 0.0, 3.0],
        },
    ];
    let (bytes, _) = mutate(&doc, &ops, None).expect("mixed batch"); // unseeded path
    let out = Doc::from_bytes(bytes);
    assert_no_dangling(&out);
    // Only rooted types carry a GlobalId at field 0 — a property's
    // field 0 is its Name and legitimately repeats across records.
    let rooted = [
        "IFCWALL",
        "IFCPROPERTYSET",
        "IFCRELDEFINESBYPROPERTIES",
        "IFCELEMENTQUANTITY",
    ];
    let mut seen: HashSet<String> = HashSet::new();
    for &id in out.ids() {
        if !rooted.contains(&type_of(&out, id).as_str()) {
            continue;
        }
        let raw = field_raw(&out, id, 0);
        if let Some(g) = decode_string(&raw) {
            assert!(seen.insert(g.clone()), "duplicate GlobalId {g} on #{id}");
        }
    }
}

#[test]
fn property_names_do_not_resolve_as_guids() {
    // GH #132 item 4: any field-0 string used to resolve as a GlobalId —
    // a mistyped seed could silently hit a property Name or a material.
    // The rooted-shape guard must reject them.
    let doc = open();
    let ops = [MutateOp::Rename {
        guid: "FireRating".to_string(), // an IfcPropertySingleValue Name
        name: Some("x".to_string()),
        description: None,
    }];
    let err = mutate(&doc, &ops, Some(1)).expect_err("property name must not resolve");
    assert!(err.failures[0].1.contains("unknown GlobalId"));
    // resolve_guids agrees.
    let (found, missing) = doc.resolve_guids(&["FireRating".to_string()]);
    assert!(found.is_empty());
    assert_eq!(missing, vec!["FireRating".to_string()]);
}

// ---- GH #150: child placements follow their parent -------------------

const GUID_HOST: &str = "HostWallGuid000000001";
const GUID_OPENING: &str = "OpeningGuid0000000002";
const GUID_TWIN: &str = "TwinWallGuid000000003";

/// A wall whose LocalPlacement is the PlacementRelTo of an opening's
/// LocalPlacement — the chain every real void rides on. With
/// `twin_shares_wall_placement`, a second wall also names the wall's
/// placement as its ObjectPlacement (genuine product sharing).
fn fixture_child_placement(twin_shares_wall_placement: bool) -> String {
    let mut s = String::new();
    s.push_str("ISO-10303-21;\nHEADER;\n");
    s.push_str("FILE_DESCRIPTION((''),'2;1');\n");
    s.push_str("FILE_NAME('t','',(''),(''),'','','');\n");
    s.push_str("FILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n");
    // Storey root placement.
    s.push_str("#100=IFCCARTESIANPOINT((0.,0.,0.));\n");
    s.push_str("#101=IFCAXIS2PLACEMENT3D(#100,$,$);\n");
    s.push_str("#102=IFCLOCALPLACEMENT($,#101);\n");
    // Wall placement, relative to the storey.
    s.push_str("#110=IFCCARTESIANPOINT((1.,2.,3.));\n");
    s.push_str("#111=IFCAXIS2PLACEMENT3D(#110,$,$);\n");
    s.push_str("#112=IFCLOCALPLACEMENT(#102,#111);\n");
    // Opening placement, relative to the WALL's placement.
    s.push_str("#120=IFCCARTESIANPOINT((0.5,0.,1.));\n");
    s.push_str("#121=IFCAXIS2PLACEMENT3D(#120,$,$);\n");
    s.push_str("#122=IFCLOCALPLACEMENT(#112,#121);\n");
    s.push_str(&format!(
        "#130=IFCWALL('{GUID_HOST}',$,'Host wall',$,$,#112,$,$,$);\n"
    ));
    s.push_str(&format!(
        "#131=IFCOPENINGELEMENT('{GUID_OPENING}',$,'Void',$,$,#122,$,$);\n"
    ));
    s.push_str("#132=IFCRELVOIDSELEMENT('RelVoidGuid0000000004',$,$,$,#130,#131);\n");
    if twin_shares_wall_placement {
        s.push_str(&format!(
            "#133=IFCWALL('{GUID_TWIN}',$,'Twin wall',$,$,#112,$,$,$);\n"
        ));
    }
    s.push_str("ENDSEC;\nEND-ISO-10303-21;\n");
    s
}

fn point_xyz(doc: &Doc, pt: u64) -> [f64; 3] {
    let raw = field_raw(doc, pt, 0);
    let Field::List(body) = parse_field(&raw) else {
        panic!("not a list")
    };
    let mut out = [0.0; 3];
    for (i, p) in split_top_level_args(body).iter().enumerate() {
        let Field::Number(n) = parse_field(p) else {
            panic!("coord")
        };
        out[i] = n;
    }
    out
}

/// World location of a product: the sum of the placement chain. The
/// fixture uses default axes throughout, so summing is exact.
fn world_location(doc: &Doc, guid: &str) -> [f64; 3] {
    let elem = resolve(doc, guid);
    let mut lp = refs_of_field(doc, elem, 5)[0];
    let mut out = [0.0f64; 3];
    for _ in 0..64 {
        let a2p = refs_of_field(doc, lp, 1)[0];
        let loc = point_xyz(doc, refs_of_field(doc, a2p, 0)[0]);
        for i in 0..3 {
            out[i] += loc[i];
        }
        match refs_of_field(doc, lp, 0).first() {
            Some(&parent) => lp = parent,
            None => return out,
        }
    }
    panic!("placement chain of {guid} did not terminate");
}

#[test]
fn translate_wall_carries_its_opening_along() {
    // GH #150: the wall's LocalPlacement has TWO referrers — the wall and
    // the opening's child placement — but only ONE product referrer, so
    // it must be edited in place and the opening must ride along.
    let doc = Doc::from_bytes(fixture_child_placement(false).into_bytes());
    assert_eq!(world_location(&doc, GUID_HOST), [1.0, 2.0, 3.0]);
    assert_eq!(world_location(&doc, GUID_OPENING), [1.5, 2.0, 4.0]);

    let delta = [10.0, 0.0, -1.0];
    let ops = [MutateOp::Translate {
        guid: GUID_HOST.to_string(),
        delta,
    }];
    let (bytes, stats) = mutate(&doc, &ops, Some(1)).expect("translate");
    assert_eq!(
        stats.placements_cloned, 0,
        "a child placement is not a sharer — the wall's LP is edited in place"
    );

    let out = Doc::from_bytes(bytes);
    assert_no_dangling(&out);
    // The wall still owns #112, and the opening still chains through it.
    let host = resolve(&out, GUID_HOST);
    assert_eq!(refs_of_field(&out, host, 5), vec![112]);
    let opening = resolve(&out, GUID_OPENING);
    assert_eq!(refs_of_field(&out, opening, 5), vec![122]);
    assert_eq!(refs_of_field(&out, 122, 0), vec![112]);
    // Both moved by the same delta.
    assert_eq!(world_location(&out, GUID_HOST), [11.0, 2.0, 2.0]);
    assert_eq!(world_location(&out, GUID_OPENING), [11.5, 2.0, 3.0]);
}

#[test]
fn translate_wall_sharing_placement_with_twin_still_cows() {
    // The same chain, but a second WALL shares the placement: product
    // sharing is real, so CoW must still fire and the twin stay put.
    let doc = Doc::from_bytes(fixture_child_placement(true).into_bytes());
    let ops = [MutateOp::Translate {
        guid: GUID_HOST.to_string(),
        delta: [10.0, 0.0, -1.0],
    }];
    let (bytes, stats) = mutate(&doc, &ops, Some(1)).expect("translate");
    assert_eq!(stats.placements_cloned, 1, "two products share #112");

    let out = Doc::from_bytes(bytes);
    assert_no_dangling(&out);
    assert_eq!(world_location(&out, GUID_HOST), [11.0, 2.0, 2.0]);
    assert_eq!(world_location(&out, GUID_TWIN), [1.0, 2.0, 3.0]);
    // The opening stays on the original (shared) placement.
    assert_eq!(refs_of_field(&out, 122, 0), vec![112]);
    assert_eq!(world_location(&out, GUID_OPENING), [1.5, 2.0, 4.0]);
}
