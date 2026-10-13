//! The IFC2X3 occurrence / type mapping (ambiguity register A2, A6).
//!
//! Two readings of one rule live here:
//!
//! * [`mapped_type_for`]: the rule the entity facet applies to a name that
//!   is not an IFC2X3 class (IfcTester `Entity.filter` / `__call__`,
//!   `ifctester/facet.py:208-243`): the name maps when `<NAME>TYPE` is an
//!   IFC2X3 type object, and an occurrence matches when its type object
//!   is exactly that class. [`mapped_names`] enumerates the names this
//!   rule accepts, so an `xs:enumeration` / pattern of entity names maps
//!   member by member exactly as the same names would one at a time
//!   (A6, GH #224).
//! * [`TABLE`]: buildingSMART's `ifc2x3-occurrence-type-mapping-table.md`
//!   (IDS repo @ a67047736aa93586d723329fce3aab1b9ac056af,
//!   `Documentation/ImplementersDocumentation/`), 57 rows of
//!   (IDS name, IFC2X3 occurrence entity, IFC2X3 type entity). It is the
//!   only source that knows each name's occurrence entity, which the
//!   schema-free requirement audit needs: a requirement `IFCAIRTERMINAL`
//!   under applicability `IFCFLOWTERMINAL` can match an IFC2X3 file, so it
//!   is not "can never match".
//!
//! The rule accepts two names the table does not list, `IFCELECTRICHEATER`
//! and `IFCGASTERMINAL` (IFC2X3's own `IfcElectricHeaterType` /
//! `IfcGasTerminalType`, deprecated in IFC2X3 and gone in IFC4); the test
//! below pins that difference.

use super::schema_tables::SchemaTables;

/// (IDS facet name, IFC2X3 occurrence entity, IFC2X3 type entity),
/// uppercase, in the table's order.
pub const TABLE: &[(&str, &str, &str)] = &[
    ("IFCFURNITURE", "IFCFURNISHINGELEMENT", "IFCFURNITURETYPE"),
    (
        "IFCSYSTEMFURNITUREELEMENT",
        "IFCFURNISHINGELEMENT",
        "IFCSYSTEMFURNITUREELEMENTTYPE",
    ),
    (
        "IFCACTUATOR",
        "IFCDISTRIBUTIONCONTROLELEMENT",
        "IFCACTUATORTYPE",
    ),
    ("IFCALARM", "IFCDISTRIBUTIONCONTROLELEMENT", "IFCALARMTYPE"),
    (
        "IFCCONTROLLER",
        "IFCDISTRIBUTIONCONTROLELEMENT",
        "IFCCONTROLLERTYPE",
    ),
    (
        "IFCFLOWINSTRUMENT",
        "IFCDISTRIBUTIONCONTROLELEMENT",
        "IFCFLOWINSTRUMENTTYPE",
    ),
    (
        "IFCSENSOR",
        "IFCDISTRIBUTIONCONTROLELEMENT",
        "IFCSENSORTYPE",
    ),
    (
        "IFCAIRTOAIRHEATRECOVERY",
        "IFCENERGYCONVERSIONDEVICE",
        "IFCAIRTOAIRHEATRECOVERYTYPE",
    ),
    ("IFCBOILER", "IFCENERGYCONVERSIONDEVICE", "IFCBOILERTYPE"),
    ("IFCCHILLER", "IFCENERGYCONVERSIONDEVICE", "IFCCHILLERTYPE"),
    ("IFCCOIL", "IFCENERGYCONVERSIONDEVICE", "IFCCOILTYPE"),
    (
        "IFCCONDENSER",
        "IFCENERGYCONVERSIONDEVICE",
        "IFCCONDENSERTYPE",
    ),
    (
        "IFCCOOLEDBEAM",
        "IFCENERGYCONVERSIONDEVICE",
        "IFCCOOLEDBEAMTYPE",
    ),
    (
        "IFCCOOLINGTOWER",
        "IFCENERGYCONVERSIONDEVICE",
        "IFCCOOLINGTOWERTYPE",
    ),
    (
        "IFCELECTRICGENERATOR",
        "IFCENERGYCONVERSIONDEVICE",
        "IFCELECTRICGENERATORTYPE",
    ),
    (
        "IFCELECTRICMOTOR",
        "IFCENERGYCONVERSIONDEVICE",
        "IFCELECTRICMOTORTYPE",
    ),
    (
        "IFCEVAPORATIVECOOLER",
        "IFCENERGYCONVERSIONDEVICE",
        "IFCEVAPORATIVECOOLERTYPE",
    ),
    (
        "IFCEVAPORATOR",
        "IFCENERGYCONVERSIONDEVICE",
        "IFCEVAPORATORTYPE",
    ),
    (
        "IFCHEATEXCHANGER",
        "IFCENERGYCONVERSIONDEVICE",
        "IFCHEATEXCHANGERTYPE",
    ),
    (
        "IFCHUMIDIFIER",
        "IFCENERGYCONVERSIONDEVICE",
        "IFCHUMIDIFIERTYPE",
    ),
    (
        "IFCMOTORCONNECTION",
        "IFCENERGYCONVERSIONDEVICE",
        "IFCMOTORCONNECTIONTYPE",
    ),
    (
        "IFCSPACEHEATER",
        "IFCENERGYCONVERSIONDEVICE",
        "IFCSPACEHEATERTYPE",
    ),
    (
        "IFCTRANSFORMER",
        "IFCENERGYCONVERSIONDEVICE",
        "IFCTRANSFORMERTYPE",
    ),
    (
        "IFCTUBEBUNDLE",
        "IFCENERGYCONVERSIONDEVICE",
        "IFCTUBEBUNDLETYPE",
    ),
    (
        "IFCUNITARYEQUIPMENT",
        "IFCENERGYCONVERSIONDEVICE",
        "IFCUNITARYEQUIPMENTTYPE",
    ),
    (
        "IFCAIRTERMINALBOX",
        "IFCFLOWCONTROLLER",
        "IFCAIRTERMINALBOXTYPE",
    ),
    ("IFCDAMPER", "IFCFLOWCONTROLLER", "IFCDAMPERTYPE"),
    (
        "IFCELECTRICTIMECONTROL",
        "IFCFLOWCONTROLLER",
        "IFCELECTRICTIMECONTROLTYPE",
    ),
    ("IFCFLOWMETER", "IFCFLOWCONTROLLER", "IFCFLOWMETERTYPE"),
    (
        "IFCPROTECTIVEDEVICE",
        "IFCFLOWCONTROLLER",
        "IFCPROTECTIVEDEVICETYPE",
    ),
    (
        "IFCSWITCHINGDEVICE",
        "IFCFLOWCONTROLLER",
        "IFCSWITCHINGDEVICETYPE",
    ),
    ("IFCVALVE", "IFCFLOWCONTROLLER", "IFCVALVETYPE"),
    (
        "IFCCABLECARRIERFITTING",
        "IFCFLOWFITTING",
        "IFCCABLECARRIERFITTINGTYPE",
    ),
    ("IFCDUCTFITTING", "IFCFLOWFITTING", "IFCDUCTFITTINGTYPE"),
    ("IFCJUNCTIONBOX", "IFCFLOWFITTING", "IFCJUNCTIONBOXTYPE"),
    ("IFCPIPEFITTING", "IFCFLOWFITTING", "IFCPIPEFITTINGTYPE"),
    ("IFCCOMPRESSOR", "IFCFLOWMOVINGDEVICE", "IFCCOMPRESSORTYPE"),
    ("IFCFAN", "IFCFLOWMOVINGDEVICE", "IFCFANTYPE"),
    ("IFCPUMP", "IFCFLOWMOVINGDEVICE", "IFCPUMPTYPE"),
    (
        "IFCCABLECARRIERSEGMENT",
        "IFCFLOWSEGMENT",
        "IFCCABLECARRIERSEGMENTTYPE",
    ),
    ("IFCCABLESEGMENT", "IFCFLOWSEGMENT", "IFCCABLESEGMENTTYPE"),
    ("IFCDUCTSEGMENT", "IFCFLOWSEGMENT", "IFCDUCTSEGMENTTYPE"),
    ("IFCPIPESEGMENT", "IFCFLOWSEGMENT", "IFCPIPESEGMENTTYPE"),
    (
        "IFCELECTRICFLOWSTORAGEDEVICE",
        "IFCFLOWSTORAGEDEVICE",
        "IFCELECTRICFLOWSTORAGEDEVICETYPE",
    ),
    ("IFCTANK", "IFCFLOWSTORAGEDEVICE", "IFCTANKTYPE"),
    ("IFCAIRTERMINAL", "IFCFLOWTERMINAL", "IFCAIRTERMINALTYPE"),
    (
        "IFCELECTRICAPPLIANCE",
        "IFCFLOWTERMINAL",
        "IFCELECTRICAPPLIANCETYPE",
    ),
    (
        "IFCFIRESUPPRESSIONTERMINAL",
        "IFCFLOWTERMINAL",
        "IFCFIRESUPPRESSIONTERMINALTYPE",
    ),
    ("IFCLAMP", "IFCFLOWTERMINAL", "IFCLAMPTYPE"),
    ("IFCLIGHTFIXTURE", "IFCFLOWTERMINAL", "IFCLIGHTFIXTURETYPE"),
    ("IFCOUTLET", "IFCFLOWTERMINAL", "IFCOUTLETTYPE"),
    (
        "IFCSANITARYTERMINAL",
        "IFCFLOWTERMINAL",
        "IFCSANITARYTERMINALTYPE",
    ),
    (
        "IFCSTACKTERMINAL",
        "IFCFLOWTERMINAL",
        "IFCSTACKTERMINALTYPE",
    ),
    (
        "IFCWASTETERMINAL",
        "IFCFLOWTERMINAL",
        "IFCWASTETERMINALTYPE",
    ),
    (
        "IFCDUCTSILENCER",
        "IFCFLOWTREATMENTDEVICE",
        "IFCDUCTSILENCERTYPE",
    ),
    ("IFCFILTER", "IFCFLOWTREATMENTDEVICE", "IFCFILTERTYPE"),
    (
        "IFCVIBRATIONISOLATOR",
        "IFCEQUIPMENTELEMENT",
        "IFCVIBRATIONISOLATORTYPE",
    ),
];

/// The IFC2X3 type object a name maps to under the entity facet's rule:
/// `name` is not an IFC2X3 class, does not end in `TYPE`, and
/// `<name>TYPE` is a type object. `t` must be the IFC2X3 tables.
pub fn mapped_type_for(t: &'static SchemaTables, name: &str) -> Option<&'static str> {
    if name.ends_with("TYPE") || t.canonical(name).is_some() {
        return None;
    }
    let type_name = format!("{name}TYPE");
    t.canonical(&type_name)
        .filter(|tn| *tn == type_name && t.is_type_object(tn))
}

/// Every name [`mapped_type_for`] accepts, as `(name, type)`, sorted by
/// name. `t` must be the IFC2X3 tables.
pub fn mapped_names(t: &'static SchemaTables) -> Vec<(String, &'static str)> {
    let mut v: Vec<(String, &'static str)> = t
        .type_objects
        .iter()
        .filter_map(|tn| {
            let name = tn.strip_suffix("TYPE")?;
            mapped_type_for(t, name).map(|m| (name.to_string(), m))
        })
        .collect();
    v.sort();
    v
}

/// Names of [`TABLE`] whose occurrence entity is `occurrence`.
pub fn table_names_for_occurrence(occurrence: &str) -> impl Iterator<Item = &'static str> + '_ {
    TABLE
        .iter()
        .filter(move |(_, o, _)| *o == occurrence)
        .map(|(n, _, _)| *n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ir::Schema;
    use crate::ids::schema_tables::tables;

    #[test]
    fn ids_2x3_mapping_table_agrees_with_the_schema_rule() {
        let t2 = tables(Schema::Ifc2x3);
        assert_eq!(TABLE.len(), 57);
        for (n, occ, ty) in TABLE {
            assert!(!t2.has_entity(n), "{n} is an IFC2X3 class");
            assert!(t2.has_entity(occ), "{occ} missing in IFC2X3");
            assert!(t2.is_type_object(ty), "{ty} not a type object");
            assert_eq!(mapped_type_for(t2, n), Some(*ty), "{n}");
            // Every table name is an IFC4 class.
            assert!(tables(Schema::Ifc4).has_entity(n), "{n} not in IFC4");
        }
        let rule: Vec<String> = mapped_names(t2).into_iter().map(|(n, _)| n).collect();
        let extra: Vec<&str> = rule
            .iter()
            .map(String::as_str)
            .filter(|n| !TABLE.iter().any(|(t, _, _)| t == n))
            .collect();
        assert_eq!(extra, ["IFCELECTRICHEATER", "IFCGASTERMINAL"]);
        assert_eq!(rule.len(), 59);
        // IFCDOOR (IfcDoorStyle) and IFCCOLUMN (a class) never map.
        assert_eq!(mapped_type_for(t2, "IFCDOOR"), None);
        assert_eq!(mapped_type_for(t2, "IFCCOLUMN"), None);
        assert_eq!(
            table_names_for_occurrence("IFCFLOWTERMINAL").count(),
            9,
            "flow terminal rows"
        );
    }
}
