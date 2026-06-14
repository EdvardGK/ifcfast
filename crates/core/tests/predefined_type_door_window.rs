//! Regression tests for IFC4 IfcDoor / IfcWindow PredefinedType extraction.
//!
//! Bug #74: IFC4 IfcDoor / IfcWindow carry two trailing enums plus a
//! user-defined string —
//!   IfcDoor:   …, PredefinedType, OperationType,    UserDefinedOperationType
//!   IfcWindow: …, PredefinedType, PartitioningType, UserDefinedPartitioningType
//! The old walk-from-right took OperationType/PartitioningType (the wrong
//! attribute), and when the trailing UserDefined string was set it returned
//! None even though PredefinedType was `.USERDEFINED.`. These tests pin the
//! correct behaviour and guard IFC2X3 (no PredefinedType on door/window).

use _core::indexer;

/// Look up the predefined_type for the product with the given GUID.
fn predefined_for(file: &str, guid: &str) -> Option<Option<String>> {
    let idx = indexer::index(file.as_bytes());
    idx.product_guid
        .iter()
        .position(|g| g == guid)
        .map(|i| idx.product_predefined_type[i].clone())
}

fn ifc4_header(schema: &str) -> String {
    format!(
        "ISO-10303-21;\n\
HEADER;\n\
FILE_DESCRIPTION((''),'2;1');\n\
FILE_NAME('test','',(''),(''),'','','');\n\
FILE_SCHEMA(('{schema}'));\n\
ENDSEC;\n\
DATA;\n"
    )
}

const FOOTER: &str = "ENDSEC;\nEND-ISO-10303-21;\n";

#[test]
fn ifc4_door_predefined_type_is_first_enum_not_operation_type() {
    // #40=IFCDOOR(...,2100.,900.,.DOOR.,.SINGLE_SWING_LEFT.,$)
    // PredefinedType=.DOOR., OperationType=.SINGLE_SWING_LEFT.
    let file = format!(
        "{}#1=IFCDOOR('0door_normal_guid000000',$,'D1',$,$,$,$,'tag1',2100.,900.,.DOOR.,.SINGLE_SWING_LEFT.,$);\n{}",
        ifc4_header("IFC4"),
        FOOTER
    );
    let got = predefined_for(&file, "0door_normal_guid000000");
    assert_eq!(
        got,
        Some(Some("DOOR".to_string())),
        "IFC4 IfcDoor PredefinedType should be DOOR, not the OperationType"
    );
}

#[test]
fn ifc4_door_userdefined_is_preserved_not_none() {
    // Trailing UserDefinedOperationType string set; PredefinedType=.USERDEFINED.
    // #41=IFCDOOR(...,.USERDEFINED.,.SINGLE_SWING_LEFT.,'MyCustomOp')
    let file = format!(
        "{}#1=IFCDOOR('0door_userdef_guid000000',$,'D2',$,$,$,$,'tag2',2100.,900.,.USERDEFINED.,.SINGLE_SWING_LEFT.,'MyCustomOp');\n{}",
        ifc4_header("IFC4"),
        FOOTER
    );
    let got = predefined_for(&file, "0door_userdef_guid000000");
    assert_eq!(
        got,
        Some(Some("USERDEFINED".to_string())),
        "IFC4 IfcDoor USERDEFINED PredefinedType must be preserved, not collapsed to None"
    );
}

#[test]
fn ifc4_door_notdefined_when_absent() {
    // PredefinedType=.NOTDEFINED., OperationType=$, UserDefined=$
    let file = format!(
        "{}#1=IFCDOOR('0door_notdef_guid0000000',$,'D3',$,$,$,$,'tag3',2100.,900.,.NOTDEFINED.,$,$);\n{}",
        ifc4_header("IFC4"),
        FOOTER
    );
    let got = predefined_for(&file, "0door_notdef_guid0000000");
    assert_eq!(got, Some(Some("NOTDEFINED".to_string())));
}

#[test]
fn ifc4_window_predefined_type_is_first_enum_not_partitioning_type() {
    // PredefinedType=.WINDOW., PartitioningType=.SINGLE_PANEL.
    let file = format!(
        "{}#1=IFCWINDOW('0windownormal_guid00000',$,'W1',$,$,$,$,'tag4',1200.,1500.,.WINDOW.,.SINGLE_PANEL.,$);\n{}",
        ifc4_header("IFC4"),
        FOOTER
    );
    let got = predefined_for(&file, "0windownormal_guid00000");
    assert_eq!(
        got,
        Some(Some("WINDOW".to_string())),
        "IFC4 IfcWindow PredefinedType should be WINDOW, not the PartitioningType"
    );
}

#[test]
fn ifc4_window_userdefined_is_preserved() {
    let file = format!(
        "{}#1=IFCWINDOW('0windowuserdef_guid0000',$,'W2',$,$,$,$,'tag5',1200.,1500.,.USERDEFINED.,.SINGLE_PANEL.,'CustomPart');\n{}",
        ifc4_header("IFC4"),
        FOOTER
    );
    let got = predefined_for(&file, "0windowuserdef_guid0000");
    assert_eq!(got, Some(Some("USERDEFINED".to_string())));
}

#[test]
fn ifc2x3_door_has_no_predefined_type() {
    // IFC2X3 IfcDoor: …, OverallHeight, OverallWidth — NO PredefinedType,
    // NO OperationType. Trailing fields are numeric / null. Must stay None.
    let file = format!(
        "{}#1=IFCDOOR('0door2x3_guid000000000',$,'D2x3',$,$,$,$,'tag6',2100.,900.);\n{}",
        ifc4_header("IFC2X3"),
        FOOTER
    );
    let got = predefined_for(&file, "0door2x3_guid000000000");
    assert_eq!(
        got,
        Some(None),
        "IFC2X3 IfcDoor has no PredefinedType; must be None"
    );
}

#[test]
fn ifc2x3_window_has_no_predefined_type() {
    let file = format!(
        "{}#1=IFCWINDOW('0window2x3_guid0000000',$,'W2x3',$,$,$,$,'tag7',1200.,1500.);\n{}",
        ifc4_header("IFC2X3"),
        FOOTER
    );
    let got = predefined_for(&file, "0window2x3_guid0000000");
    assert_eq!(got, Some(None));
}

#[test]
fn ifc4_door_standardcase_shares_layout() {
    // IfcDoorStandardCase has the same 13-field layout as IfcDoor.
    let file = format!(
        "{}#1=IFCDOORSTANDARDCASE('0doorstd_guid0000000000',$,'DSC',$,$,$,$,'tag9',2100.,900.,.DOOR.,.DOUBLE_SWING_LEFT.,$);\n{}",
        ifc4_header("IFC4"),
        FOOTER
    );
    let got = predefined_for(&file, "0doorstd_guid0000000000");
    assert_eq!(got, Some(Some("DOOR".to_string())));
}

#[test]
fn ifc4_plain_element_still_uses_last_enum() {
    // A non-door/window IFC4 product (single trailing PredefinedType enum)
    // must keep the original walk-from-right behaviour.
    // IfcWall (IFC4): …, PredefinedType is the last enum.
    let file = format!(
        "{}#1=IFCWALL('0wall_guid00000000000000',$,'WALL1',$,$,$,$,'tag8',.STANDARD.);\n{}",
        ifc4_header("IFC4"),
        FOOTER
    );
    let got = predefined_for(&file, "0wall_guid00000000000000");
    assert_eq!(got, Some(Some("STANDARD".to_string())));
}
