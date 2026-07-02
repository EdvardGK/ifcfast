//! Subset rules for `IfcRel*` relationship objects (GH #124 Phase 2b).
//!
//! Forward-reachability closure (`doc/refs.rs`) reaches everything a seed
//! *depends on*, but never the relationships that *point at* the seed — a
//! product doesn't reference the containment rel that places it, the pset
//! rel that annotates it, or the material rel that clads it. Those are
//! recovered by a separate rel pass (`doc/subset.rs`), and this module is
//! the schema knowledge that pass runs on.
//!
//! ## The uniform anchor / pull model
//!
//! Every relationship we care about has two sides. Rather than special-
//! case each type's keep logic, we classify the two sides by role:
//!
//! - **anchor** — the side that holds *concrete kept elements*: the
//!   products, the spatial children, the declared types. A rel is retained
//!   iff at least one anchor ref survives in the keep set. When the anchor
//!   is a SET, it is rewritten to `∩ keep` (participants outside the subset
//!   are spliced out); an emptied anchor set means the rel is dropped.
//! - **pull** — the *upstream* side the anchors depend on: the parent in
//!   the spatial tree, the property set, the material, the group. When a
//!   rel is retained, its pull ref is added to the keep set and
//!   forward-closed, so the definition/ancestor it names comes along.
//!
//! This single classification drives the whole pass: seeding products and
//! iterating anchor→pull to a fixpoint climbs the spatial spine to
//! `IfcProject` (via `IfcRelAggregates` / `IfcRelContainedInSpatialStructure`)
//! *and* drags in every attached pset/material/type, with the anchor-set
//! rewrite guaranteeing no dangling participant is left in a kept rel.
//!
//! ## Field indices
//!
//! All `IfcRel*` types derive from `IfcRoot`, so positions 0..=3 are always
//! `GlobalId, OwnerHistory, Name, Description`. The rel-specific fields
//! follow at index 4. Indices here are pinned against real records in
//! `tests/doc_rel_rules.rs` (local fixtures + the corpus gate) — do not
//! adjust one without a fixture that exercises it.

/// One side of a relationship: which positional arg holds it, and whether
/// the arg is a single `#ref` or an aggregate `(#a,#b,…)` SET of refs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RelField {
    /// 0-based position in the STEP argument list.
    pub index: usize,
    /// `true` if the field is a SET/LIST of refs; `false` for a single ref.
    pub is_set: bool,
}

impl RelField {
    const fn single(index: usize) -> RelField {
        RelField { index, is_set: false }
    }
    const fn set(index: usize) -> RelField {
        RelField { index, is_set: true }
    }
}

/// A subset rule for one `IfcRel*` type. See the module docs for the
/// anchor / pull roles.
#[derive(Clone, Copy, Debug)]
pub struct RelRule {
    /// Uppercased STEP type token, e.g. `b"IFCRELAGGREGATES"`.
    pub type_name: &'static [u8],
    /// The side holding concrete kept elements. Survival of any anchor ref
    /// keeps the rel; a SET anchor is pruned to `∩ keep`.
    pub anchor: RelField,
    /// The upstream side pulled into the keep set (and forward-closed) when
    /// the rel is retained.
    pub pull: RelField,
}

/// The relationship types the subset pass understands, with their field
/// layouts. Indices verified against IFC2x3/IFC4/IFC4x3 records.
///
/// **Invariant across every active rule:** `anchor` is the keep-condition
/// side — a rel is retained iff at least one anchor ref survives, and only
/// the anchor field (when a SET) is ever rewritten. `pull` names the upstream
/// dependencies added to keep when the rel activates; it may be a single ref
/// *or* a SET. A SET pull is safe because pull refs are only ever *added* to
/// keep (never rewritten) and the emitted rel names them verbatim, so every
/// pull member is present — no dangling. The subset pass iterates `pull` as a
/// list and the emit splicer only ever touches `anchor`, so both sides of the
/// classification are honoured whether `pull` is single or set.
///
/// Not every `IfcRel*` is here:
/// - Connectivity / constraint rels (`IfcRelConnects*`,
///   `IfcRelSpaceBoundary`, `IfcRelReferencedInSpatialStructure`) add
///   cross-links that would balloon a "minimal" subset. `IfcRelConnectsPath-
///   Elements` (1951 in G55_ARK) is deliberately dropped for this reason:
///   its two sides are peer neighbours (wall-to-wall joins), not a
///   dependency, so pulling the connected element would chain across the
///   whole model. A subset stays valid without it — connectivity is
///   advisory, not structural.
///
/// The pass treats an unknown `IfcRel*` as a plain node: forward-closure
/// keeps it only if something already kept references it (which, for a rel,
/// never happens), so unknown rels are simply dropped rather than
/// mis-pruned. Add a rule here (with a pinning fixture) to bring one in.
pub const REL_RULES: &[RelRule] = &[
    // Spatial decomposition: RelatingObject(4) aggregates RelatedObjects(5).
    // Anchor = the children (climb toward IfcProject); pull = the parent.
    RelRule {
        type_name: b"IFCRELAGGREGATES",
        anchor: RelField::set(5),
        pull: RelField::single(4),
    },
    // Spatial containment: RelatedElements(4) sit in RelatingStructure(5).
    // Anchor = the contained products; pull = the storey/space.
    RelRule {
        type_name: b"IFCRELCONTAINEDINSPATIALSTRUCTURE",
        anchor: RelField::set(4),
        pull: RelField::single(5),
    },
    // Property attachment: RelatedObjects(4) ← RelatingPropertyDefinition(5).
    RelRule {
        type_name: b"IFCRELDEFINESBYPROPERTIES",
        anchor: RelField::set(4),
        pull: RelField::single(5),
    },
    // Type attachment: RelatedObjects(4) ← RelatingType(5).
    RelRule {
        type_name: b"IFCRELDEFINESBYTYPE",
        anchor: RelField::set(4),
        pull: RelField::single(5),
    },
    // Material association: RelatedObjects(4) ← RelatingMaterial(5).
    RelRule {
        type_name: b"IFCRELASSOCIATESMATERIAL",
        anchor: RelField::set(4),
        pull: RelField::single(5),
    },
    // Classification association: RelatedObjects(4) ← RelatingClassification(5).
    RelRule {
        type_name: b"IFCRELASSOCIATESCLASSIFICATION",
        anchor: RelField::set(4),
        pull: RelField::single(5),
    },
    // Nesting (ports, distribution parts): RelatingObject(4) nests
    // RelatedObjects(5). Anchor = the nested children; pull = the host.
    RelRule {
        type_name: b"IFCRELNESTS",
        anchor: RelField::set(5),
        pull: RelField::single(4),
    },
    // Group/system membership: RelatedObjects(4), RelatedObjectsType(5),
    // RelatingGroup(6). NOTE the extra typed-enum field at 5 pushes the
    // group ref to index 6 — the classic off-by-one this table pins down.
    RelRule {
        type_name: b"IFCRELASSIGNSTOGROUP",
        anchor: RelField::set(4),
        pull: RelField::single(6),
    },
    // Context declares definitions (IFC4+): RelatingContext(4) declares
    // RelatedDefinitions(5). Anchor = the declared types; pull = the
    // project/library context.
    RelRule {
        type_name: b"IFCRELDECLARES",
        anchor: RelField::set(5),
        pull: RelField::single(4),
    },
    // Voids: RelatingBuildingElement(4) is voided by RelatedOpeningElement(5).
    // Both single. Anchor = the wall (if it survives, keep its hole); pull =
    // the opening (needed for CSG cut-openings to reproduce the void).
    RelRule {
        type_name: b"IFCRELVOIDSELEMENT",
        anchor: RelField::single(4),
        pull: RelField::single(5),
    },
    // Fills: RelatingOpeningElement(4) is filled by RelatedBuildingElement(5).
    // Both single. Anchor = the door/window filler (if it survives, keep the
    // opening it sits in); pull = the opening.
    RelRule {
        type_name: b"IFCRELFILLSELEMENT",
        anchor: RelField::single(5),
        pull: RelField::single(4),
    },
    // Coverings: RelatingBuildingElement(4) is covered by RelatedCoverings(5,
    // SET of IfcCovering). Anchor = the host wall/slab (single); pull = its
    // coverings (set). Parallel to Voids: if the host survives, keep the
    // finishes attached to it. Anchoring on the specific host (not the
    // coverings) means seeding one wall pulls only *that* wall's coverings —
    // no ballooning. First rule to use a SET pull. (GH #126)
    RelRule {
        type_name: b"IFCRELCOVERSBLDGELEMENTS",
        anchor: RelField::single(4),
        pull: RelField::set(5),
    },
    // System service: RelatingSystem(4, single) serves RelatedBuildings(5,
    // SET of IfcSpatialElement). Anchor = the system (activates only when a
    // system is deliberately seeded, so a one-wall subset never drags it in);
    // pull = the served buildings, which the spatial climb already keeps, so
    // the pull is near-free and just anchors the service link itself. This is
    // the member-anchoring the old deferral note called for. (GH #126)
    RelRule {
        type_name: b"IFCRELSERVICESBUILDINGS",
        anchor: RelField::single(4),
        pull: RelField::set(5),
    },
];

/// Look up the [`RelRule`] for an uppercased STEP type token, if the
/// subset pass has one. Comparison is exact on the uppercased name.
pub fn rule_for(type_name: &[u8]) -> Option<&'static RelRule> {
    REL_RULES.iter().find(|r| r.type_name == type_name)
}

/// The refs held by one [`RelField`] of an already-split argument list.
/// A single-ref field yields 0 or 1 ids; a SET field yields every `#id`
/// in its aggregate (a `$`/absent field yields none).
pub fn field_refs(args: &[&[u8]], field: RelField) -> Vec<u64> {
    let raw = match args.get(field.index) {
        Some(raw) => *raw,
        None => return Vec::new(),
    };
    match crate::lexer::parse_field(raw) {
        crate::lexer::Field::Ref(id) if !field.is_set => vec![id],
        crate::lexer::Field::List(body) if field.is_set => crate::lexer::parse_ref_list(body),
        _ => Vec::new(),
    }
}

/// The absolute byte range within `span` of rel field `field`'s value —
/// the trimmed argument slice, parens included for a SET. `None` if the
/// span isn't a well-formed record or the field is out of range. Used by
/// the subset emitter to splice a pruned anchor SET in place while leaving
/// every other byte of the record verbatim.
///
/// The returned range is computed by pointer arithmetic against `span`;
/// the split arg slices are sub-slices of `span`, so this is sound.
pub fn field_span(span: &[u8], field: RelField) -> Option<std::ops::Range<usize>> {
    let (_id, _type_name, args) = crate::lexer::parse_record_span(span)?;
    let split = crate::lexer::split_top_level_args(args);
    let raw = split.get(field.index)?;
    if raw.is_empty() {
        return None;
    }
    let base = span.as_ptr() as usize;
    let start = (raw.as_ptr() as usize).checked_sub(base)?;
    Some(start..start + raw.len())
}

/// Parse a rel record span into its `(rule, anchor_refs, pull_refs)`, or
/// `None` if the record isn't a known `IfcRel*` type. The span is the
/// verbatim record bytes as stored in a [`super::Doc`] (may include the
/// trailing `;` and whitespace).
pub fn parse_rel(span: &[u8]) -> Option<(&'static RelRule, Vec<u64>, Vec<u64>)> {
    let (_id, type_name, args) = crate::lexer::parse_record_span(span)?;
    let rule = rule_for(type_name)?;
    let split = crate::lexer::split_top_level_args(args);
    let anchor = field_refs(&split, rule.anchor);
    let pull = field_refs(&split, rule.pull);
    Some((rule, anchor, pull))
}
