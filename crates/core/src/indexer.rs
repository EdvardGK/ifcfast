//! Tier-1 indexer: walks STEP records and extracts the subset of
//! IFC entity attributes that fastparse's index.parquet + storeys.parquet
//! need.
//!
//! Output is column-major (Vec per attribute) so PyO3 can hand it to
//! pandas / pyarrow without per-row Python object construction.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use crate::lexer::{
    data_section_start, endsec_position, for_each_record, parse_field, parse_ref_list,
    split_top_level_args, split_top_level_args_into, Field,
};

// ----------------------------------------------------------------------
// Static type sets — keep tight; downstream is the fastparse cache schema.
// ----------------------------------------------------------------------

/// IfcProduct subtypes we extract as "products". This list is the union
/// of types observed across the LBK Building C model set plus the common
/// IFC4 building elements. Unknown types simply don't appear in the
/// product table — they're still counted in `all_entity_counts` so the
/// caller can spot a missing type.
const PRODUCT_TYPES: &[&[u8]] = &[
    // Walls
    b"IFCWALL", b"IFCWALLSTANDARDCASE", b"IFCWALLELEMENTEDCASE", b"IFCCURTAINWALL",
    // Slabs / plates
    b"IFCSLAB", b"IFCSLABSTANDARDCASE", b"IFCSLABELEMENTEDCASE", b"IFCPLATE",
    b"IFCPLATESTANDARDCASE",
    // Structural members
    b"IFCBEAM", b"IFCBEAMSTANDARDCASE", b"IFCCOLUMN", b"IFCCOLUMNSTANDARDCASE",
    b"IFCMEMBER", b"IFCMEMBERSTANDARDCASE",
    b"IFCFOOTING", b"IFCPILE",
    // Openings / fenestration
    b"IFCDOOR", b"IFCDOORSTANDARDCASE", b"IFCWINDOW", b"IFCWINDOWSTANDARDCASE",
    b"IFCOPENINGELEMENT", b"IFCVOIDINGFEATURE", b"IFCSURFACEFEATURE",
    // Stairs / ramps / rails
    b"IFCSTAIR", b"IFCSTAIRFLIGHT", b"IFCRAMP", b"IFCRAMPFLIGHT",
    b"IFCRAILING", b"IFCROOF",
    // Covering / finish
    b"IFCCOVERING",
    // Generic
    b"IFCBUILDINGELEMENTPROXY", b"IFCBUILDINGELEMENTPART",
    b"IFCELEMENTASSEMBLY", b"IFCTRANSPORTELEMENT",
    b"IFCANNOTATION", b"IFCVIRTUALELEMENT",
    b"IFCDISCRETEACCESSORY", b"IFCFASTENER", b"IFCMECHANICALFASTENER",
    b"IFCREINFORCINGBAR", b"IFCREINFORCINGMESH", b"IFCTENDON", b"IFCTENDONANCHOR",
    // Distribution / MEP
    b"IFCDISTRIBUTIONELEMENT", b"IFCDISTRIBUTIONFLOWELEMENT",
    b"IFCDISTRIBUTIONCONTROLELEMENT", b"IFCDISTRIBUTIONPORT",
    b"IFCFLOWFITTING", b"IFCFLOWSEGMENT", b"IFCFLOWTERMINAL",
    b"IFCFLOWCONTROLLER", b"IFCFLOWMOVINGDEVICE", b"IFCFLOWSTORAGEDEVICE",
    b"IFCFLOWTREATMENTDEVICE", b"IFCENERGYCONVERSIONDEVICE",
    b"IFCPIPEFITTING", b"IFCPIPESEGMENT", b"IFCDUCTFITTING", b"IFCDUCTSEGMENT",
    b"IFCDUCTSILENCER",
    b"IFCCABLECARRIERFITTING", b"IFCCABLECARRIERSEGMENT",
    b"IFCCABLEFITTING", b"IFCCABLESEGMENT",
    b"IFCVALVE", b"IFCFLOWVALVE",
    b"IFCSANITARYTERMINAL", b"IFCLIGHTFIXTURE", b"IFCOUTLET",
    b"IFCSWITCHINGDEVICE", b"IFCELECTRICAPPLIANCE",
    b"IFCELECTRICDISTRIBUTIONBOARD", b"IFCELECTRICFLOWSTORAGEDEVICE",
    b"IFCAIRTERMINAL", b"IFCAIRTERMINALBOX", b"IFCDAMPER", b"IFCFILTER",
    b"IFCBOILER", b"IFCBURNER", b"IFCCHILLER", b"IFCCOMPRESSOR",
    b"IFCCONDENSER", b"IFCCOOLINGTOWER", b"IFCEVAPORATOR",
    b"IFCFAN", b"IFCHEATEXCHANGER", b"IFCHUMIDIFIER",
    b"IFCMOTORCONNECTION", b"IFCPUMP", b"IFCTANK", b"IFCUNITARYEQUIPMENT",
    b"IFCSENSOR", b"IFCACTUATOR", b"IFCCONTROLLER", b"IFCALARM",
    b"IFCFLOWMETER", b"IFCPROTECTIVEDEVICE", b"IFCPROTECTIVEDEVICETRIPPINGUNIT",
    b"IFCJUNCTIONBOX", b"IFCCOMMUNICATIONSAPPLIANCE",
    b"IFCAUDIOVISUALAPPLIANCE", b"IFCFIRESUPPRESSIONTERMINAL",
    b"IFCMEDICALDEVICE", b"IFCMOBILETELECOMMUNICATIONSAPPLIANCE",
    b"IFCSOLARDEVICE", b"IFCSTACKTERMINAL", b"IFCSPACEHEATER",
    b"IFCWASTETERMINAL", b"IFCUNITARYCONTROLELEMENT",
    // Lights / lamps / additional MEP
    b"IFCLAMP", b"IFCCOIL",
    // Survey / layout — IfcGrid is IfcProduct; IfcGridAxis is NOT
    b"IFCGRID",
    // Furnishings
    b"IFCFURNISHINGELEMENT", b"IFCFURNITURE", b"IFCSYSTEMFURNITUREELEMENT",
    // Civil / structural
    b"IFCEARTHWORKSCUT", b"IFCEARTHWORKSFILL", b"IFCEARTHWORKSELEMENT",
    b"IFCKERB", b"IFCPAVEMENT", b"IFCRAIL", b"IFCROAD",
    b"IFCBRIDGE", b"IFCBRIDGEPART", b"IFCMARINEFACILITY", b"IFCMARINEPART",
];

/// Spatial structure types — separate output table.
const STOREY_TYPES: &[&[u8]] = &[b"IFCBUILDINGSTOREY"];

const SITE_TYPE: &[u8] = b"IFCSITE";
const BUILDING_TYPE: &[u8] = b"IFCBUILDING";
const PROJECT_TYPE: &[u8] = b"IFCPROJECT";
const SPACE_TYPE: &[u8] = b"IFCSPACE";
const APPLICATION_TYPE: &[u8] = b"IFCAPPLICATION";
const CONTAINED_TYPE: &[u8] = b"IFCRELCONTAINEDINSPATIALSTRUCTURE";
const AGGREGATES_TYPE: &[u8] = b"IFCRELAGGREGATES";
const SI_UNIT_TYPE: &[u8] = b"IFCSIUNIT";
const UNIT_ASSIGN_TYPE: &[u8] = b"IFCUNITASSIGNMENT";
const VOIDS_ELEMENT_TYPE: &[u8] = b"IFCRELVOIDSELEMENT";
const DEFINES_BY_TYPE_TYPE: &[u8] = b"IFCRELDEFINESBYTYPE";

// ----------------------------------------------------------------------
// Dispatch
// ----------------------------------------------------------------------

/// All the entity categories the indexer reacts to. Everything else in
/// the file is ignored (but counted in the `total` record stat). One
/// HashMap lookup per record replaces the previous chain of HashSet
/// lookups + byte-slice equality checks — a big win on MEP files where
/// ~99% of records are types we don't care about (e.g. IfcCartesianPoint,
/// IfcPolyLoop, IfcPropertySingleValue).
#[derive(Debug, Clone, Copy)]
enum EntityKind {
    Product,
    Storey,
    Site,
    Building,
    Project,
    Space,
    Application,
    ContainedInSpatialStructure,
    SiUnit,
    UnitAssignment,
    Aggregates,
    VoidsElement,
    DefinesByType,
    /// Any IfcXxxType (IfcWallType, IfcDoorType, IfcSensorType, …)
    /// — matched by a byte-suffix fallback rather than dispatch-map
    /// enumeration so new IFC schema additions don't drop silently.
    TypeObject,
}

fn dispatch_map() -> &'static HashMap<&'static [u8], EntityKind> {
    static MAP: OnceLock<HashMap<&'static [u8], EntityKind>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut m: HashMap<&'static [u8], EntityKind> =
            HashMap::with_capacity(PRODUCT_TYPES.len() + STOREY_TYPES.len() + 9);
        for t in PRODUCT_TYPES {
            m.insert(t, EntityKind::Product);
        }
        for t in STOREY_TYPES {
            m.insert(t, EntityKind::Storey);
        }
        m.insert(SITE_TYPE, EntityKind::Site);
        m.insert(BUILDING_TYPE, EntityKind::Building);
        m.insert(PROJECT_TYPE, EntityKind::Project);
        m.insert(SPACE_TYPE, EntityKind::Space);
        m.insert(APPLICATION_TYPE, EntityKind::Application);
        m.insert(CONTAINED_TYPE, EntityKind::ContainedInSpatialStructure);
        m.insert(SI_UNIT_TYPE, EntityKind::SiUnit);
        m.insert(UNIT_ASSIGN_TYPE, EntityKind::UnitAssignment);
        m.insert(AGGREGATES_TYPE, EntityKind::Aggregates);
        m.insert(VOIDS_ELEMENT_TYPE, EntityKind::VoidsElement);
        m.insert(DEFINES_BY_TYPE_TYPE, EntityKind::DefinesByType);
        m
    })
}

// ----------------------------------------------------------------------
// Output containers
// ----------------------------------------------------------------------

#[derive(Default)]
pub struct IndexedFile {
    // ----- Tier 0/1 manifest fields -----
    pub schema: String,            // e.g. "IFC4" or "IFC2X3"
    pub project_name: Option<String>,
    pub authoring_app: Option<String>,

    // ----- Type histogram for PRODUCT types only -----
    pub type_counts: HashMap<String, u32>,

    // ----- Products (column-major) -----
    pub product_step_id: Vec<u64>,
    pub product_guid: Vec<String>,
    pub product_entity: Vec<String>,
    pub product_name: Vec<Option<String>>,
    pub product_predefined_type: Vec<Option<String>>,
    pub product_object_type: Vec<Option<String>>,
    pub product_tag: Vec<Option<String>>,

    // ----- Storeys (column-major) -----
    pub storey_step_id: Vec<u64>,
    pub storey_guid: Vec<String>,
    pub storey_name: Vec<Option<String>>,
    pub storey_elevation: Vec<Option<f64>>,
    pub storey_building_step_id: Vec<Option<u64>>,

    // ----- Site / Building / Project / Space (for parent_guid resolution) -----
    pub site_step_id_to_guid: HashMap<u64, String>,
    pub building_step_id_to_guid: HashMap<u64, String>,
    pub project_step_id_to_guid: HashMap<u64, String>,
    pub space_step_id_to_guid: HashMap<u64, String>,

    // ----- Containment relationships (parallel `Vec<u64>` columns) -----
    // Stored as parallel arrays rather than `Vec<(u64, u64)>` to avoid one
    // tuple allocation per row when these get marshalled into Python. On
    // very-MEP-heavy files (>100K relationships) that's the difference
    // between dozens and hundreds of ms in the PyO3 bridge.

    /// IfcRelContainedInSpatialStructure: parallel arrays of
    /// `(child_step_id[i], storey_step_id[i])`. Already filtered to
    /// storey-relating containment only.
    pub contained_in_child: Vec<u64>,
    pub contained_in_structure: Vec<u64>,

    /// IfcRelAggregates: parallel arrays of `(child_step_id[i],
    /// parent_step_id[i])`. Spatial relating objects are NOT filtered
    /// out — the parent can be a product, storey, building or site.
    pub aggregates_child: Vec<u64>,
    pub aggregates_parent: Vec<u64>,

    /// IfcRelAggregates filtered to storey↔building pairs only —
    /// `(storey_step_id[i], building_step_id[i])`.
    pub storey_building_storey: Vec<u64>,
    pub storey_building_building: Vec<u64>,

    /// IfcRelVoidsElement: parallel arrays of
    /// `(opening_step_id[i], host_step_id[i])`. One row per relation;
    /// each relation links exactly one opening to exactly one host
    /// (unlike RelAggregates / RelContainedInSpatialStructure which fan
    /// out N relateds per row).
    pub voids_opening: Vec<u64>,
    pub voids_host: Vec<u64>,

    /// IfcRelDefinesByType: parallel arrays of
    /// `(product_step_id[i], type_step_id[i])`. RelatedObjects is a list,
    /// so we fan out N product rows per relation.
    pub defines_by_type_product: Vec<u64>,
    pub defines_by_type_type: Vec<u64>,

    /// IfcTypeObject (and its subclasses — IfcWallType, IfcDoorType,
    /// IfcSensorType, …): parallel column arrays. Captures the GUID and
    /// Name of every IfcXxxType in the file so the RelDefinesByType
    /// relation can be resolved to (type_guid, type_name) per product.
    pub type_object_step_id: Vec<u64>,
    pub type_object_entity: Vec<String>,
    pub type_object_guid: Vec<String>,
    pub type_object_name: Vec<Option<String>>,

    // ----- Length unit (metres per model unit). None means undetermined. -----
    pub unit_scale: Option<f64>,
}

// ----------------------------------------------------------------------
// HEADER section — extract schema, originating app, etc.
// ----------------------------------------------------------------------

fn extract_header(buf: &[u8]) -> (String, Option<String>) {
    let mut schema = String::new();
    let mut originating: Option<String> = None;

    // FILE_SCHEMA (('IFC4'));   /   FILE_SCHEMA (('IFC2X3')) ;
    if let Some(start) = find_token(buf, b"FILE_SCHEMA") {
        if let Some(open) = find_byte(buf, start, b'(') {
            if let Some(close) = find_byte(buf, open + 1, b')') {
                let s = &buf[open + 1..close];
                // Strip inner parens / commas / quotes / whitespace.
                let s = std::str::from_utf8(s).unwrap_or("");
                let s = s.replace(['(', ')', '\''], "").replace(',', "");
                schema = s.trim().to_string();
                // If multiple schemas listed, take the first.
                if let Some(sp) = schema.split_whitespace().next() {
                    schema = sp.to_string();
                }
            }
        }
    }

    // FILE_NAME ('name', 'time_stamp', ('author',), ('org',), 'preprocessor_version', 'originating_system', 'authorisation');
    if let Some(start) = find_token(buf, b"FILE_NAME") {
        if let Some(open) = find_byte(buf, start, b'(') {
            // Find matching ')'.
            if let Some(close) = find_matching_paren(buf, open) {
                let args = &buf[open + 1..close];
                let fields = split_top_level_args(args);
                // Position 5 = originating_system (0-indexed).
                if fields.len() > 5 {
                    if let Field::String(s) = parse_field(fields[5]) {
                        if !s.is_empty() {
                            originating = Some(s);
                        }
                    }
                }
            }
        }
    }

    (schema, originating)
}

fn find_byte(buf: &[u8], from: usize, target: u8) -> Option<usize> {
    memchr::memchr(target, &buf[from..]).map(|o| from + o)
}

fn find_token(buf: &[u8], needle: &[u8]) -> Option<usize> {
    let limit = buf.len().min(64 * 1024); // header is at the start
    let prefix = &buf[..limit];
    let mut i = 0;
    while i + needle.len() <= prefix.len() {
        if &prefix[i..i + needle.len()] == needle {
            return Some(i + needle.len());
        }
        i += 1;
    }
    None
}

fn find_matching_paren(buf: &[u8], open_idx: usize) -> Option<usize> {
    let mut depth: i32 = 0;
    let mut i = open_idx;
    let mut in_string = false;
    while i < buf.len() {
        let b = buf[i];
        if in_string {
            if b == b'\'' {
                if i + 1 < buf.len() && buf[i + 1] == b'\'' {
                    i += 2;
                    continue;
                }
                in_string = false;
            }
        } else {
            match b {
                b'\'' => in_string = true,
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

// ----------------------------------------------------------------------
// Main entry: walk the file
// ----------------------------------------------------------------------

pub fn index(buf: &[u8]) -> IndexedFile {
    let mut out = IndexedFile::default();

    let (schema, originating) = extract_header(buf);
    out.schema = schema;
    if let Some(o) = originating {
        out.authoring_app = Some(o);
    }

    let dispatch = dispatch_map();

    // Snapshot schema for the extractor — it needs to know IFC2X3 vs IFC4
    // to suppress predefined_type for entities where the trailing-enum
    // slot is a different attribute in IFC2X3 (see issue #8 finding 1).
    let is_ifc2x3 = out.schema.eq_ignore_ascii_case("IFC2X3");

    // step_id -> (unit_type, prefix_name, unit_name) for SI units we see
    let mut si_units: HashMap<u64, (String, String, String)> = HashMap::new();
    // The first IfcUnitAssignment.Units we encounter; one project = one
    // assignment in practice.
    let mut unit_assignment_refs: Vec<u64> = Vec::new();

    let data_start = data_section_start(buf).unwrap_or(0);
    let data_end = endsec_position(buf, data_start);

    // Reused across every record — saves one Vec allocation per STEP
    // entity (600K+ on ST28_RIV).
    let mut fields_buf: Vec<&[u8]> = Vec::with_capacity(16);

    // Two-pass would let us resolve some refs, but a single pass is enough:
    // we only need step_id→guid maps that are built as we go, and downstream
    // (Python) does the final guid resolution for relationships.
    for_each_record(buf, data_start, data_end, |rec| {
        let t = rec.type_name;
        // Single-lookup dispatch. Hot-path miss (>99% of records on big
        // MEP files) is one HashMap probe; previously each miss walked
        // two HashSets and ~8 byte-slice equality checks.
        let kind = match dispatch.get(t) {
            Some(k) => *k,
            None => {
                // Fallback: any IfcXxxType entity (IfcWallType,
                // IfcSensorType, …) is the target of IfcRelDefinesByType.
                // Enumerating every subclass in the dispatch map ages
                // poorly across schema versions; a 7-char suffix check
                // costs ~one memcmp per un-dispatched record and lets
                // new schema additions surface automatically.
                if t.len() > 7 && t.starts_with(b"IFC") && t.ends_with(b"TYPE") {
                    EntityKind::TypeObject
                } else {
                    return;
                }
            }
        };
        match kind {
            EntityKind::Product => {
                split_top_level_args_into(rec.args, &mut fields_buf);
                extract_product(&mut out, rec.id, t, &fields_buf, is_ifc2x3);
            }
            EntityKind::Storey => {
                split_top_level_args_into(rec.args, &mut fields_buf);
                extract_storey(&mut out, rec.id, &fields_buf);
            }
            EntityKind::Site => {
                split_top_level_args_into(rec.args, &mut fields_buf);
                if let Some(guid) = string_at(&fields_buf, 0) {
                    out.site_step_id_to_guid.insert(rec.id, guid);
                }
            }
            EntityKind::Building => {
                split_top_level_args_into(rec.args, &mut fields_buf);
                if let Some(guid) = string_at(&fields_buf, 0) {
                    out.building_step_id_to_guid.insert(rec.id, guid);
                }
            }
            EntityKind::Project => {
                split_top_level_args_into(rec.args, &mut fields_buf);
                if let Some(name) = string_at(&fields_buf, 2) {
                    out.project_name = Some(name);
                }
                if let Some(guid) = string_at(&fields_buf, 0) {
                    out.project_step_id_to_guid.insert(rec.id, guid);
                }
            }
            EntityKind::Space => {
                // IfcSpace can be a parent in IfcRelAggregates rels (other
                // spaces or assemblies aggregated under it). Needs a step_id
                // resolver entry to avoid silently dropping those rels.
                split_top_level_args_into(rec.args, &mut fields_buf);
                if let Some(guid) = string_at(&fields_buf, 0) {
                    out.space_step_id_to_guid.insert(rec.id, guid);
                }
            }
            EntityKind::Application => {
                // IfcApplication: ApplicationDeveloper, Version,
                // ApplicationFullName, ApplicationIdentifier.
                split_top_level_args_into(rec.args, &mut fields_buf);
                if let Some(full_name) = string_at(&fields_buf, 2) {
                    out.authoring_app = Some(full_name);
                }
            }
            EntityKind::ContainedInSpatialStructure => {
                // IfcRelContainedInSpatialStructure(_,_,_,_, RelatedElements, RelatingStructure).
                split_top_level_args_into(rec.args, &mut fields_buf);
                if fields_buf.len() >= 6 {
                    if let Field::List(body) = parse_field(fields_buf[4]) {
                        if let Field::Ref(structure_id) = parse_field(fields_buf[5]) {
                            for child in parse_ref_list(body) {
                                out.contained_in_child.push(child);
                                out.contained_in_structure.push(structure_id);
                            }
                        }
                    }
                }
            }
            EntityKind::SiUnit => {
                // IfcSIUnit(Dimensions, UnitType, Prefix, Name).
                split_top_level_args_into(rec.args, &mut fields_buf);
                let ut = enum_at(&fields_buf, 1).unwrap_or_default();
                let prefix = enum_at(&fields_buf, 2).unwrap_or_default();
                let name = enum_at(&fields_buf, 3).unwrap_or_default();
                si_units.insert(rec.id, (ut, prefix, name));
            }
            EntityKind::UnitAssignment => {
                // IfcUnitAssignment(Units) — Units is a list of refs at arg[0].
                split_top_level_args_into(rec.args, &mut fields_buf);
                if let Some(f) = fields_buf.first() {
                    if let Field::List(body) = parse_field(f) {
                        if unit_assignment_refs.is_empty() {
                            unit_assignment_refs = parse_ref_list(body);
                        }
                    }
                }
            }
            EntityKind::Aggregates => {
                // IfcRelAggregates(_,_,_,_, RelatingObject, RelatedObjects).
                split_top_level_args_into(rec.args, &mut fields_buf);
                if fields_buf.len() >= 6 {
                    if let Field::Ref(rel) = parse_field(fields_buf[4]) {
                        if let Field::List(body) = parse_field(fields_buf[5]) {
                            for child in parse_ref_list(body) {
                                out.aggregates_child.push(child);
                                out.aggregates_parent.push(rel);
                            }
                        }
                    }
                }
            }
            EntityKind::VoidsElement => {
                // IfcRelVoidsElement(GlobalId, OwnerHistory, Name, Description,
                //                    RelatingBuildingElement, RelatedOpeningElement).
                // Same field positions in IFC2X3 and IFC4. Both refs are
                // singletons (not lists) — one opening voids one host.
                split_top_level_args_into(rec.args, &mut fields_buf);
                if fields_buf.len() >= 6 {
                    if let (Field::Ref(host), Field::Ref(opening)) = (
                        parse_field(fields_buf[4]),
                        parse_field(fields_buf[5]),
                    ) {
                        out.voids_opening.push(opening);
                        out.voids_host.push(host);
                    }
                }
            }
            EntityKind::DefinesByType => {
                // IfcRelDefinesByType(GlobalId, OwnerHistory, Name, Description,
                //                     RelatedObjects, RelatingType).
                // RelatedObjects is a list of refs (typically many product
                // step ids); RelatingType is a single ref to an IfcTypeObject.
                split_top_level_args_into(rec.args, &mut fields_buf);
                if fields_buf.len() >= 6 {
                    if let Field::List(body) = parse_field(fields_buf[4]) {
                        if let Field::Ref(type_id) = parse_field(fields_buf[5]) {
                            for child in parse_ref_list(body) {
                                out.defines_by_type_product.push(child);
                                out.defines_by_type_type.push(type_id);
                            }
                        }
                    }
                }
            }
            EntityKind::TypeObject => {
                // IfcTypeObject / IfcTypeProduct / IfcXxxType all inherit
                // from IfcRoot: arg[0] GlobalId, arg[1] OwnerHistory,
                // arg[2] Name. Capture the entity name too so consumers
                // can tell IfcWallType from IfcSensorType.
                split_top_level_args_into(rec.args, &mut fields_buf);
                if let Some(guid) = string_at(&fields_buf, 0) {
                    let name = string_at(&fields_buf, 2);
                    out.type_object_step_id.push(rec.id);
                    out.type_object_entity
                        .push(type_name_uppercase_with_proper_case(t));
                    out.type_object_guid.push(guid);
                    out.type_object_name.push(name);
                }
            }
        }
    });

    // Walk aggregates again to populate storey→building from rels whose
    // relating is an IfcBuilding. We have the building set now.
    for (child, parent) in out
        .aggregates_child
        .iter()
        .zip(out.aggregates_parent.iter())
    {
        if out.building_step_id_to_guid.contains_key(parent) {
            // child might or might not be a storey — Python side decides
            // using the storey table.
            out.storey_building_storey.push(*child);
            out.storey_building_building.push(*parent);
        }
    }

    // Filter `contained_in` to storey-relating only. Filter in-place
    // (write-index pattern) so we don't allocate two fresh Vecs the size
    // of the unfiltered input — on ST28_RIV that's ~90K entries × 16 B.
    let storey_ids: HashSet<u64> = out.storey_step_id.iter().copied().collect();
    let n = out.contained_in_child.len();
    let mut write = 0;
    for read in 0..n {
        let s = out.contained_in_structure[read];
        if storey_ids.contains(&s) {
            out.contained_in_child[write] = out.contained_in_child[read];
            out.contained_in_structure[write] = s;
            write += 1;
        }
    }
    out.contained_in_child.truncate(write);
    out.contained_in_structure.truncate(write);

    // Resolve unit_scale (metres per model unit). Look through the
    // IfcUnitAssignment.Units list for a LENGTHUNIT SI unit, then map
    // (Prefix, Name) to a scale factor.
    for unit_ref in &unit_assignment_refs {
        if let Some((ut, prefix, name)) = si_units.get(unit_ref) {
            if ut.eq_ignore_ascii_case("LENGTHUNIT") {
                let base = match name.as_str() {
                    "METRE" | "METER" => 1.0,
                    "FOOT" => 0.3048,
                    "INCH" => 0.0254,
                    _ => continue,
                };
                let scale = match prefix.as_str() {
                    "" => base,
                    "EXA" => base * 1e18,
                    "PETA" => base * 1e15,
                    "TERA" => base * 1e12,
                    "GIGA" => base * 1e9,
                    "MEGA" => base * 1e6,
                    "KILO" => base * 1e3,
                    "HECTO" => base * 1e2,
                    "DECA" => base * 10.0,
                    "DECI" => base * 1e-1,
                    "CENTI" => base * 1e-2,
                    "MILLI" => base * 1e-3,
                    "MICRO" => base * 1e-6,
                    "NANO" => base * 1e-9,
                    "PICO" => base * 1e-12,
                    "FEMTO" => base * 1e-15,
                    "ATTO" => base * 1e-18,
                    _ => base,
                };
                out.unit_scale = Some(scale);
                break;
            }
        }
    }

    out
}

fn enum_at(fields: &[&[u8]], idx: usize) -> Option<String> {
    let f = fields.get(idx)?;
    match parse_field(f) {
        Field::Enum(e) => std::str::from_utf8(e).ok().map(|s| s.to_string()),
        _ => None,
    }
}

fn extract_product(
    out: &mut IndexedFile,
    step_id: u64,
    type_name: &[u8],
    fields: &[&[u8]],
    is_ifc2x3: bool,
) {
    let guid = match string_at(fields, 0) {
        Some(g) => g,
        None => return,
    };
    let entity = type_name_uppercase_with_proper_case(type_name);

    let name = string_at(fields, 2);
    let object_type = string_at(fields, 4);
    // Tag is always the LAST positional argument that isn't an enum on
    // IfcElement subtypes — but the safe, schema-agnostic move is to try
    // arg[7]: that's the position for IfcElement.Tag, and on subtypes
    // that don't inherit Tag (rare) we just get a non-string back and
    // discard it.
    let tag = string_at(fields, 7);

    // PredefinedType is the LAST enum field on most IfcElement subtypes —
    // but in IFC2X3, several entities use the trailing slot for a
    // different attribute (IfcReinforcingBar.BarRole,
    // IfcStair/IfcRamp.ShapeType, IfcDistributionPort.FlowDirection).
    // ifcopenshell's schema-aware extraction returns None for
    // PredefinedType on these in IFC2X3, so we suppress to match. The
    // IFC4 schema standardised PredefinedType in those slots, so the
    // suppression only applies to IFC2X3.
    let suppress_predefined = is_ifc2x3 && is_predefined_type_unavailable_in_ifc2x3(type_name);
    let mut predefined: Option<String> = None;
    if !suppress_predefined {
        for f in fields.iter().rev() {
            match parse_field(f) {
                Field::Enum(e) => {
                    if let Ok(s) = std::str::from_utf8(e) {
                        predefined = Some(s.to_string());
                    }
                    break;
                }
                // Skip nulls / stars but stop at anything else so we don't
                // bleed across the schema-positional boundary.
                Field::Null | Field::Star => continue,
                _ => break,
            }
        }
    }

    // Bump the type count without cloning `entity` on the hot path: only
    // the first time we see a given entity name does the HashMap own a
    // copy. Subsequent products of the same type increment in place.
    if let Some(count) = out.type_counts.get_mut(&entity) {
        *count += 1;
    } else {
        out.type_counts.insert(entity.clone(), 1);
    }
    out.product_step_id.push(step_id);
    out.product_guid.push(guid);
    out.product_entity.push(entity);
    out.product_name.push(name);
    out.product_predefined_type.push(predefined);
    out.product_object_type.push(object_type);
    out.product_tag.push(tag);
}

fn extract_storey(out: &mut IndexedFile, step_id: u64, fields: &[&[u8]]) {
    let guid = match string_at(fields, 0) {
        Some(g) => g,
        None => return,
    };
    let name = string_at(fields, 2);

    // Elevation is the LAST numeric field on IfcBuildingStorey, with
    // CompositionType (.ELEMENT./.PARTIAL./...) usually preceding it.
    // Schema differences: IFC2X3 → arg[8], IFC4 → arg[9]. Walk from the
    // right: skip trailing nulls, the next number is elevation.
    let mut elevation: Option<f64> = None;
    for f in fields.iter().rev() {
        match parse_field(f) {
            Field::Number(n) => {
                elevation = Some(n);
                break;
            }
            Field::Null | Field::Star | Field::Enum(_) => continue,
            _ => break,
        }
    }

    out.storey_step_id.push(step_id);
    out.storey_guid.push(guid);
    out.storey_name.push(name);
    out.storey_elevation.push(elevation);
    out.storey_building_step_id.push(None); // filled in by Python join
}

/// Entities whose trailing-enum slot in IFC2X3 is NOT PredefinedType.
///
/// Established by the parity audit on 2026-05-12 (Issue #8). Each entry
/// is an entity that either lacks PredefinedType entirely in IFC2X3 or
/// has it at a non-trailing position. The trailing enum slot in these
/// cases carries a different attribute name and ifcopenshell's
/// schema-aware extraction returns None.
fn is_predefined_type_unavailable_in_ifc2x3(entity: &[u8]) -> bool {
    matches!(
        entity,
        b"IFCREINFORCINGBAR"          // trailing enum is BarRole
        | b"IFCSTAIR"                 // trailing enum is ShapeType (IFC4 adds PredefinedType)
        | b"IFCRAMP"                  // same as IfcStair
        | b"IFCDISTRIBUTIONPORT"      // trailing enum is FlowDirection (IFC4 adds PredefinedType)
        | b"IFCBUILDINGELEMENTPROXY"  // trailing enum is CompositionType (IFC4 adds PredefinedType)
    )
}


fn string_at(fields: &[&[u8]], idx: usize) -> Option<String> {
    let f = fields.get(idx)?;
    match parse_field(f) {
        Field::String(s) => Some(s),
        _ => None,
    }
}

/// All known STEP-uppercase → ifcopenshell-titlecase pairs. Exposed as
/// a `&'static` slice so the lazy HashMap below can be built once.
const ENTITY_NAME_PAIRS: &[(&[u8], &str)] = &[
        // Walls
        (b"IFCWALL", "IfcWall"),
        (b"IFCWALLSTANDARDCASE", "IfcWallStandardCase"),
        (b"IFCWALLELEMENTEDCASE", "IfcWallElementedCase"),
        (b"IFCCURTAINWALL", "IfcCurtainWall"),
        // Slabs / plates
        (b"IFCSLAB", "IfcSlab"),
        (b"IFCSLABSTANDARDCASE", "IfcSlabStandardCase"),
        (b"IFCSLABELEMENTEDCASE", "IfcSlabElementedCase"),
        (b"IFCPLATE", "IfcPlate"),
        (b"IFCPLATESTANDARDCASE", "IfcPlateStandardCase"),
        // Structural
        (b"IFCBEAM", "IfcBeam"),
        (b"IFCBEAMSTANDARDCASE", "IfcBeamStandardCase"),
        (b"IFCCOLUMN", "IfcColumn"),
        (b"IFCCOLUMNSTANDARDCASE", "IfcColumnStandardCase"),
        (b"IFCMEMBER", "IfcMember"),
        (b"IFCMEMBERSTANDARDCASE", "IfcMemberStandardCase"),
        (b"IFCFOOTING", "IfcFooting"),
        (b"IFCPILE", "IfcPile"),
        // Fenestration
        (b"IFCDOOR", "IfcDoor"),
        (b"IFCDOORSTANDARDCASE", "IfcDoorStandardCase"),
        (b"IFCWINDOW", "IfcWindow"),
        (b"IFCWINDOWSTANDARDCASE", "IfcWindowStandardCase"),
        (b"IFCOPENINGELEMENT", "IfcOpeningElement"),
        (b"IFCVOIDINGFEATURE", "IfcVoidingFeature"),
        (b"IFCSURFACEFEATURE", "IfcSurfaceFeature"),
        // Stairs etc.
        (b"IFCSTAIR", "IfcStair"),
        (b"IFCSTAIRFLIGHT", "IfcStairFlight"),
        (b"IFCRAMP", "IfcRamp"),
        (b"IFCRAMPFLIGHT", "IfcRampFlight"),
        (b"IFCRAILING", "IfcRailing"),
        (b"IFCROOF", "IfcRoof"),
        (b"IFCCOVERING", "IfcCovering"),
        // Generic
        (b"IFCBUILDINGELEMENTPROXY", "IfcBuildingElementProxy"),
        (b"IFCBUILDINGELEMENTPART", "IfcBuildingElementPart"),
        (b"IFCELEMENTASSEMBLY", "IfcElementAssembly"),
        (b"IFCTRANSPORTELEMENT", "IfcTransportElement"),
        (b"IFCANNOTATION", "IfcAnnotation"),
        (b"IFCVIRTUALELEMENT", "IfcVirtualElement"),
        (b"IFCDISCRETEACCESSORY", "IfcDiscreteAccessory"),
        (b"IFCFASTENER", "IfcFastener"),
        (b"IFCMECHANICALFASTENER", "IfcMechanicalFastener"),
        (b"IFCREINFORCINGBAR", "IfcReinforcingBar"),
        (b"IFCREINFORCINGMESH", "IfcReinforcingMesh"),
        (b"IFCTENDON", "IfcTendon"),
        (b"IFCTENDONANCHOR", "IfcTendonAnchor"),
        // Distribution / MEP
        (b"IFCDISTRIBUTIONELEMENT", "IfcDistributionElement"),
        (b"IFCDISTRIBUTIONFLOWELEMENT", "IfcDistributionFlowElement"),
        (b"IFCDISTRIBUTIONCONTROLELEMENT", "IfcDistributionControlElement"),
        (b"IFCDISTRIBUTIONPORT", "IfcDistributionPort"),
        (b"IFCFLOWFITTING", "IfcFlowFitting"),
        (b"IFCFLOWSEGMENT", "IfcFlowSegment"),
        (b"IFCFLOWTERMINAL", "IfcFlowTerminal"),
        (b"IFCFLOWCONTROLLER", "IfcFlowController"),
        (b"IFCFLOWMOVINGDEVICE", "IfcFlowMovingDevice"),
        (b"IFCFLOWSTORAGEDEVICE", "IfcFlowStorageDevice"),
        (b"IFCFLOWTREATMENTDEVICE", "IfcFlowTreatmentDevice"),
        (b"IFCENERGYCONVERSIONDEVICE", "IfcEnergyConversionDevice"),
        (b"IFCPIPEFITTING", "IfcPipeFitting"),
        (b"IFCPIPESEGMENT", "IfcPipeSegment"),
        (b"IFCDUCTFITTING", "IfcDuctFitting"),
        (b"IFCDUCTSEGMENT", "IfcDuctSegment"),
        (b"IFCDUCTSILENCER", "IfcDuctSilencer"),
        (b"IFCCABLECARRIERFITTING", "IfcCableCarrierFitting"),
        (b"IFCCABLECARRIERSEGMENT", "IfcCableCarrierSegment"),
        (b"IFCCABLEFITTING", "IfcCableFitting"),
        (b"IFCCABLESEGMENT", "IfcCableSegment"),
        (b"IFCVALVE", "IfcValve"),
        (b"IFCFLOWVALVE", "IfcFlowValve"),
        (b"IFCSANITARYTERMINAL", "IfcSanitaryTerminal"),
        (b"IFCLIGHTFIXTURE", "IfcLightFixture"),
        (b"IFCOUTLET", "IfcOutlet"),
        (b"IFCSWITCHINGDEVICE", "IfcSwitchingDevice"),
        (b"IFCELECTRICAPPLIANCE", "IfcElectricAppliance"),
        (b"IFCELECTRICDISTRIBUTIONBOARD", "IfcElectricDistributionBoard"),
        (b"IFCELECTRICFLOWSTORAGEDEVICE", "IfcElectricFlowStorageDevice"),
        (b"IFCAIRTERMINAL", "IfcAirTerminal"),
        (b"IFCAIRTERMINALBOX", "IfcAirTerminalBox"),
        (b"IFCDAMPER", "IfcDamper"),
        (b"IFCFILTER", "IfcFilter"),
        (b"IFCBOILER", "IfcBoiler"),
        (b"IFCBURNER", "IfcBurner"),
        (b"IFCCHILLER", "IfcChiller"),
        (b"IFCCOMPRESSOR", "IfcCompressor"),
        (b"IFCCONDENSER", "IfcCondenser"),
        (b"IFCCOOLINGTOWER", "IfcCoolingTower"),
        (b"IFCEVAPORATOR", "IfcEvaporator"),
        (b"IFCFAN", "IfcFan"),
        (b"IFCHEATEXCHANGER", "IfcHeatExchanger"),
        (b"IFCHUMIDIFIER", "IfcHumidifier"),
        (b"IFCMOTORCONNECTION", "IfcMotorConnection"),
        (b"IFCPUMP", "IfcPump"),
        (b"IFCTANK", "IfcTank"),
        (b"IFCUNITARYEQUIPMENT", "IfcUnitaryEquipment"),
        (b"IFCSENSOR", "IfcSensor"),
        (b"IFCACTUATOR", "IfcActuator"),
        (b"IFCCONTROLLER", "IfcController"),
        (b"IFCALARM", "IfcAlarm"),
        (b"IFCFLOWMETER", "IfcFlowMeter"),
        (b"IFCPROTECTIVEDEVICE", "IfcProtectiveDevice"),
        (b"IFCPROTECTIVEDEVICETRIPPINGUNIT", "IfcProtectiveDeviceTrippingUnit"),
        (b"IFCJUNCTIONBOX", "IfcJunctionBox"),
        (b"IFCCOMMUNICATIONSAPPLIANCE", "IfcCommunicationsAppliance"),
        (b"IFCAUDIOVISUALAPPLIANCE", "IfcAudioVisualAppliance"),
        (b"IFCFIRESUPPRESSIONTERMINAL", "IfcFireSuppressionTerminal"),
        (b"IFCMEDICALDEVICE", "IfcMedicalDevice"),
        (b"IFCMOBILETELECOMMUNICATIONSAPPLIANCE", "IfcMobileTelecommunicationsAppliance"),
        (b"IFCSOLARDEVICE", "IfcSolarDevice"),
        (b"IFCSTACKTERMINAL", "IfcStackTerminal"),
        (b"IFCSPACEHEATER", "IfcSpaceHeater"),
        (b"IFCWASTETERMINAL", "IfcWasteTerminal"),
        (b"IFCUNITARYCONTROLELEMENT", "IfcUnitaryControlElement"),
        (b"IFCBUILDINGSYSTEM", "IfcBuildingSystem"),
        (b"IFCLAMP", "IfcLamp"),
        (b"IFCCOIL", "IfcCoil"),
        (b"IFCGRID", "IfcGrid"),
        (b"IFCGRIDAXIS", "IfcGridAxis"),
        // Furnishings
        (b"IFCFURNISHINGELEMENT", "IfcFurnishingElement"),
        (b"IFCFURNITURE", "IfcFurniture"),
        (b"IFCSYSTEMFURNITUREELEMENT", "IfcSystemFurnitureElement"),
        // Civil
        (b"IFCEARTHWORKSCUT", "IfcEarthworksCut"),
        (b"IFCEARTHWORKSFILL", "IfcEarthworksFill"),
        (b"IFCEARTHWORKSELEMENT", "IfcEarthworksElement"),
        (b"IFCKERB", "IfcKerb"),
        (b"IFCPAVEMENT", "IfcPavement"),
        (b"IFCRAIL", "IfcRail"),
        (b"IFCROAD", "IfcRoad"),
        (b"IFCBRIDGE", "IfcBridge"),
        (b"IFCBRIDGEPART", "IfcBridgePart"),
        (b"IFCMARINEFACILITY", "IfcMarineFacility"),
        (b"IFCMARINEPART", "IfcMarinePart"),
        (b"IFCBUILDINGSTOREY", "IfcBuildingStorey"),
        (b"IFCSITE", "IfcSite"),
        (b"IFCBUILDING", "IfcBuilding"),
        (b"IFCPROJECT", "IfcProject"),
        (b"IFCAPPLICATION", "IfcApplication"),
        (b"IFCRELCONTAINEDINSPATIALSTRUCTURE", "IfcRelContainedInSpatialStructure"),
        (b"IFCRELAGGREGATES", "IfcRelAggregates"),
];

/// Lazy lookup table from STEP uppercase bytes to ifcopenshell title-case.
/// Replaces an earlier linear scan that became a measurable cost on big
/// MEP files (87K+ products × ~130 byte-slice compares per call).
fn entity_name_map() -> &'static HashMap<&'static [u8], &'static str> {
    static MAP: OnceLock<HashMap<&'static [u8], &'static str>> = OnceLock::new();
    MAP.get_or_init(|| ENTITY_NAME_PAIRS.iter().copied().collect())
}

/// Produce the title-case IFC entity name used by `ifcopenshell` (and by
/// the rest of fastparse): the STEP file has `IFCWALLSTANDARDCASE` but
/// downstream code expects `IfcWallStandardCase`. Unknown types get
/// `IfcXxxxx` with first-letter-only capitalisation of the suffix.
fn type_name_uppercase_with_proper_case(t: &[u8]) -> String {
    if let Some(canonical) = entity_name_map().get(t) {
        return (*canonical).to_string();
    }
    // Fallback: keep "Ifc" then title-case the rest.
    if t.len() >= 3 && &t[..3] == b"IFC" {
        let suffix = &t[3..];
        let mut s = String::with_capacity(t.len());
        s.push('I');
        s.push('f');
        s.push('c');
        let mut upper_next = true;
        for &c in suffix {
            let ch = c as char;
            if upper_next {
                s.push(ch.to_ascii_uppercase());
                upper_next = false;
            } else {
                s.push(ch.to_ascii_lowercase());
            }
        }
        s
    } else {
        std::str::from_utf8(t).unwrap_or("").to_string()
    }
}
