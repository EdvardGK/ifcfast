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

/// IfcProduct subtypes we extract as "products": every entity that
/// descends from `IfcProduct` in IFC2X3 / IFC4 / IFC4X3 and is concrete
/// in at least one of them, minus the spatial classes the indexer routes
/// to their own tables (`IfcSite`, `IfcBuilding`, `IfcBuildingStorey`,
/// and `IfcSpace`, which it dispatches separately and ALSO emits as a
/// product row).
///
/// GENERATED from the schemas (`scripts/gen_schema_supertypes.py` →
/// `crate::schema_products`), never hand-maintained. The hand list it
/// replaced drifted three times — `IfcGeographicElement` (GH #178),
/// title-case gaps (GH #186), 62 schema classes including `IfcCooledBeam`
/// and `IfcOpeningStandardCase` (GH #201) — and every drift dropped a real product class from
/// every table with no warning while the file had other products.
/// `tests/test_product_whitelist_parity_178.py` pins it EQUAL to the
/// closure computed from `ifcfast.data.schema_supertypes`.
///
/// `pub(crate)` so the mesh dispatcher can ask the canonical question
/// "is this entity a meshable product?" instead of carrying its own
/// permissive blacklist. See [`is_meshable_product`].
pub(crate) use crate::schema_products::PRODUCT_TYPES;

/// Spatial structure types — separate output table.
const STOREY_TYPES: &[&[u8]] = &[b"IFCBUILDINGSTOREY"];

const SITE_TYPE: &[u8] = b"IFCSITE";
const BUILDING_TYPE: &[u8] = b"IFCBUILDING";
const PROJECT_TYPE: &[u8] = b"IFCPROJECT";
pub(crate) const SPACE_TYPE: &[u8] = b"IFCSPACE";
const APPLICATION_TYPE: &[u8] = b"IFCAPPLICATION";
const CONTAINED_TYPE: &[u8] = b"IFCRELCONTAINEDINSPATIALSTRUCTURE";
const AGGREGATES_TYPE: &[u8] = b"IFCRELAGGREGATES";
// Unit entities are dispatched from `crate::units::UNIT_ENTITY_TYPES`;
// these four remain only for the legacy test oracle below.
#[cfg(test)]
const SI_UNIT_TYPE: &[u8] = b"IFCSIUNIT";
#[cfg(test)]
const CONVERSION_UNIT_TYPE: &[u8] = b"IFCCONVERSIONBASEDUNIT";
#[cfg(test)]
const MEASURE_WITH_UNIT_TYPE: &[u8] = b"IFCMEASUREWITHUNIT";
#[cfg(test)]
const UNIT_ASSIGN_TYPE: &[u8] = b"IFCUNITASSIGNMENT";
const VOIDS_ELEMENT_TYPE: &[u8] = b"IFCRELVOIDSELEMENT";
const DEFINES_BY_TYPE_TYPE: &[u8] = b"IFCRELDEFINESBYTYPE";
// GH #192 slice 3. Field positions come from `doc::rel_rules`
// (`REL_RULES`), never re-derived here.
const NESTS_TYPE: &[u8] = b"IFCRELNESTS";
const ASSIGNS_TO_GROUP_TYPE: &[u8] = b"IFCRELASSIGNSTOGROUP";
const ASSIGNS_TO_GROUP_BY_FACTOR_TYPE: &[u8] = b"IFCRELASSIGNSTOGROUPBYFACTOR";
const FILLS_ELEMENT_TYPE: &[u8] = b"IFCRELFILLSELEMENT";
/// Records [`crate::body_rep`] reads to answer `has_body` (GH #202):
/// the product's shape, its representations, and — for an
/// identifier-less `MappedRepresentation` only — the mapped source.
const SHAPE_RECORD_TYPES: &[&[u8]] = &[
    b"IFCPRODUCTDEFINITIONSHAPE",
    b"IFCSHAPEREPRESENTATION",
    b"IFCMAPPEDITEM",
    b"IFCREPRESENTATIONMAP",
];

#[cfg(test)]
use crate::units::{si_length_scale_checked, SiScaleError};

/// Extract the IFC project's linear-unit-to-metres scale for callers that
/// build only the extractor tables (e.g. `extract_all` in the Python
/// wheel): one cheap walk over the unit entities instead of an entire
/// indexer pass. Resolved through [`crate::units::UnitTable`], the same
/// resolver [`index`] uses, so the two agree bit for bit. Returns `None`
/// when no LENGTHUNIT resolves — never an assumed metre (GH #149). Only
/// the Python wrapper uses this today; gated to keep
/// `--no-default-features --features csg`-style smoke builds free of
/// dead-code warnings (CI runs with `-D warnings`).
#[cfg(feature = "python")]
pub(crate) fn extract_unit_scale(table: &crate::entity_table::EntityTable) -> Option<f64> {
    // This entry point has no IndexedFile to hang warnings on; the indexer
    // pass over the same file reports them.
    let mut warnings: Vec<String> = Vec::new();
    crate::units::UnitTable::from_table(table).length_scale(&mut warnings)
}

/// Canonical "should the mesher walk this entity as a product?" check.
/// Union of [`PRODUCT_TYPES`] + IFCSPACE. Spaces are *not* in
/// `PRODUCT_TYPES` (the indexer dispatches them as a separate
/// `EntityKind::Space` for storey-of tracking) but they DO have body
/// geometry and need to appear in the substrate's instance table — so
/// the mesher's notion of "product" is a strict superset of the
/// indexer's.
///
/// Replaces the mesh module's prior permissive "starts with IFC and not
/// in this blacklist" filter, which leaked representation primitives
/// like IfcPolyloop, IfcFaceOuterBound, IfcSphericalSurface, etc. into
/// the streaming product loop. Those were silently dropped pre-fix
/// (because they had no Representation reference); the silent-drop fix
/// would have written them as junk instance rows without this filter
/// tightening.
#[cfg_attr(not(feature = "mesh"), allow(dead_code))]
pub(crate) fn is_meshable_product(type_name: &[u8]) -> bool {
    static SET: OnceLock<HashSet<&'static [u8]>> = OnceLock::new();
    SET.get_or_init(|| {
        let mut s: HashSet<&'static [u8]> = HashSet::with_capacity(PRODUCT_TYPES.len() + 1);
        for t in PRODUCT_TYPES {
            s.insert(t);
        }
        s.insert(SPACE_TYPE);
        s
    })
    .contains(type_name)
}

/// The tier-1 product whitelist, in ifcopenshell title case
/// (`IfcGeographicElement`, not `IFCGEOGRAPHICELEMENT`).
///
/// Exposed so the Python layer can assert — in CI, not by inspection —
/// that this list still covers every entity `classify.py` declares a
/// take-off product. The two lists drifted silently for a year and a
/// file made of `IfcGeographicElement` indexed to zero products
/// (GH #178). See `tests/test_product_whitelist_parity_178.py` and
/// `_core.product_types()`.
pub fn product_type_names() -> Vec<String> {
    PRODUCT_TYPES
        .iter()
        .copied()
        .map(type_name_uppercase_with_proper_case)
        .collect()
}

/// Cheap "this record has the shape of an `IfcProduct` but no rule
/// claimed it" probe (GH #178).
///
/// The indexer's dispatch miss is silent by design — >99% of records on
/// an MEP file are `IfcCartesianPoint` / `IfcPolyLoop` / property
/// values. But a miss on a real product is a silently EMPTY model, so
/// misses that look like products are counted and surfaced
/// (`IndexedFile::skipped_product_type_counts` → `summary()`).
///
/// Two stages, cheapest first:
/// 1. `IfcRoot` prefilter — every `IfcRoot` subtype starts with a
///    22-character `IfcGloballyUniqueId` string literal, so
///    `args[0] == '\''` and `args[23] == '\''` rejects the hot path at
///    two byte compares, with no argument split.
/// 2. `IfcProduct` shape — attribute 4 is `ObjectType` (a string or
///    `$`, never a list / reference / enum), and attributes 5 and 6 are
///    `ObjectPlacement` and `Representation`: both must be a reference
///    or `$`, and at least one an actual reference. That rejects
///    `IfcPropertySet` (5 args) and `IfcTypeObject` subtypes (arg 5/6
///    are LISTS), which is most of the population of `IfcRoot` entities
///    we deliberately don't index as products.
///
/// The `IfcRel*` relationships need BOTH halves. Many of them do carry
/// Ref-or-`$` at attributes 5/6 — `IfcRelAssignsToGroup` (a list at 4,
/// refs at 5/6), `IfcRelConnectsPathElements`,
/// `IfcRelSpaceBoundary(1st/2ndLevel)`, `IfcRelConnectsPorts`, and the
/// `IfcRelAssignsTo*` family with `ObjectType` = `$` — so the shape test
/// alone counts them as skipped products. The caller rejects any type
/// name starting with `IFCREL` BEFORE calling this (no split at all),
/// and the `ObjectType` check here is the belt to that braces: a
/// relationship whose attribute 4 is a list or a reference can never
/// pass, whatever it is called.
///
/// Deliberately misses a product carrying neither placement nor
/// representation — undetectable at this cost, and invisible in every
/// downstream table anyway. Also misses a record whose GlobalId is not
/// exactly 22 characters (the prefilter checks the closing quote at byte
/// 23): every IFC writer emits 22, and relaxing it would let every
/// `IfcPropertySingleValue('Name',…)` through to the split, which is the
/// hot path this filter exists to protect.
fn looks_like_unindexed_product<'a>(args: &'a [u8], fields: &mut Vec<&'a [u8]>) -> bool {
    if args.len() < 24 || args[0] != b'\'' || args[23] != b'\'' {
        return false;
    }
    split_top_level_args_into(args, fields);
    if fields.len() < 7 {
        return false;
    }
    // Attribute 4 is `IfcObject.ObjectType` — an optional STRING. A
    // list (`IfcRelAssignsToGroup.RelatedObjects`), a reference or an
    // enum there means this is not an `IfcObject` at all.
    if !matches!(parse_field(fields[4]), Field::String(_) | Field::Null) {
        return false;
    }
    let placement = parse_field(fields[5]);
    let representation = parse_field(fields[6]);
    let placement_ref = matches!(placement, Field::Ref(_));
    let representation_ref = matches!(representation, Field::Ref(_));
    let placement_ok = placement_ref || matches!(placement, Field::Null);
    let representation_ok = representation_ref || matches!(representation, Field::Null);
    placement_ok && representation_ok && (placement_ref || representation_ref)
}

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
    /// Any of [`crate::units::UNIT_ENTITY_TYPES`], fed to the unit
    /// collector under its own type token (GH #197).
    Unit,
    Aggregates,
    VoidsElement,
    DefinesByType,
    /// `IfcRelNests` (GH #192 slice 3).
    Nests,
    /// `IfcRelAssignsToGroup` and its `…ByFactor` subtype.
    AssignsToGroup,
    /// `IfcRelFillsElement`.
    FillsElement,
    /// A record in [`SHAPE_RECORD_TYPES`]: kept by id (a borrowed slice,
    /// no parse) so `has_body` can be resolved after the pass.
    ShapeRecord,
    /// Any IfcXxxType (IfcWallType, IfcDoorType, IfcSensorType, …)
    /// — matched by a byte-suffix fallback rather than dispatch-map
    /// enumeration so new IFC schema additions don't drop silently.
    /// Also covers IFC2x3-only IfcDoorStyle / IfcWindowStyle, which are
    /// IfcTypeProduct subtypes without the `*Type` suffix (see #18).
    TypeObject,
}

fn dispatch_map() -> &'static HashMap<&'static [u8], EntityKind> {
    static MAP: OnceLock<HashMap<&'static [u8], EntityKind>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut m: HashMap<&'static [u8], EntityKind> = HashMap::with_capacity(
            PRODUCT_TYPES.len() + STOREY_TYPES.len() + SHAPE_RECORD_TYPES.len() + 15,
        );
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
        for t in crate::units::UNIT_ENTITY_TYPES {
            m.insert(t, EntityKind::Unit);
        }
        m.insert(AGGREGATES_TYPE, EntityKind::Aggregates);
        m.insert(VOIDS_ELEMENT_TYPE, EntityKind::VoidsElement);
        m.insert(DEFINES_BY_TYPE_TYPE, EntityKind::DefinesByType);
        m.insert(NESTS_TYPE, EntityKind::Nests);
        m.insert(ASSIGNS_TO_GROUP_TYPE, EntityKind::AssignsToGroup);
        m.insert(ASSIGNS_TO_GROUP_BY_FACTOR_TYPE, EntityKind::AssignsToGroup);
        m.insert(FILLS_ELEMENT_TYPE, EntityKind::FillsElement);
        for t in SHAPE_RECORD_TYPES {
            m.insert(t, EntityKind::ShapeRecord);
        }
        m
    })
}

// ----------------------------------------------------------------------
// Output containers
// ----------------------------------------------------------------------

#[derive(Default)]
pub struct IndexedFile {
    // ----- Tier 0/1 manifest fields -----
    pub schema: String, // e.g. "IFC4" or "IFC2X3"
    pub project_name: Option<String>,
    pub authoring_app: Option<String>,

    // ----- Type histogram for PRODUCT types only -----
    pub type_counts: HashMap<String, u32>,

    /// Entities that carry the `IfcProduct` attribute shape but are NOT
    /// in [`PRODUCT_TYPES`], keyed by the raw uppercase STEP token
    /// (`IFCTUBEBUNDLE`) with an occurrence count (GH #178).
    ///
    /// Empty on every model whose products are all whitelisted. Non-empty
    /// alongside `product_step_id.is_empty()` is the silent-zero
    /// signature: the file HAS products, this build just doesn't know
    /// the class. Python turns that pair into a `UserWarning` at
    /// `ifcfast.open()` and reports it in `Model.summary()`.
    pub skipped_product_type_counts: HashMap<String, u32>,

    // ----- Products (column-major) -----
    pub product_step_id: Vec<u64>,
    pub product_guid: Vec<String>,
    pub product_entity: Vec<String>,
    pub product_name: Vec<Option<String>>,
    pub product_predefined_type: Vec<Option<String>>,
    pub product_object_type: Vec<Option<String>>,
    pub product_tag: Vec<Option<String>>,
    /// Step id of the product's `Representation` attribute (normally an
    /// `IfcProductDefinitionShape`), `None` when it is `$`.
    pub product_representation: Vec<Option<u64>>,
    /// The product has a 3D body representation (GH #202): decided by
    /// [`crate::body_rep::select_body`], the same function the mesher
    /// uses to pick the representation it tessellates. Read from the
    /// representation records, never by meshing.
    pub product_has_body: Vec<bool>,
    /// `RepresentationType` of the representation that made
    /// `product_has_body` true (`MappedRepresentation` for a mapped body);
    /// `None` when there is no body.
    pub product_body_rep_type: Vec<Option<String>>,

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

    /// IfcRelFillsElement: parallel arrays of `(opening_step_id[i],
    /// element_step_id[i])` — the door / window (RelatedBuildingElement)
    /// that fills the opening (RelatingOpeningElement). One row per
    /// relation, file order; the Python side resolves guids through the
    /// product table exactly as it does for `voids` (GH #192 slice 3).
    pub fills_opening: Vec<u64>,
    pub fills_element: Vec<u64>,

    /// IfcRelNests: one row per (relation, related object), file order.
    /// `nests_position` is the 0-based index in `RelatedObjects` (an
    /// ordered LIST in IFC4+, a SET in IFC2X3, where it is the written
    /// order). Guids are resolved here because either side can be any
    /// `IfcObjectDefinition` (ports, tasks, …), not only a product; a
    /// row whose side is not a rooted record in the file is dropped and
    /// counted in `warnings` (GH #192 slice 3).
    pub nests_parent: Vec<u64>,
    pub nests_child: Vec<u64>,
    pub nests_position: Vec<u32>,
    pub nests_parent_guid: Vec<String>,
    pub nests_child_guid: Vec<String>,

    /// IfcRelAssignsToGroup and IfcRelAssignsToGroupByFactor: one row
    /// per (relation, member), file order. `groups_group_entity` is the
    /// group's class in ifcopenshell spelling (`IfcDistributionSystem`,
    /// `IfcZone`, …). Same drop rule as nests.
    pub groups_group: Vec<u64>,
    pub groups_member: Vec<u64>,
    pub groups_group_guid: Vec<String>,
    pub groups_group_entity: Vec<String>,
    pub groups_member_guid: Vec<String>,

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

    /// Non-fatal defects found while indexing — an undeclared or
    /// unresolvable length unit, an unknown SI prefix, a missing
    /// FILE_SCHEMA (GH #149 / #159). Every entry was ALSO printed to
    /// stderr when it was raised; this vector is the programmatic
    /// channel for callers that want to surface or fail on them.
    pub warnings: Vec<String>,

    /// Set when the DATA walk did not end legitimately — a stray byte
    /// mid-stream, an unterminated final record, or a missing `DATA;`
    /// marker (GH #148). `Some` means EVERY table above is PARTIAL:
    /// records after the offset named in the message were never read.
    /// Callers must refuse to publish a substrate / QTO / clash result
    /// built from such an index.
    pub parse_error: Option<String>,
}

// ----------------------------------------------------------------------
// HEADER section — extract schema, originating app, etc.
// ----------------------------------------------------------------------

pub(crate) fn extract_header(buf: &[u8]) -> (String, Option<String>) {
    let mut schema = String::new();
    let mut originating: Option<String> = None;

    // Header search window: everything up to `DATA;`. The old fixed
    // 64 KB cap silently lost FILE_SCHEMA on files with a long
    // FILE_DESCRIPTION (federated exports list every discipline there),
    // and losing FILE_SCHEMA means `is_ifc2x3 == false` — an IFC2X3
    // file parsed with IFC4 attribute semantics, no error (GH #159).
    // `data_section_start` is itself string- and comment-aware, so the
    // window can never run past the real header.
    let header_end = header_window_end(buf);

    // FILE_SCHEMA (('IFC4'));   /   FILE_SCHEMA (('IFC2X3')) ;
    if let Some(start) = find_token(buf, b"FILE_SCHEMA", header_end) {
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
    if let Some(start) = find_token(buf, b"FILE_NAME", header_end) {
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

/// End of the HEADER search window: the start of the DATA section, or —
/// when there is no `DATA;` marker at all (a header-only or truncated
/// file) — the whole buffer capped at 1 MiB so a pathological input
/// can't turn a header probe into a full-file scan.
fn header_window_end(buf: &[u8]) -> usize {
    data_section_start(buf).unwrap_or_else(|| buf.len().min(1024 * 1024))
}

/// Find `needle` as a HEADER keyword within `buf[..limit]`, returning
/// the offset just past it.
///
/// String- and comment-aware: `FILE_NAME('... FILE_SCHEMA ...')` must
/// not match inside the quoted value, and neither must a `/* */`
/// banner. Also requires a token boundary before the match so
/// `MY_FILE_SCHEMA` doesn't hit.
fn find_token(buf: &[u8], needle: &[u8], limit: usize) -> Option<usize> {
    let end = limit.min(buf.len());
    let mut i = 0;
    let mut prev: u8 = 0;
    while i < end {
        match buf[i] {
            b'\'' => {
                // Quoted string: inert. Skip to the closing quote,
                // honouring the STEP `''` escape.
                i += 1;
                while i < end {
                    if buf[i] == b'\'' {
                        if i + 1 < end && buf[i + 1] == b'\'' {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                prev = b'\'';
            }
            b'/' if i + 1 < end && buf[i + 1] == b'*' => {
                i += 2;
                while i + 1 < end && !(buf[i] == b'*' && buf[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(end);
                prev = b' ';
            }
            b => {
                let boundary = !prev.is_ascii_alphanumeric() && prev != b'_';
                if boundary
                    && b == needle[0]
                    && i + needle.len() <= end
                    && &buf[i..i + needle.len()] == needle
                {
                    return Some(i + needle.len());
                }
                prev = b;
                i += 1;
            }
        }
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
    if out.schema.is_empty() {
        // No FILE_SCHEMA means we don't know whether to apply IFC2X3 or
        // IFC4 attribute semantics; we fall back to IFC4, which is a
        // guess. Say so instead of pretending (GH #159).
        let msg = "no FILE_SCHEMA found in the HEADER section — the schema \
                   version is UNKNOWN and IFC4 attribute semantics are \
                   assumed. On an IFC2X3 file that mis-reads trailing \
                   attributes (predefined_type and friends)."
            .to_string();
        out.warnings.push(msg);
    }
    if let Some(o) = originating {
        out.authoring_app = Some(o);
    }

    let dispatch = dispatch_map();

    // Snapshot schema for the extractor — it needs to know IFC2X3 vs IFC4
    // to suppress predefined_type for entities where the trailing-enum
    // slot is a different attribute in IFC2X3 (see issue #8 finding 1).
    let is_ifc2x3 = out.schema.eq_ignore_ascii_case("IFC2X3");

    // IfcSIUnit / IfcConversionBasedUnit (how imperial files declare
    // FOOT / INCH, GH #73) / IfcMeasureWithUnit / the first non-empty
    // IfcUnitAssignment, collected for `crate::units::UnitTable`, which
    // resolves unit_scale after the pass.
    let mut units = crate::units::UnitCollector::default();

    // A missing `DATA;` marker is recorded, not swallowed (GH #148).
    // We still scan from byte 0 so bare record-list fixtures keep
    // working, but the caller is told the entity stream was never
    // located — with `unwrap_or(0)` alone this produced zero records
    // and exit 0.
    let data_start = match data_section_start(buf) {
        Some(s) => s,
        None => {
            out.parse_error = Some(
                "no `DATA;` section marker found — scanning from byte 0. If \
                 this is an IFC file it is malformed or truncated in its \
                 header; entity coverage is not trustworthy."
                    .to_string(),
            );
            0
        }
    };
    let data_end = endsec_position(buf, data_start);

    // Reused across every record — saves one Vec allocation per STEP
    // entity (600K+ on ST28_RIV).
    let mut fields_buf: Vec<&[u8]> = Vec::with_capacity(16);

    // Shape / representation records by id, borrowed from `buf` — the
    // input to `has_body` once every product's Representation ref is
    // known (GH #202). Borrowed slices, no parse on the hot path.
    let mut shape_records: HashMap<u64, (&[u8], &[u8])> = HashMap::new();

    // GH #192 slice 3: nests / group rows as step ids, resolved to guids
    // after the pass against `rooted` (every record whose first argument
    // is a GlobalId-shaped string; relationships excluded). Either side
    // of these relations can sit anywhere in the file, before or after
    // the relation, and need not be a product.
    let mut raw_nests: Vec<(u64, u64, u32)> = Vec::new();
    let mut raw_groups: Vec<(u64, u64)> = Vec::new();
    let mut rooted: Vec<(u64, &[u8], &[u8])> = Vec::new();
    let nests_rule = crate::doc::rule_for(NESTS_TYPE).expect("REL_RULES pins IFCRELNESTS");
    let fills_rule =
        crate::doc::rule_for(FILLS_ELEMENT_TYPE).expect("REL_RULES pins IFCRELFILLSELEMENT");

    // Two-pass would let us resolve some refs, but a single pass is enough:
    // we only need step_id→guid maps that are built as we go, and downstream
    // (Python) does the final guid resolution for relationships.
    let scan_end = for_each_record(buf, data_start, data_end, |rec| {
        let t = rec.type_name;
        if looks_rooted(rec.args) && !t.starts_with(b"IFCREL") {
            rooted.push((rec.id, t, rec.args));
        }
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
                //
                // IFC2x3 exception: `IfcDoorStyle` / `IfcWindowStyle` are
                // IfcTypeProduct subtypes that don't follow the `*Type`
                // naming convention (collapsed into IfcDoorType /
                // IfcWindowType in IFC4). They still appear as the
                // `RelatingType` of `IfcRelDefinesByType` on IFC2x3 files,
                // so they must be classified as TypeObject here or 100% of
                // door/window typing leaks silently on IFC2x3. See #18.
                let suffix_ok = t.len() > 7 && t.starts_with(b"IFC") && t.ends_with(b"TYPE");
                let ifc2x3_style = t == b"IFCDOORSTYLE" || t == b"IFCWINDOWSTYLE";
                // Bare base classes: `IfcTypeProduct` / `IfcTypeObject` are
                // non-abstract in both IFC2x3 and IFC4. Revit emits them as
                // the `RelatingType` of `IfcRelDefinesByType` for types that
                // have no schema-specific `*Type` subtype (e.g. roof types on
                // IFC2x3, which has no IfcRoofType). They end in `PRODUCT` /
                // `OBJECT`, so they miss the `*Type` suffix check and would be
                // skipped — dropping the type record, its type_guid linkage on
                // occurrences, and any inherited psets/quantities. See #69.
                let bare_base = t == b"IFCTYPEPRODUCT" || t == b"IFCTYPEOBJECT";
                if suffix_ok || ifc2x3_style || bare_base {
                    EntityKind::TypeObject
                } else {
                    // GH #178: a miss that has the IfcProduct attribute
                    // shape is counted, not just dropped — otherwise a
                    // file made of an un-whitelisted product class
                    // indexes to zero products with no signal at all.
                    // `IfcRel*` relationships are rejected by name,
                    // before any argument split: several of them carry
                    // Ref-or-`$` at attributes 5/6 and would otherwise be
                    // counted as skipped products on every real file.
                    if t.starts_with(b"IFC")
                        && !t.starts_with(b"IFCREL")
                        && looks_like_unindexed_product(rec.args, &mut fields_buf)
                    {
                        // `from_utf8_lossy` borrows for valid UTF-8; only
                        // the first sight of a class allocates a key.
                        let key = String::from_utf8_lossy(t);
                        if let Some(n) = out.skipped_product_type_counts.get_mut(key.as_ref()) {
                            *n += 1;
                        } else {
                            out.skipped_product_type_counts.insert(key.into_owned(), 1);
                        }
                    }
                    return;
                }
            }
        };
        match kind {
            EntityKind::ShapeRecord => {
                shape_records.insert(rec.id, (t, rec.args));
            }
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
                    out.space_step_id_to_guid.insert(rec.id, guid.clone());
                    // Also extract as a product so the bundle's semantics
                    // map picks up IfcSpace alongside building elements.
                    // IfcSpace is an IfcProduct subtype in the IFC schema —
                    // it lives in a separate `EntityKind` purely so storey
                    // / aggregate resolution can fast-path it, but dropping
                    // it from the products table is a reveal-all violation:
                    // spaces have psets, materials, classifications,
                    // geometry, and semantic identity just like walls do.
                    extract_product(&mut out, rec.id, t, &fields_buf, is_ifc2x3);
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
            EntityKind::Unit => {
                // IfcSIUnit / IfcConversionBasedUnit[WithOffset] (how
                // imperial files declare FOOT / INCH, GH #73) /
                // IfcMeasureWithUnit / IfcUnitAssignment / derived …:
                // the collector parses each by its real type token.
                split_top_level_args_into(rec.args, &mut fields_buf);
                units.feed(rec.id, t, &fields_buf);
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
                    if let (Field::Ref(host), Field::Ref(opening)) =
                        (parse_field(fields_buf[4]), parse_field(fields_buf[5]))
                    {
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
            EntityKind::Nests => {
                // IfcRelNests: RelatingObject(4) nests RelatedObjects(5),
                // positions from REL_RULES (pull = host, anchor = parts).
                split_top_level_args_into(rec.args, &mut fields_buf);
                if let Some(&parent) = crate::doc::field_refs(&fields_buf, nests_rule.pull).first()
                {
                    for (pos, child) in crate::doc::field_refs(&fields_buf, nests_rule.anchor)
                        .into_iter()
                        .enumerate()
                    {
                        raw_nests.push((parent, child, pos as u32));
                    }
                }
            }
            EntityKind::AssignsToGroup => {
                // IfcRelAssignsToGroup[ByFactor]: RelatedObjects(4) ←
                // RelatingGroup(6) (REL_RULES; RelatedObjectsType sits at 5).
                split_top_level_args_into(rec.args, &mut fields_buf);
                if let Some(rule) = crate::doc::rule_for(t) {
                    if let Some(&group) = crate::doc::field_refs(&fields_buf, rule.pull).first() {
                        for member in crate::doc::field_refs(&fields_buf, rule.anchor) {
                            raw_groups.push((group, member));
                        }
                    }
                }
            }
            EntityKind::FillsElement => {
                // IfcRelFillsElement: RelatingOpeningElement(4) is filled
                // by RelatedBuildingElement(5), both single (REL_RULES).
                split_top_level_args_into(rec.args, &mut fields_buf);
                let opening = crate::doc::field_refs(&fields_buf, fills_rule.pull);
                let element = crate::doc::field_refs(&fields_buf, fills_rule.anchor);
                if let (Some(&o), Some(&e)) = (opening.first(), element.first()) {
                    out.fills_opening.push(o);
                    out.fills_element.push(e);
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

    // Surface a truncated record stream. A stray byte mid-DATA (the
    // classic doubled `;;`) stops the walk; before GH #148 that was
    // completely silent and every table below described a partial
    // model at exit 0.
    if let Some(msg) = scan_end.describe() {
        out.parse_error = Some(msg);
    }

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

    resolve_nests_and_groups(&mut out, &raw_nests, &raw_groups, &rooted);

    // All IfcRelContainedInSpatialStructure edges pass through —
    // structures can be Site, Building, Storey, or Space. The Python
    // side resolves each `contained_in_structure` step id against
    // site/building/storey/space tables to recover (container_guid,
    // container_kind). Filtering to storey-only here used to drop
    // ~5–15% of edges silently and made site/building-level
    // containment invisible to the spatial graph (GH #32).

    // GH #202: has_body / body_rep_type from the representation records.
    // One lookup of the product's shape record + its representations;
    // `crate::body_rep` is shared with the mesher so the flag and the
    // tessellated representation cannot disagree.
    out.product_has_body
        .reserve(out.product_representation.len());
    out.product_body_rep_type
        .reserve(out.product_representation.len());
    for repr in &out.product_representation {
        let body = repr.and_then(|id| crate::body_rep::select_body(&shape_records, id));
        out.product_has_body.push(body.is_some());
        out.product_body_rep_type
            .push(body.and_then(|b| b.rep_type));
    }

    // Resolve unit_scale (metres per model unit). Look through the
    // IfcUnitAssignment.Units list for a LENGTHUNIT — either an
    // IfcSIUnit (metric) or an IfcConversionBasedUnit (imperial:
    // FOOT / INCH, GH #73) — and derive metres-per-unit.
    let mut unit_warnings: Vec<String> = Vec::new();
    out.unit_scale = units.finish().length_scale(&mut unit_warnings);
    out.warnings.append(&mut unit_warnings);

    out
}

/// Does a record's argument list open with a GlobalId-shaped string
/// (`'` + 22 characters + `'` + `,`)? Cheap byte checks only; a property
/// whose name happens to be 22 characters long also passes, which is
/// harmless: only step ids a nests / group relation names are looked up.
#[inline]
fn looks_rooted(args: &[u8]) -> bool {
    let a = args.trim_ascii_start();
    a.len() > 24 && a[0] == b'\'' && a[23] == b'\'' && a[24] == b','
}

/// Resolve the nests / group rows of the pass to guids (and the group's
/// class). A row whose side is not a rooted record of the file (a
/// dangling reference) is dropped and counted in one warning.
fn resolve_nests_and_groups(
    out: &mut IndexedFile,
    raw_nests: &[(u64, u64, u32)],
    raw_groups: &[(u64, u64)],
    rooted: &[(u64, &[u8], &[u8])],
) {
    if raw_nests.is_empty() && raw_groups.is_empty() {
        return;
    }
    let mut need: HashSet<u64> = HashSet::new();
    for (p, c, _) in raw_nests {
        need.insert(*p);
        need.insert(*c);
    }
    for (g, m) in raw_groups {
        need.insert(*g);
        need.insert(*m);
    }
    // step id -> (guid, type token). First record wins on a duplicated id.
    let mut by_id: HashMap<u64, (String, &[u8])> = HashMap::with_capacity(need.len());
    for (id, t, args) in rooted {
        if !need.contains(id) || by_id.contains_key(id) {
            continue;
        }
        let fields = split_top_level_args(args);
        if let Some(guid) = string_at(&fields, 0) {
            by_id.insert(*id, (guid, t));
        }
    }
    let mut dropped = 0usize;
    for &(p, c, pos) in raw_nests {
        match (by_id.get(&p), by_id.get(&c)) {
            (Some((pg, _)), Some((cg, _))) => {
                out.nests_parent.push(p);
                out.nests_child.push(c);
                out.nests_position.push(pos);
                out.nests_parent_guid.push(pg.clone());
                out.nests_child_guid.push(cg.clone());
            }
            _ => dropped += 1,
        }
    }
    for &(g, m) in raw_groups {
        match (by_id.get(&g), by_id.get(&m)) {
            (Some((gg, gt)), Some((mg, _))) => {
                out.groups_group.push(g);
                out.groups_member.push(m);
                out.groups_group_guid.push(gg.clone());
                out.groups_group_entity
                    .push(type_name_uppercase_with_proper_case(gt));
                out.groups_member_guid.push(mg.clone());
            }
            _ => dropped += 1,
        }
    }
    if dropped > 0 {
        let msg = format!(
            "{dropped} IfcRelNests / IfcRelAssignsToGroup member row(s) name a record \
             that is missing or has no GlobalId (dangling reference); those rows \
             are left out of `nests` / `groups`."
        );
        eprintln!("ifcfast: {msg}");
        out.warnings.push(msg);
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
    // `Tag` sits at arg[7] on IfcElement subtypes and arg[8] on
    // IfcProxy; spatial elements have no Tag at all and carry `LongName`
    // at arg[7] (reading it as Tag put `"Kontor 3.04"` on every named
    // IfcSpace, GH #159). The position comes from the generated schema
    // table, so a class without the attribute gets None.
    let tag = tag_position(type_name).and_then(|pos| string_at(fields, pos));

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
        if !is_ifc2x3 && has_trailing_predefined_then_operation_enum(type_name) {
            // IFC4 IfcDoor / IfcWindow carry TWO trailing enums plus a
            // user-defined string:
            //   IfcDoor:   …, PredefinedType, OperationType,   UserDefinedOperationType
            //   IfcWindow: …, PredefinedType, PartitioningType, UserDefinedPartitioningType
            // A naive walk-from-right takes OperationType/PartitioningType
            // (wrong attribute), and when the trailing UserDefined string is
            // set it stops on the string and returns None — even though
            // PredefinedType is `.USERDEFINED.` in that case. PredefinedType
            // is the SECOND enum from the right: skip the trailing string /
            // null / star, skip exactly one enum (Operation/Partitioning),
            // then the next enum is PredefinedType. See #74.
            predefined = predefined_for_door_window(fields);
        } else {
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
    // IfcProduct.Representation is attribute 6 in every schema.
    out.product_representation
        .push(match fields.get(6).map(|f| parse_field(f)) {
            Some(Field::Ref(id)) => Some(id),
            _ => None,
        });
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
        | b"IFCROOF"                  // trailing enum is ShapeType (IFC4 adds PredefinedType) — GH #159
        | b"IFCSPACE"                 // see below — GH #159
        | b"IFCDISTRIBUTIONPORT"      // trailing enum is FlowDirection (IFC4 adds PredefinedType)
        | b"IFCBUILDINGELEMENTPROXY" // trailing enum is CompositionType (IFC4 adds PredefinedType)
    )
    // IFCSPACE, in detail. IFC2X3 IfcSpace ends
    //   …, LongName(7), CompositionType(8), InteriorOrExteriorSpace(9),
    //      ElevationWithFlooring(10: REAL, OPTIONAL)
    // and has NO PredefinedType. IFC4 replaces attr 9 with PredefinedType.
    // The walk-from-right reader skips a `$` ElevationWithFlooring (the
    // overwhelmingly common case) and then takes the next enum — which on
    // IFC2X3 is InteriorOrExteriorSpace, so every 2x3 space came out as
    // predefined_type="INTERNAL"/"EXTERNAL". ifcopenshell returns None
    // there because the attribute doesn't exist in the schema, so the
    // suppression is what restores parity.
}

/// STEP position of the `Tag` attribute on product class `entity`, or
/// `None` when the class has no `Tag` (spatial elements, ports,
/// annotations, grids, alignment / positioning elements, structural
/// items, …). Generated table, see [`crate::schema_products`].
fn tag_position(entity: &[u8]) -> Option<usize> {
    static MAP: OnceLock<HashMap<&'static [u8], usize>> = OnceLock::new();
    MAP.get_or_init(|| {
        crate::schema_products::TAG_POSITION
            .iter()
            .copied()
            .collect()
    })
    .get(entity)
    .copied()
}

/// IFC4 entities whose PredefinedType is followed by a SECOND trailing enum
/// (Operation/Partitioning type) plus a user-defined string. For these the
/// last enum is NOT PredefinedType — PredefinedType is the second enum from
/// the right. Occurrence entities only; the `*Type` styles are extracted on
/// a different path (EntityKind::TypeObject) and are not products. See #74.
fn has_trailing_predefined_then_operation_enum(entity: &[u8]) -> bool {
    // IfcDoorStandardCase / IfcWindowStandardCase add no direct attributes
    // over IfcDoor / IfcWindow, so they share the identical 13-field trailing
    // layout and the same bug.
    matches!(
        entity,
        b"IFCDOOR" | b"IFCWINDOW" | b"IFCDOORSTANDARDCASE" | b"IFCWINDOWSTANDARDCASE"
    )
}

/// Extract PredefinedType for IFC4 IfcDoor / IfcWindow.
///
/// Both have an identical trailing layout (13 direct attributes):
///   …, OverallHeight(8), OverallWidth(9),
///   PredefinedType(10), Operation/PartitioningType(11), UserDefined…(12)
/// PredefinedType is therefore the THIRD field from the end. The two enums
/// after it (Operation/Partitioning, plus the UserDefined string) may each be
/// `$`, so a "second enum from the right" walk is unreliable when
/// OperationType is unset — positional indexing from the end is the robust
/// choice. `.USERDEFINED.` is returned as-is, never collapsed to None. See #74.
fn predefined_for_door_window(fields: &[&[u8]]) -> Option<String> {
    // len - 3 == PredefinedType slot. Guard against malformed/truncated
    // records that lack the trailing block (e.g. an IFC2X3 door mislabelled
    // IFC4) by requiring at least 11 fields (indices 0..=10).
    if fields.len() < 11 {
        return None;
    }
    let idx = fields.len() - 3;
    match parse_field(fields[idx]) {
        Field::Enum(e) => std::str::from_utf8(e).ok().map(|s| s.to_string()),
        _ => None,
    }
}

fn string_at(fields: &[&[u8]], idx: usize) -> Option<String> {
    let f = fields.get(idx)?;
    match parse_field(f) {
        Field::String(s) => Some(s),
        _ => None,
    }
}

/// STEP-uppercase → ifcopenshell-titlecase for every entity in
/// IFC2X3 / IFC4 / IFC4X3, generated from the same schema walk as
/// [`PRODUCT_TYPES`] (GH #201), so a whitelisted class can never lack a
/// canonical spelling. Before this, type objects fell through to the
/// fallback caser (`IFCWALLTYPE` → `IfcWalltype`).
use crate::schema_products::ENTITY_NAMES as ENTITY_NAME_PAIRS;

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
pub(crate) fn type_name_uppercase_with_proper_case(t: &[u8]) -> String {
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

#[cfg(test)]
mod unit_scale_tests {
    use super::index;

    /// Imperial files declare length via IfcConversionBasedUnit, NOT
    /// IfcSIUnit — the case that GH #73 left as silent dead code.
    const FEET_FIXTURE: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION((''),'2;1');
FILE_NAME('feet.ifc','2026-06-13T00:00:00',(''),(''),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCPROJECT('0Test00000000000000001',$,'p',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#4));
#3=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);
#4=IFCCONVERSIONBASEDUNIT(#7,.LENGTHUNIT.,'FOOT',#9);
#7=IFCDIMENSIONALEXPONENTS(1,0,0,0,0,0,0);
#9=IFCMEASUREWITHUNIT(IFCLENGTHMEASURE(0.3048),#3);
#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);
#6=IFCAXIS2PLACEMENT3D(#8,$,$);
#8=IFCCARTESIANPOINT((0.,0.,0.));
ENDSEC;
END-ISO-10303-21;
"#;

    const INCH_FIXTURE: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION((''),'2;1');
FILE_NAME('inch.ifc','2026-06-13T00:00:00',(''),(''),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCPROJECT('0Test00000000000000001',$,'p',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#4));
#3=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);
#4=IFCCONVERSIONBASEDUNIT(#7,.LENGTHUNIT.,'INCH',#9);
#7=IFCDIMENSIONALEXPONENTS(1,0,0,0,0,0,0);
#9=IFCMEASUREWITHUNIT(IFCLENGTHMEASURE(0.0254),#3);
#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);
#6=IFCAXIS2PLACEMENT3D(#8,$,$);
#8=IFCCARTESIANPOINT((0.,0.,0.));
ENDSEC;
END-ISO-10303-21;
"#;

    const MILLIMETRE_FIXTURE: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION((''),'2;1');
FILE_NAME('mm.ifc','2026-06-13T00:00:00',(''),(''),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCPROJECT('0Test00000000000000001',$,'p',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#3));
#3=IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.);
#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);
#6=IFCAXIS2PLACEMENT3D(#8,$,$);
#8=IFCCARTESIANPOINT((0.,0.,0.));
ENDSEC;
END-ISO-10303-21;
"#;

    /// LENGTHUNIT is a conversion-based unit, but its ConversionFactor
    /// chain is broken (the IfcMeasureWithUnit ref dangles). We must NOT
    /// silently default to metres — unit_scale stays None and a loud
    /// warning is emitted ("fail loudly").
    const BROKEN_CONVERSION_FIXTURE: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION((''),'2;1');
FILE_NAME('broken.ifc','2026-06-13T00:00:00',(''),(''),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCPROJECT('0Test00000000000000001',$,'p',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#4));
#4=IFCCONVERSIONBASEDUNIT(#7,.LENGTHUNIT.,'FOOT',#9);
#7=IFCDIMENSIONALEXPONENTS(1,0,0,0,0,0,0);
#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);
#6=IFCAXIS2PLACEMENT3D(#8,$,$);
#8=IFCCARTESIANPOINT((0.,0.,0.));
ENDSEC;
END-ISO-10303-21;
"#;

    #[test]
    fn conversion_based_foot_resolves_to_0_3048() {
        let out = index(FEET_FIXTURE.as_bytes());
        let scale = out
            .unit_scale
            .expect("FOOT conversion-based unit must resolve to a scale");
        assert!(
            (scale - 0.3048).abs() < 1e-9,
            "expected 0.3048 m/ft, got {scale}"
        );
    }

    #[test]
    fn conversion_based_inch_resolves_to_0_0254() {
        let out = index(INCH_FIXTURE.as_bytes());
        let scale = out
            .unit_scale
            .expect("INCH conversion-based unit must resolve to a scale");
        assert!(
            (scale - 0.0254).abs() < 1e-9,
            "expected 0.0254 m/in, got {scale}"
        );
    }

    #[test]
    fn si_millimetre_still_resolves() {
        // Regression: the refactor must not break the metric SI path.
        let out = index(MILLIMETRE_FIXTURE.as_bytes());
        let scale = out.unit_scale.expect("MILLI METRE must resolve");
        assert!((scale - 0.001).abs() < 1e-12, "expected 0.001, got {scale}");
    }

    /// GH #69: a bare `IFCTYPEPRODUCT` typing an IfcRoof through
    /// IfcRelDefinesByType. Revit emits this on IFC2x3 (no IfcRoofType)
    /// for "Basic Roof" types. The bare base class must be captured as a
    /// TypeObject and its occurrence must link back to it.
    const BARE_TYPE_PRODUCT_FIXTURE: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION((''),'2;1');
FILE_NAME('bare.ifc','2026-06-13T00:00:00',(''),(''),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC2X3'));
ENDSEC;
DATA;
#1=IFCPROJECT('0Test00000000000000001',$,'p',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#3));
#3=IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.);
#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);
#6=IFCAXIS2PLACEMENT3D(#8,$,$);
#8=IFCCARTESIANPOINT((0.,0.,0.));
#10=IFCROOF('1Roof00000000000000001',$,'R',$,$,$,$,'t',.NOTDEFINED.);
#32=IFCTYPEPRODUCT('5Type0000000000000001',$,'Basic Roof',$,$,$,$,$);
#33=IFCRELDEFINESBYTYPE('6RelType000000000000001',$,$,$,(#10),#32);
ENDSEC;
END-ISO-10303-21;
"#;

    #[test]
    fn bare_type_product_is_captured_and_linked() {
        // Before the fix the suffix check (ends_with "TYPE") rejected
        // IFCTYPEPRODUCT, so the type was invisible AND its occurrence
        // never got a type linkage.
        let out = index(BARE_TYPE_PRODUCT_FIXTURE.as_bytes());

        // The type entity is visible.
        let pos = out
            .type_object_step_id
            .iter()
            .position(|&s| s == 32)
            .expect("bare IFCTYPEPRODUCT must appear in type_object table");
        assert_eq!(out.type_object_guid[pos], "5Type0000000000000001");
        assert_eq!(out.type_object_entity[pos], "IfcTypeProduct");
        assert_eq!(out.type_object_name[pos].as_deref(), Some("Basic Roof"));

        // The occurrence links to the type via IfcRelDefinesByType.
        let linked = out
            .defines_by_type_product
            .iter()
            .zip(out.defines_by_type_type.iter())
            .any(|(&p, &t)| p == 10 && t == 32);
        assert!(linked, "roof occurrence must link to its bare type");
    }

    /// GH #197: a zero-offset `IfcConversionBasedUnitWithOffset`
    /// LENGTHUNIT. The indexer's single pass must feed it to the unit
    /// collector under its real type token, so tier-1 `unit_scale`
    /// agrees with the standalone `UnitTable::from_table` walk.
    const WITH_OFFSET_FIXTURE: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION((''),'2;1');
FILE_NAME('offset.ifc','2026-09-27T00:00:00',(''),(''),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCPROJECT('0Test00000000000000001',$,'p',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#4));
#3=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);
#4=IFCCONVERSIONBASEDUNITWITHOFFSET(#7,.LENGTHUNIT.,'FOOT',#9,0.);
#7=IFCDIMENSIONALEXPONENTS(1,0,0,0,0,0,0);
#9=IFCMEASUREWITHUNIT(IFCLENGTHMEASURE(0.3048),#3);
#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);
#6=IFCAXIS2PLACEMENT3D(#8,$,$);
#8=IFCCARTESIANPOINT((0.,0.,0.));
ENDSEC;
END-ISO-10303-21;
"#;

    #[test]
    fn conversion_based_unit_with_offset_reaches_the_indexer() {
        let out = index(WITH_OFFSET_FIXTURE.as_bytes());
        let table = crate::entity_table::EntityTable::build(WITH_OFFSET_FIXTURE.as_bytes());
        let mut w = Vec::new();
        let expected = crate::units::UnitTable::from_table(&table).length_scale(&mut w);
        assert_eq!(expected, Some(0.3048), "{w:?}");
        assert_eq!(out.unit_scale, expected, "warnings: {:?}", out.warnings);
    }

    #[test]
    fn broken_conversion_does_not_silently_imply_metres() {
        // Fail-loud: an unresolvable conversion-based LENGTHUNIT leaves
        // unit_scale None (consumers know units are unknown) rather than
        // 1.0/metres masquerading as a real value.
        let out = index(BROKEN_CONVERSION_FIXTURE.as_bytes());
        assert!(
            out.unit_scale.is_none(),
            "broken conversion factor must NOT resolve to a scale, got {:?}",
            out.unit_scale
        );
    }
}

#[cfg(test)]
mod parse_core_fix_tests {
    use super::{index, si_length_scale_checked, SiScaleError};

    const HDR4: &str = "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION((''),'2;1');\n\
FILE_NAME('t.ifc','2026-09-06T00:00:00',(''),(''),'ifcfast','ifcfast','');\n\
FILE_SCHEMA(('IFC4'));\nENDSEC;\n";
    const HDR2X3: &str = "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION((''),'2;1');\n\
FILE_NAME('t.ifc','2026-09-06T00:00:00',(''),(''),'ifcfast','ifcfast','');\n\
FILE_SCHEMA(('IFC2X3'));\nENDSEC;\n";

    // ---- GH #149: unit scale ------------------------------------------

    /// An unrecognised IfcSIPrefix must NOT collapse to the base unit.
    /// `.KILLO.` (typo for KILO) previously resolved to 1.0 m/unit — a
    /// plausible-looking number that is wrong by 1000×.
    #[test]
    fn unknown_si_prefix_is_an_error_not_the_base_unit() {
        assert_eq!(
            si_length_scale_checked("KILLO", "METRE"),
            Err(SiScaleError::UnknownPrefix)
        );
        assert_eq!(
            si_length_scale_checked("", "RADIAN"),
            Err(SiScaleError::NotLength)
        );
        assert_eq!(si_length_scale_checked("MILLI", "METRE"), Ok(1e-3));
    }

    #[test]
    fn unknown_prefix_leaves_unit_scale_unset_and_warns() {
        let src = format!(
            "{HDR4}DATA;\n\
             #1=IFCPROJECT('0Test00000000000000001',$,'p',$,$,$,$,$,#2);\n\
             #2=IFCUNITASSIGNMENT((#3));\n\
             #3=IFCSIUNIT(*,.LENGTHUNIT.,.KILLO.,.METRE.);\n\
             ENDSEC;\nEND-ISO-10303-21;\n"
        );
        let out = index(src.as_bytes());
        assert!(
            out.unit_scale.is_none(),
            "unknown prefix must not resolve, got {:?}",
            out.unit_scale
        );
        assert!(
            out.warnings.iter().any(|w| w.contains("IfcSIPrefix")),
            "an unknown SI prefix must surface a warning, got {:?}",
            out.warnings
        );
    }

    #[test]
    fn missing_unit_assignment_warns() {
        let src = format!(
            "{HDR4}DATA;\n#1=IFCWALL('0Wall00000000000000001',$,'w',$,$,$,$,'T1');\n\
             ENDSEC;\nEND-ISO-10303-21;\n"
        );
        let out = index(src.as_bytes());
        assert!(out.unit_scale.is_none());
        assert!(
            out.warnings.iter().any(|w| w.contains("IfcUnitAssignment")),
            "an undeclared length unit must warn, got {:?}",
            out.warnings
        );
    }

    // ---- GH #148: truncation ------------------------------------------

    /// A doubled `;;` mid-DATA used to drop every later record with no
    /// signal. The index must now carry a parse_error naming the offset.
    #[test]
    fn doubled_semicolon_sets_parse_error() {
        let src = format!(
            "{HDR4}DATA;\n\
             #1=IFCWALL('0Wall00000000000000001',$,'w1',$,$,$,$,'T1');\n\
             #2=IFCSLAB('0Slab00000000000000001',$,'s1',$,$,$,$,'T2');;\n\
             #3=IFCBEAM('0Beam00000000000000001',$,'b1',$,$,$,$,'T3');\n\
             ENDSEC;\nEND-ISO-10303-21;\n"
        );
        let out = index(src.as_bytes());
        assert_eq!(
            out.product_guid.len(),
            2,
            "records before the stray byte still parse"
        );
        let err = out
            .parse_error
            .expect("a truncated record stream must set parse_error");
        assert!(err.contains("truncated"), "got {err:?}");
    }

    #[test]
    fn clean_file_has_no_parse_error() {
        let src = format!(
            "{HDR4}DATA;\n\
             #1=IFCWALL('0Wall00000000000000001',$,'w1',$,$,$,$,'T1');\n\
             ENDSEC;\nEND-ISO-10303-21;\n"
        );
        let out = index(src.as_bytes());
        assert!(out.parse_error.is_none(), "got {:?}", out.parse_error);
        assert_eq!(out.product_guid.len(), 1);
    }

    // ---- GH #159: header probe ----------------------------------------

    /// FILE_SCHEMA past the old 64 KB probe cap (long FILE_DESCRIPTION)
    /// must still be found — otherwise an IFC2X3 file is parsed with
    /// IFC4 semantics and nothing says so.
    #[test]
    fn file_schema_found_beyond_64k_of_header() {
        let filler = "x".repeat(70_000);
        let src = format!(
            "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION(('{filler}'),'2;1');\n\
             FILE_SCHEMA(('IFC2X3'));\nENDSEC;\nDATA;\n\
             #1=IFCWALL('0Wall00000000000000001',$,'w1',$,$,$,$,'T1');\n\
             ENDSEC;\nEND-ISO-10303-21;\n"
        );
        let out = index(src.as_bytes());
        assert_eq!(out.schema, "IFC2X3");
    }

    /// A `FILE_SCHEMA` mention inside a quoted HEADER value must not be
    /// mistaken for the real keyword.
    #[test]
    fn file_schema_inside_a_string_is_not_matched() {
        let src = "ISO-10303-21;\nHEADER;\n\
             FILE_DESCRIPTION(('no FILE_SCHEMA(( here'),'2;1');\n\
             FILE_SCHEMA(('IFC2X3'));\nENDSEC;\nDATA;\nENDSEC;\nEND-ISO-10303-21;\n";
        let out = index(src.as_bytes());
        assert_eq!(out.schema, "IFC2X3");
    }

    // ---- GH #159: spatial elements ------------------------------------

    /// IfcSpace arg[7] is LongName, not Tag. Reading it as Tag put room
    /// names in the `tag` column of every 2x3 and IFC4 model.
    #[test]
    fn space_longname_is_not_read_as_tag() {
        let src = format!(
            "{HDR4}DATA;\n\
             #1=IFCSPACE('0Space0000000000000001',$,'3.04',$,$,$,$,'Kontor 3.04',\
             .ELEMENT.,.INTERNAL.,$);\n\
             ENDSEC;\nEND-ISO-10303-21;\n"
        );
        let out = index(src.as_bytes());
        let i = out
            .product_entity
            .iter()
            .position(|e| e == "IfcSpace")
            .expect("space must be indexed as a product");
        assert_eq!(
            out.product_tag[i], None,
            "IfcSpace has no Tag attribute; LongName must not leak into it"
        );
        assert_eq!(out.product_name[i].as_deref(), Some("3.04"));
    }

    /// IFC2X3 IfcSpace has no PredefinedType — its trailing enum slot is
    /// InteriorOrExteriorSpace. IFC4's is PredefinedType and must keep
    /// working.
    #[test]
    fn ifc2x3_space_predefined_type_suppressed_ifc4_kept() {
        let space = "#1=IFCSPACE('0Space0000000000000001',$,'3.04',$,$,$,$,'Kontor',\
                     .ELEMENT.,.INTERNAL.,$);\n";
        let out2x3 =
            index(format!("{HDR2X3}DATA;\n{space}ENDSEC;\nEND-ISO-10303-21;\n").as_bytes());
        assert_eq!(
            out2x3.product_predefined_type[0], None,
            "IFC2X3 IfcSpace.InteriorOrExteriorSpace must not surface as predefined_type"
        );
        let out4 = index(format!("{HDR4}DATA;\n{space}ENDSEC;\nEND-ISO-10303-21;\n").as_bytes());
        assert_eq!(
            out4.product_predefined_type[0].as_deref(),
            Some("INTERNAL"),
            "IFC4 IfcSpace's trailing enum IS PredefinedType"
        );
    }

    /// IFC2X3 IfcRoof's trailing enum is ShapeType, not PredefinedType.
    #[test]
    fn ifc2x3_roof_shape_type_suppressed() {
        let roof = "#1=IFCROOF('0Roof00000000000000001',$,'r',$,$,$,$,'T1',.GABLE_ROOF.);\n";
        let out2x3 = index(format!("{HDR2X3}DATA;\n{roof}ENDSEC;\nEND-ISO-10303-21;\n").as_bytes());
        assert_eq!(out2x3.product_predefined_type[0], None);
        let out4 = index(format!("{HDR4}DATA;\n{roof}ENDSEC;\nEND-ISO-10303-21;\n").as_bytes());
        assert_eq!(
            out4.product_predefined_type[0].as_deref(),
            Some("GABLE_ROOF")
        );
    }
}

#[cfg(test)]
mod product_whitelist_tests {
    use super::{
        entity_name_map, index, product_type_names, type_name_uppercase_with_proper_case,
        PRODUCT_TYPES,
    };

    const HDR: &str = "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION((''),'2;1');\n\
FILE_NAME('t.ifc','2026-09-17T00:00:00',(''),(''),'ifcfast','ifcfast','');\n\
FILE_SCHEMA(('IFC4'));\nENDSEC;\n";

    /// Every whitelisted type must have a canonical title-case spelling.
    /// Without one the fallback title-caser emits `Ifcgeographicelement`,
    /// which no downstream consumer (`classify.py`, `m.types()`, the
    /// oracle) recognises — a second, quieter drift of the same kind as
    /// GH #178.
    #[test]
    fn every_product_type_has_a_canonical_name() {
        let map = entity_name_map();
        let missing: Vec<String> = PRODUCT_TYPES
            .iter()
            .filter(|t| !map.contains_key(**t))
            .map(|t| String::from_utf8_lossy(t).into_owned())
            .collect();
        assert!(
            missing.is_empty(),
            "PRODUCT_TYPES entries with no ENTITY_NAME_PAIRS spelling: {missing:?}"
        );
    }

    #[test]
    fn geographic_and_civil_elements_are_products() {
        assert_eq!(
            type_name_uppercase_with_proper_case(b"IFCGEOGRAPHICELEMENT"),
            "IfcGeographicElement"
        );
        let names = product_type_names();
        for want in ["IfcGeographicElement", "IfcCivilElement"] {
            assert!(names.iter().any(|n| n == want), "{want} not whitelisted");
        }
    }

    /// GH #178: the whole point of the fix — a file whose only product
    /// is an un-whitelisted class must not index to a silent zero.
    #[test]
    fn unindexed_product_classes_are_counted() {
        let src = format!(
            "{HDR}DATA;\n\
#1=IFCPROJECT('0Test00000000000000001',$,'p',$,$,$,$,(),$);\n\
#2=IFCACMEWIDGET('0Test00000000000000002',$,'tb',$,$,#3,$,$,$);\n\
#3=IFCLOCALPLACEMENT($,$);\n\
#4=IFCPROPERTYSET('0Test00000000000000003',$,'Pset_X',$,());\n\
ENDSEC;\nEND-ISO-10303-21;\n"
        );
        let idx = index(src.as_bytes());
        // A vendor class outside every schema: IfcTubeBundle was the
        // example here until GH #201 derived the whitelist from the schema.
        assert!(
            idx.product_step_id.is_empty(),
            "IfcAcmeWidget is not a schema class"
        );
        assert_eq!(
            idx.skipped_product_type_counts
                .get("IFCACMEWIDGET")
                .copied(),
            Some(1)
        );
        // IfcPropertySet is an IfcRoot with 5 args — it must NOT be
        // mistaken for a product.
        assert_eq!(idx.skipped_product_type_counts.len(), 1);
    }

    /// GH #178 (review): several `IfcRel*` relationships carry
    /// Ref-or-`$` at attributes 5/6 — `IfcRelAssignsToGroup`,
    /// `IfcRelConnectsPathElements`, `IfcRelSpaceBoundary`,
    /// `IfcRelConnectsPorts` — so the attribute-shape probe alone
    /// counts them as skipped products on every real file, drowning the
    /// signal the counter exists for.
    #[test]
    fn relationships_are_never_counted_as_skipped_products() {
        let src = format!(
            "{HDR}DATA;\n\
#1=IFCPROJECT('0Test00000000000000001',$,'p',$,$,$,$,(),$);\n\
#2=IFCWALL('0Test00000000000000002',$,'w',$,$,#3,$,$,$);\n\
#3=IFCLOCALPLACEMENT($,$);\n\
#4=IFCRELASSIGNSTOGROUP('0Test00000000000000004',$,$,$,(#5,#6),$,#7);\n\
#5=IFCWALL('0Test00000000000000005',$,'w2',$,$,#3,$,$,$);\n\
#6=IFCWALL('0Test00000000000000006',$,'w3',$,$,#3,$,$,$);\n\
#7=IFCGROUP('0Test00000000000000007',$,'g',$,$);\n\
#8=IFCRELCONNECTSPATHELEMENTS('0Test00000000000000008',$,$,$,$,#5,#6,(),(),.ATSTART.,.ATEND.);\n\
#9=IFCRELSPACEBOUNDARY('0Test00000000000000009',$,$,$,#10,#5,$,.PHYSICAL.,.EXTERNAL.);\n\
#10=IFCSPACE('0Test00000000000000010',$,'s',$,$,#3,$,$,$,$,$);\n\
#11=IFCRELCONNECTSPORTS('0Test00000000000000011',$,$,$,#12,#13,$);\n\
#12=IFCDISTRIBUTIONPORT('0Test00000000000000012',$,'p1',$,$,#3,$,$,$,$);\n\
#13=IFCDISTRIBUTIONPORT('0Test00000000000000013',$,'p2',$,$,#3,$,$,$,$);\n\
ENDSEC;\nEND-ISO-10303-21;\n"
        );
        let idx = index(src.as_bytes());
        assert!(
            idx.skipped_product_type_counts.is_empty(),
            "relationships counted as skipped products: {:?}",
            idx.skipped_product_type_counts
        );
    }

    /// GH #192 slice 3: IfcRelNests / IfcRelAssignsToGroup[ByFactor] /
    /// IfcRelFillsElement rows, positions from REL_RULES. Relations sit
    /// BEFORE their endpoints (forward refs), a group member is a
    /// non-product (a port), ByFactor counts, and a dangling member is
    /// dropped with a warning.
    #[test]
    fn nests_groups_and_fills_are_indexed() {
        let src = format!(
            "{HDR}DATA;\n\
#1=IFCRELNESTS('0Test00000000000000001',$,$,$,#10,(#12,#11));\n\
#2=IFCRELASSIGNSTOGROUP('0Test00000000000000002',$,$,$,(#10,#12),$,#20);\n\
#3=IFCRELASSIGNSTOGROUPBYFACTOR('0Test00000000000000003',$,$,$,(#10,#99),$,#21,0.5);\n\
#4=IFCRELFILLSELEMENT('0Test00000000000000004',$,$,$,#30,#31);\n\
#5=IFCRELVOIDSELEMENT('0Test00000000000000005',$,$,$,#32,#30);\n\
#10=IFCPUMP('0Test00000000000000010',$,'p',$,$,$,$,$,$);\n\
#11=IFCDISTRIBUTIONPORT('0Test00000000000000011',$,'in',$,$,$,$,$,$,$);\n\
#12=IFCDISTRIBUTIONPORT('0Test00000000000000012',$,'out',$,$,$,$,$,$,$);\n\
#20=IFCDISTRIBUTIONSYSTEM('0Test00000000000000020',$,'s',$,$,$,.WATERSUPPLY.);\n\
#21=IFCZONE('0Test00000000000000021',$,'z',$,$,$);\n\
#30=IFCOPENINGELEMENT('0Test00000000000000030',$,$,$,$,$,$,$,$);\n\
#31=IFCDOOR('0Test00000000000000031',$,$,$,$,$,$,$,$,$,$,$,$);\n\
#32=IFCWALL('0Test00000000000000032',$,$,$,$,$,$,$,$);\n\
ENDSEC;\nEND-ISO-10303-21;\n"
        );
        let idx = index(src.as_bytes());
        assert_eq!(idx.nests_parent, vec![10, 10]);
        assert_eq!(idx.nests_child, vec![12, 11]);
        assert_eq!(idx.nests_position, vec![0, 1]);
        assert_eq!(idx.nests_parent_guid[0], "0Test00000000000000010");
        assert_eq!(idx.nests_child_guid[1], "0Test00000000000000011");
        assert_eq!(idx.groups_group, vec![20, 20, 21]);
        assert_eq!(idx.groups_member, vec![10, 12, 10]);
        assert_eq!(
            idx.groups_group_entity,
            vec!["IfcDistributionSystem", "IfcDistributionSystem", "IfcZone"]
        );
        assert_eq!(idx.groups_member_guid[1], "0Test00000000000000012");
        assert_eq!(idx.fills_opening, vec![30]);
        assert_eq!(idx.fills_element, vec![31]);
        assert_eq!(idx.voids_opening, vec![30]);
        assert_eq!(idx.voids_host, vec![32]);
        assert!(
            idx.warnings.iter().any(|w| w.contains("1 IfcRelNests")),
            "{:?}",
            idx.warnings
        );
    }

    /// A whitelisted product is indexed, not counted as skipped.
    #[test]
    fn whitelisted_products_are_not_counted_as_skipped() {
        let src = format!(
            "{HDR}DATA;\n\
#1=IFCPROJECT('0Test00000000000000001',$,'p',$,$,$,$,(),$);\n\
#2=IFCGEOGRAPHICELEMENT('0Test00000000000000002',$,'terrain',$,$,#3,$,$,.TERRAIN.);\n\
#3=IFCLOCALPLACEMENT($,$);\n\
ENDSEC;\nEND-ISO-10303-21;\n"
        );
        let idx = index(src.as_bytes());
        assert_eq!(idx.product_entity, vec!["IfcGeographicElement".to_string()]);
        assert!(idx.skipped_product_type_counts.is_empty());
    }

    /// GH #201: the MEP classes the hand list missed are indexed, in
    /// ifcopenshell spelling, and none of them is counted as skipped.
    #[test]
    fn schema_derived_whitelist_indexes_gh201_classes() {
        let classes = [
            ("IFCCOOLEDBEAM", "IfcCooledBeam"),
            ("IFCAIRTOAIRHEATRECOVERY", "IfcAirToAirHeatRecovery"),
            ("IFCELECTRICTIMECONTROL", "IfcElectricTimeControl"),
            ("IFCENGINE", "IfcEngine"),
            ("IFCFLOWINSTRUMENT", "IfcFlowInstrument"),
            ("IFCINTERCEPTOR", "IfcInterceptor"),
            ("IFCTUBEBUNDLE", "IfcTubeBundle"),
            (
                "IFCELECTRICDISTRIBUTIONPOINT",
                "IfcElectricDistributionPoint",
            ),
            ("IFCCONVEYORSEGMENT", "IfcConveyorSegment"),
            ("IFCDISTRIBUTIONBOARD", "IfcDistributionBoard"),
            ("IFCOPENINGSTANDARDCASE", "IfcOpeningStandardCase"),
        ];
        let mut data = String::from(
            "#1=IFCPROJECT('0Test00000000000000001',$,'p',$,$,$,$,(),$);\n#3=IFCLOCALPLACEMENT($,$);\n",
        );
        for (i, (tok, _)) in classes.iter().enumerate() {
            data.push_str(&format!(
                "#{}={tok}('0Test{:017}',$,'x',$,$,#3,$,$,$);\n",
                10 + i,
                10 + i
            ));
        }
        let idx = index(format!("{HDR}DATA;\n{data}ENDSEC;\nEND-ISO-10303-21;\n").as_bytes());
        let want: Vec<String> = classes.iter().map(|(_, n)| n.to_string()).collect();
        assert_eq!(idx.product_entity, want);
        assert!(idx.skipped_product_type_counts.is_empty());
        assert!(!PRODUCT_TYPES.contains(&&b"IFCFLOWVALVE"[..]));
        // Spatial structure keeps its own tables.
        for t in [
            &b"IFCSITE"[..],
            b"IFCBUILDING",
            b"IFCBUILDINGSTOREY",
            b"IFCSPACE",
        ] {
            assert!(!PRODUCT_TYPES.contains(&t));
        }
    }

    /// The spelling table is the full schema, so type objects no longer go
    /// through the first-letter fallback caser (`IfcWalltype`).
    #[test]
    fn type_objects_get_ifcopenshell_spelling() {
        assert_eq!(
            type_name_uppercase_with_proper_case(b"IFCWALLTYPE"),
            "IfcWallType"
        );
        assert_eq!(
            type_name_uppercase_with_proper_case(b"IFCBUILDINGELEMENTPROXYTYPE"),
            "IfcBuildingElementProxyType"
        );
    }

    /// `Tag` is read from its schema position: arg 7 on IfcElement
    /// subtypes, arg 8 on IfcProxy, and not at all on spatial elements
    /// (arg 7 is `LongName` there, GH #159).
    #[test]
    fn tag_follows_the_schema_position() {
        let src = format!(
            "{HDR}DATA;\n\
#3=IFCLOCALPLACEMENT($,$);\n\
#4=IFCWALL('0Test00000000000000004',$,'w',$,$,#3,$,'T-wall',$);\n\
#5=IFCPROXY('0Test00000000000000005',$,'p',$,$,#3,$,.PRODUCT.,'T-proxy');\n\
#6=IFCSPATIALZONE('0Test00000000000000006',$,'z',$,$,#3,$,'Long name',.USERDEFINED.);\n\
ENDSEC;\nEND-ISO-10303-21;\n"
        );
        let idx = index(src.as_bytes());
        assert_eq!(
            idx.product_tag,
            vec![
                Some("T-wall".to_string()),
                Some("T-proxy".to_string()),
                None
            ]
        );
    }

    /// GH #202: `has_body` / `body_rep_type` come from the representation
    /// records, in either file order (shape before or after the product).
    #[test]
    fn has_body_is_read_from_representations() {
        let src = format!(
            "{HDR}DATA;\n\
#1=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.E-05,$,$);\n\
#3=IFCLOCALPLACEMENT($,$);\n\
#10=IFCSHAPEREPRESENTATION(#1,'Body','SweptSolid',());\n\
#11=IFCSHAPEREPRESENTATION(#1,'Axis','Curve2D',());\n\
#12=IFCSHAPEREPRESENTATION(#1,'Box','BoundingBox',());\n\
#13=IFCSHAPEREPRESENTATION(#1,'FootPrint','GeometricSet',());\n\
#20=IFCPRODUCTDEFINITIONSHAPE($,$,(#11,#10));\n\
#21=IFCPRODUCTDEFINITIONSHAPE($,$,(#11,#12,#13));\n\
#30=IFCWALL('0Test00000000000000030',$,'body',$,$,#3,#20,$,$);\n\
#31=IFCWALL('0Test00000000000000031',$,'axis only',$,$,#3,#21,$,$);\n\
#32=IFCWALL('0Test00000000000000032',$,'no rep',$,$,#3,$,$,$);\n\
#33=IFCFLOWTERMINAL('0Test00000000000000033',$,'mapped',$,$,#3,#40,$);\n\
#34=IFCSPACE('0Test00000000000000034',$,'space',$,$,#3,#20,$,.ELEMENT.,.INTERNAL.,$);\n\
#40=IFCPRODUCTDEFINITIONSHAPE($,$,(#41));\n\
#41=IFCSHAPEREPRESENTATION(#1,'Body','MappedRepresentation',(#42));\n\
#42=IFCMAPPEDITEM(#43,#45);\n\
#43=IFCREPRESENTATIONMAP(#44,#10);\n\
#44=IFCAXIS2PLACEMENT3D(#46,$,$);\n\
#45=IFCCARTESIANTRANSFORMATIONOPERATOR3D($,$,#46,$,$);\n\
#46=IFCCARTESIANPOINT((0.,0.,0.));\n\
ENDSEC;\nEND-ISO-10303-21;\n"
        );
        let idx = index(src.as_bytes());
        assert_eq!(idx.product_has_body, vec![true, false, false, true, true]);
        assert_eq!(
            idx.product_body_rep_type,
            vec![
                Some("SweptSolid".to_string()),
                None,
                None,
                Some("MappedRepresentation".to_string()),
                Some("SweptSolid".to_string()),
            ]
        );
    }
}

/// Pre-UnitTable `unit_scale` resolution, kept verbatim as the test oracle
/// for routing `resolve_length_scale` through [`crate::units::UnitTable`]
/// (GH #192 slice 2A). Deleting it deletes the proof.
#[cfg(test)]
mod unit_scale_legacy {
    use super::{
        string_at, CONVERSION_UNIT_TYPE, MEASURE_WITH_UNIT_TYPE, SI_UNIT_TYPE, UNIT_ASSIGN_TYPE,
    };
    use crate::lexer::{parse_field, parse_ref_list, split_top_level_args, Field};
    use crate::units::{measure_number, si_length_scale_checked, SiScaleError};
    use std::collections::HashMap;

    fn enum_at(fields: &[&[u8]], idx: usize) -> Option<String> {
        let f = fields.get(idx)?;
        match parse_field(f) {
            Field::Enum(e) => std::str::from_utf8(e).ok().map(|s| s.to_string()),
            _ => None,
        }
    }

    fn ref_at(fields: &[&[u8]], idx: usize) -> Option<u64> {
        let f = fields.get(idx)?;
        match parse_field(f) {
            Field::Ref(id) => Some(id),
            _ => None,
        }
    }
    fn push_warning(collector: &mut Vec<String>, msg: String) {
        collector.push(msg);
    }

    pub(super) fn resolve_length_scale_legacy(
        unit_assignment_refs: &[u64],
        si_units: &HashMap<u64, (String, String, String)>,
        conv_units: &HashMap<u64, (String, String, Option<u64>)>,
        measures: &HashMap<u64, (Option<f64>, Option<u64>)>,
        warnings: &mut Vec<String>,
    ) -> Option<f64> {
        for unit_ref in unit_assignment_refs {
            // SI length unit (metric files).
            if let Some((ut, prefix, name)) = si_units.get(unit_ref) {
                if ut.eq_ignore_ascii_case("LENGTHUNIT") {
                    match si_length_scale_checked(prefix, name) {
                        Ok(scale) => return Some(scale),
                        Err(SiScaleError::UnknownPrefix) => push_warning(
                            warnings,
                            format!(
                                "IfcSIUnit #{unit_ref} declares LENGTHUNIT with an \
                                 unrecognised IfcSIPrefix {prefix:?} (name {name:?}); \
                                 it cannot be converted to metres and is IGNORED \
                                 rather than treated as the un-prefixed base unit \
                                 (which would be wrong by a power of ten)."
                            ),
                        ),
                        Err(SiScaleError::NotLength) => push_warning(
                            warnings,
                            format!(
                                "IfcSIUnit #{unit_ref} declares UnitType LENGTHUNIT but \
                                 an IfcSIUnitName of {name:?}, which is not a length \
                                 unit; the declaration is inconsistent and is IGNORED."
                            ),
                        ),
                    }
                }
                continue;
            }
            // Conversion-based length unit (imperial files: FOOT / INCH).
            if let Some((ut, conv_name, factor_ref)) = conv_units.get(unit_ref) {
                if !ut.eq_ignore_ascii_case("LENGTHUNIT") {
                    continue;
                }
                let resolved =
                    factor_ref
                        .and_then(|fr| measures.get(&fr))
                        .and_then(|(value, base_ref)| {
                            let v = (*value)?;
                            let base_ref = (*base_ref)?;
                            let (base_ut, base_prefix, base_name) = si_units.get(&base_ref)?;
                            if !base_ut.eq_ignore_ascii_case("LENGTHUNIT") {
                                return None;
                            }
                            let base_scale =
                                si_length_scale_checked(base_prefix, base_name).ok()?;
                            Some(v * base_scale)
                        });
                match resolved {
                    Some(scale) => return Some(scale),
                    None => {
                        push_warning(
                            warnings,
                            format!(
                                "IfcConversionBasedUnit (LENGTHUNIT, name={conv_name:?}, \
                                 #{unit_ref}) could not be resolved to a metres-per-unit \
                                 scale; its ConversionFactor → IfcMeasureWithUnit → \
                                 IfcSIUnit chain is missing or malformed. unit_scale is \
                                 left unset (consumers default to metres, which is WRONG \
                                 for this file)."
                            ),
                        );
                        // Keep scanning: another LENGTHUNIT entry might resolve.
                    }
                }
            }
        }
        // Nothing resolved. Distinguish "the file never declared units" from
        // "it declared them and we failed" — both leave unit_scale unset, but
        // they are different defects and the reader needs to know which.
        if unit_assignment_refs.is_empty() {
            push_warning(
                warnings,
                "no IfcUnitAssignment (or an empty one) was found in this file: \
                 the project's length unit is UNDECLARED. unit_scale is left \
                 unset; consumers that default to metres will be wrong by 1000× \
                 on a millimetre-authored file."
                    .to_string(),
            );
        } else {
            push_warning(
                warnings,
                format!(
                    "the IfcUnitAssignment lists {} unit(s) but none of them \
                     resolved to a LENGTHUNIT metres-per-unit scale. unit_scale is \
                     left unset; consumers that default to metres may be wrong.",
                    unit_assignment_refs.len()
                ),
            );
        }
        None
    }

    /// The pre-UnitTable `extract_unit_scale` walk, verbatim.
    pub(super) fn legacy_unit_scale(
        table: &crate::entity_table::EntityTable,
        warnings: &mut Vec<String>,
    ) -> Option<f64> {
        let mut si_units: HashMap<u64, (String, String, String)> = HashMap::new();
        let mut conv_units: HashMap<u64, (String, String, Option<u64>)> = HashMap::new();
        let mut measures: HashMap<u64, (Option<f64>, Option<u64>)> = HashMap::new();
        let mut unit_assignment_refs: Vec<u64> = Vec::new();

        for (step_id, type_name, args) in table.iter() {
            if type_name.eq_ignore_ascii_case(SI_UNIT_TYPE) {
                let fields = split_top_level_args(args);
                let ut = enum_at(&fields, 1).unwrap_or_default();
                let prefix = enum_at(&fields, 2).unwrap_or_default();
                let name = enum_at(&fields, 3).unwrap_or_default();
                si_units.insert(step_id, (ut, prefix, name));
            } else if type_name.eq_ignore_ascii_case(CONVERSION_UNIT_TYPE) {
                let fields = split_top_level_args(args);
                let ut = enum_at(&fields, 1).unwrap_or_default();
                let name = string_at(&fields, 2).unwrap_or_default();
                let factor_ref = ref_at(&fields, 3);
                conv_units.insert(step_id, (ut, name, factor_ref));
            } else if type_name.eq_ignore_ascii_case(MEASURE_WITH_UNIT_TYPE) {
                let fields = split_top_level_args(args);
                let value = fields.first().and_then(|f| measure_number(f));
                let unit_ref = ref_at(&fields, 1);
                measures.insert(step_id, (value, unit_ref));
            } else if type_name.eq_ignore_ascii_case(UNIT_ASSIGN_TYPE)
                && unit_assignment_refs.is_empty()
            {
                let fields = split_top_level_args(args);
                if let Some(f) = fields.first() {
                    if let Field::List(body) = parse_field(f) {
                        unit_assignment_refs = parse_ref_list(body);
                    }
                }
            }
        }
        resolve_length_scale_legacy(
            &unit_assignment_refs,
            &si_units,
            &conv_units,
            &measures,
            warnings,
        )
    }

    /// Every `.ifc` under the repo's fixture trees (recursive: the IDS
    /// conformance suite adds a few hundred small files), plus
    /// `$IFCFAST_CORPUS` (colon-separated absolute paths; skipped when
    /// unset).
    fn unit_scale_files() -> Vec<std::path::PathBuf> {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            let Ok(rd) = std::fs::read_dir(dir) else {
                return;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("ifc")) {
                    out.push(p);
                }
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut out = Vec::new();
        walk(&root.join("tests/fixtures"), &mut out);
        walk(&root.join("../../tests/fixtures"), &mut out);
        if let Ok(corpus) = std::env::var("IFCFAST_CORPUS") {
            out.extend(
                corpus
                    .split(':')
                    .filter(|s| !s.is_empty())
                    .map(std::path::PathBuf::from),
            );
        }
        out.sort();
        out
    }

    /// `unit_scale` (bits) and its warnings are unchanged by routing the
    /// resolution through `UnitTable`, on both entry points: the
    /// EntityTable walk (`extract_unit_scale`) and the indexer's
    /// streaming pass.
    #[test]
    fn length_scale_matches_legacy_on_fixtures_and_corpus() {
        let files = unit_scale_files();
        assert!(files.len() >= 20, "fixture walk found only {files:?}");
        let mut resolved = 0usize;
        for path in &files {
            let buf = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let table = crate::entity_table::EntityTable::build(&buf);
            let mut w_old = Vec::new();
            let old = legacy_unit_scale(&table, &mut w_old);
            let mut w_new = Vec::new();
            let new = crate::units::UnitTable::from_table(&table).length_scale(&mut w_new);
            assert_eq!(
                old.map(f64::to_bits),
                new.map(f64::to_bits),
                "{}: unit_scale {old:?} -> {new:?}",
                path.display()
            );
            assert_eq!(w_old, w_new, "{}: warnings changed", path.display());
            let idx = super::index(&buf);
            assert_eq!(
                old.map(f64::to_bits),
                idx.unit_scale.map(f64::to_bits),
                "{}: indexer unit_scale {:?} vs legacy {old:?}",
                path.display(),
                idx.unit_scale
            );
            for w in &w_old {
                assert!(
                    idx.warnings.contains(w),
                    "{}: indexer lost warning {w:?}",
                    path.display()
                );
            }
            if old.is_some() {
                resolved += 1;
            }
        }
        eprintln!(
            "unit_scale legacy equality: {} files ({} with a resolved length unit)",
            files.len(),
            resolved
        );
    }

    /// The general resolver agrees with the length path wherever the
    /// length path resolves (they differ only on nested conversion bases
    /// and offset units, see `UnitTable::length_scale`).
    #[test]
    fn general_length_resolution_agrees_with_length_scale() {
        for path in unit_scale_files() {
            let buf = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let table = crate::entity_table::EntityTable::build(&buf);
            let units = crate::units::UnitTable::from_table(&table);
            let mut w = Vec::new();
            if let Some(s) = units.length_scale(&mut w) {
                assert_eq!(
                    units.scale_for_unit_type("LENGTHUNIT").map(f64::to_bits),
                    Some(s.to_bits()),
                    "{}",
                    path.display()
                );
            }
        }
    }
}
