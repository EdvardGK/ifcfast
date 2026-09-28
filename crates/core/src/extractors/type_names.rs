//! Canonical CamelCase spelling of IFC type tokens.
//!
//! STEP writes every name uppercase (`IFCPRESSUREMEASURE(2.5)`); the
//! public tables and the IDS failure strings print the schema spelling
//! ifcopenshell / IfcTester use (`IfcPressureMeasure`). Entities come from
//! the indexer's entity-name table; defined types (every IfcValue wrapper)
//! and enumeration types (IDS dataTypes such as
//! `IfcDoorPanelOperationEnum`) from the generated
//! [`super::defined_type_names`] table (GH #195 / GH #200). One table, both consumers, available without the `ids`
//! feature.

use super::defined_type_names::DEFINED_TYPE_NAMES;

/// The canonical spelling of a defined or enumeration type
/// (`IfcPressureMeasure`, `IfcDoorPanelOperationEnum`) for a token in any
/// ASCII case. `None` when it is neither in IFC2X3 / IFC4 / IFC4X3.
pub fn defined_type_name(token: &[u8]) -> Option<&'static str> {
    DEFINED_TYPE_NAMES
        .binary_search_by(|(k, _)| cmp_upper(k.as_bytes(), token))
        .ok()
        .map(|i| DEFINED_TYPE_NAMES[i].1)
}

/// The CamelCase name of any IFC type token: a defined / enumeration
/// type from the generated table, else the indexer's entity-name table, else the
/// indexer's `Ifc` + single-word title case fallback.
pub fn camel_type_name(token: &[u8]) -> String {
    let t = crate::lexer::trim_ws(token);
    match defined_type_name(t) {
        Some(n) => n.to_string(),
        None => crate::indexer::type_name_uppercase_with_proper_case(t),
    }
}

/// Compare an uppercase table key against a token of any ASCII case.
fn cmp_upper(key: &[u8], token: &[u8]) -> std::cmp::Ordering {
    let n = key.len().min(token.len());
    for i in 0..n {
        let c = key[i].cmp(&token[i].to_ascii_uppercase());
        if c != std::cmp::Ordering::Equal {
            return c;
        }
    }
    key.len().cmp(&token.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_sorted_and_uppercase_keyed() {
        for w in DEFINED_TYPE_NAMES.windows(2) {
            assert!(w[0].0 < w[1].0, "{} !< {}", w[0].0, w[1].0);
        }
        for (k, v) in DEFINED_TYPE_NAMES {
            assert_eq!(*k, v.to_ascii_uppercase());
        }
    }

    #[test]
    fn multi_word_defined_types_are_camel_case() {
        assert_eq!(camel_type_name(b"IFCPRESSUREMEASURE"), "IfcPressureMeasure");
        assert_eq!(camel_type_name(b"IfcPressureMeasure"), "IfcPressureMeasure");
        assert_eq!(
            camel_type_name(b"ifcpositivelengthmeasure"),
            "IfcPositiveLengthMeasure"
        );
        assert_eq!(
            camel_type_name(b"IFCTHERMODYNAMICTEMPERATUREMEASURE"),
            "IfcThermodynamicTemperatureMeasure"
        );
        assert_eq!(camel_type_name(b"IFCTEXT"), "IfcText");
        assert_eq!(camel_type_name(b"IFCBOOLEAN"), "IfcBoolean");
        assert_eq!(
            camel_type_name(b"IFCDOORPANELOPERATIONENUM"),
            "IfcDoorPanelOperationEnum"
        );
        // IFC4X3-only.
        assert_eq!(
            camel_type_name(b"IFCNONNEGATIVELENGTHMEASURE"),
            "IfcNonNegativeLengthMeasure"
        );
        // Entities still go through the indexer's table.
        assert_eq!(
            camel_type_name(b"IFCWALLSTANDARDCASE"),
            "IfcWallStandardCase"
        );
        // Unknown: the long-standing single-word fallback.
        assert_eq!(camel_type_name(b"IFCNOSUCHTHING"), "IfcNosuchthing");
        assert_eq!(defined_type_name(b"IFCWALL"), None);
        assert_eq!(defined_type_name(b"IFCPRESSUREMEASUR"), None);
        assert_eq!(defined_type_name(b"IFCPRESSUREMEASUREX"), None);
    }
}
