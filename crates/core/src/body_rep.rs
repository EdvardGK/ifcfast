//! Which of a product's representations is its 3D body (GH #202).
//!
//! ONE definition, used by two callers that must agree:
//!
//! * the tier-1 indexer, which reports `has_body` / `body_rep_type` on
//!   the products table without meshing anything
//!   ([`crate::indexer::index`]);
//! * the mesher's representation pick (`mesh::body_items`), which
//!   tessellates exactly the representation [`select_body`] returns.
//!
//! If the two ever disagreed, "count of products with a body" (the
//! denominator of every per-element check a model-control tool runs) and
//! "products that produced geometry" would silently describe different
//! sets. When `has_body` is `false` the mesher still falls back to the
//! FIRST listed representation (a `Box`, `FootPrint`, `GeometricSet` …),
//! its pre-#202 behaviour; that fallback is outside the body contract.
//!
//! Rule: each `IfcShapeRepresentation` in `IfcProductDefinitionShape.
//! Representations` is ranked into a precedence tier; the lowest tier
//! wins, and within a tier the first listed representation wins:
//!
//! 1. `RepresentationIdentifier` is `Body` or `Facetation` (ASCII
//!    case-insensitive). This tier alone reproduces the pre-v35 mesher
//!    pick exactly (first `body` / `facetation`, else first listed).
//! 2. `RepresentationIdentifier` is `Body-FallBack`.
//! 3. `RepresentationIdentifier` missing or empty, and
//!    `RepresentationType` is a 3D solid / surface type ([`SOLID_TYPES`])
//!    or `MappedRepresentation` whose mapped source representation is
//!    itself a body in any tier (bounded depth).
//!
//! `has_body` is "some tier is non-empty". Any other identifier
//! (`BoundingBox`, `Axis`, `Box`, `FootPrint`, …) and the 2D / curve /
//! set types (`Curve2D`, `Curve3D`, `Annotation2D`, `GeometricSet`, …)
//! are never bodies. Tier 3 deliberately beats the mesher's first-rep
//! fallback: `[Axis, $ Brep]` meshes the Brep, not the axis.

use crate::entity_table::EntityTable;
use crate::lexer::{parse_field, split_top_level_args, Field};

/// Random access to STEP records by id: `(TYPE_TOKEN, args)`.
///
/// Implemented by [`EntityTable`] (mesher) and by the indexer's
/// representation-record map collected during its single pass.
pub(crate) trait RecordSource {
    fn record(&self, id: u64) -> Option<(&[u8], &[u8])>;
}

impl RecordSource for EntityTable<'_> {
    fn record(&self, id: u64) -> Option<(&[u8], &[u8])> {
        self.get(id)
    }
}

impl<'a> RecordSource for std::collections::HashMap<u64, (&'a [u8], &'a [u8])> {
    fn record(&self, id: u64) -> Option<(&[u8], &[u8])> {
        self.get(&id).copied()
    }
}

/// Tier-1 identifiers: the body proper (pre-v35 mesher rule).
pub(crate) const BODY_IDENTIFIERS: &[&str] = &["Body", "Facetation"];

/// Tier-2 identifier: an exporter's fallback body.
pub(crate) const FALLBACK_BODY_IDENTIFIER: &str = "Body-FallBack";

/// Precedence tier of a body representation (lower wins).
type Tier = u8;
const TIER_BODY: Tier = 1;
const TIER_FALLBACK: Tier = 2;
const TIER_UNNAMED_SOLID: Tier = 3;

/// `RepresentationType` values that are 3D solids or surfaces — a body
/// when the identifier is absent.
pub(crate) const SOLID_TYPES: &[&str] = &[
    "SweptSolid",
    "AdvancedSweptSolid",
    "Brep",
    "AdvancedBrep",
    "CSG",
    "Clipping",
    "SurfaceModel",
    "Tessellation",
    "SolidModel",
    "SectionedSpine",
];

/// Mapped-representation chains deeper than this are treated as "not a
/// body" rather than walked (they are malformed or cyclic in practice).
const MAX_MAPPED_DEPTH: usize = 4;

/// The representation [`select_body`] picked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BodyRep {
    /// Step id of the chosen `IfcShapeRepresentation`.
    pub rep_id: u64,
    /// Its `RepresentationType`, verbatim (`None` when the attribute is `$`).
    /// A mapped body reports `MappedRepresentation`, not the source's type.
    pub rep_type: Option<String>,
}

/// The representation ids a product's `Representation` attribute lists:
/// the `IfcProductDefinitionShape.Representations` members in order, or
/// the id itself when it points straight at a representation (rare).
pub(crate) fn representation_ids<S: RecordSource + ?Sized>(src: &S, shape_id: u64) -> Vec<u64> {
    let (type_name, args) = match src.record(shape_id) {
        Some(x) => x,
        None => return Vec::new(),
    };
    if !type_name.eq_ignore_ascii_case(b"IFCPRODUCTDEFINITIONSHAPE") {
        return vec![shape_id];
    }
    // IfcProductDefinitionShape(Name, Description, Representations)
    let fields = split_top_level_args(args);
    let body = match fields.get(2).map(|f| parse_field(f)) {
        Some(Field::List(b)) => b,
        _ => return Vec::new(),
    };
    split_top_level_args(body)
        .into_iter()
        .filter_map(|f| match parse_field(f) {
            Field::Ref(id) => Some(id),
            _ => None,
        })
        .collect()
}

/// The body representation of the product whose `Representation`
/// attribute is `shape_id` by the tier rule in the module docs, or `None`
/// when it has no body. No tessellation: a lookup of the shape record and
/// its representations (plus the mapped source, only for an
/// identifier-less `MappedRepresentation`).
pub(crate) fn select_body<S: RecordSource + ?Sized>(src: &S, shape_id: u64) -> Option<BodyRep> {
    let mut best: Option<(Tier, BodyRep)> = None;
    for rid in representation_ids(src, shape_id) {
        let Some((tier, rep_type)) = body_tier(src, rid, 0) else {
            continue;
        };
        if best.as_ref().is_some_and(|(t, _)| *t <= tier) {
            continue;
        }
        best = Some((
            tier,
            BodyRep {
                rep_id: rid,
                rep_type,
            },
        ));
        if tier == TIER_BODY {
            break;
        }
    }
    best.map(|(_, b)| b)
}

/// `Some((tier, RepresentationType))` when `rep_id` is a body representation.
fn body_tier<S: RecordSource + ?Sized>(
    src: &S,
    rep_id: u64,
    depth: usize,
) -> Option<(Tier, Option<String>)> {
    let (type_name, args) = src.record(rep_id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCSHAPEREPRESENTATION") {
        return None;
    }
    // IfcShapeRepresentation(ContextOfItems, RepresentationIdentifier,
    //                        RepresentationType, Items)
    let fields = split_top_level_args(args);
    let string_at = |i: usize| match fields.get(i).map(|f| parse_field(f)) {
        Some(Field::String(s)) if !s.is_empty() => Some(s),
        _ => None,
    };
    let ident = string_at(1);
    let rep_type = string_at(2);
    if let Some(ident) = ident {
        if BODY_IDENTIFIERS
            .iter()
            .any(|b| b.eq_ignore_ascii_case(&ident))
        {
            return Some((TIER_BODY, rep_type));
        }
        if FALLBACK_BODY_IDENTIFIER.eq_ignore_ascii_case(&ident) {
            return Some((TIER_FALLBACK, rep_type));
        }
        return None;
    }
    let rt = rep_type.as_deref()?;
    if SOLID_TYPES.iter().any(|t| t.eq_ignore_ascii_case(rt)) {
        return Some((TIER_UNNAMED_SOLID, rep_type));
    }
    if rt.eq_ignore_ascii_case("MappedRepresentation") && depth < MAX_MAPPED_DEPTH {
        let items = match fields.get(3).map(|f| parse_field(f)) {
            Some(Field::List(b)) => b,
            _ => return None,
        };
        for f in split_top_level_args(items) {
            let Field::Ref(item) = parse_field(f) else {
                continue;
            };
            if let Some(source_rep) = mapped_source_representation(src, item) {
                if body_tier(src, source_rep, depth + 1).is_some() {
                    return Some((TIER_UNNAMED_SOLID, rep_type));
                }
            }
        }
    }
    None
}

/// `IfcMappedItem.MappingSource` → `IfcRepresentationMap.MappedRepresentation`.
fn mapped_source_representation<S: RecordSource + ?Sized>(src: &S, item_id: u64) -> Option<u64> {
    let (t, args) = src.record(item_id)?;
    if !t.eq_ignore_ascii_case(b"IFCMAPPEDITEM") {
        return None;
    }
    // IfcMappedItem(MappingSource, MappingTarget)
    let Field::Ref(map_id) = parse_field(split_top_level_args(args).first()?) else {
        return None;
    };
    let (t, args) = src.record(map_id)?;
    if !t.eq_ignore_ascii_case(b"IFCREPRESENTATIONMAP") {
        return None;
    }
    // IfcRepresentationMap(MappingOrigin, MappedRepresentation)
    match parse_field(split_top_level_args(args).get(1)?) {
        Field::Ref(id) => Some(id),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(src: &str) -> EntityTable<'_> {
        EntityTable::build(src.as_bytes())
    }

    const REPS: &str = "DATA;
#1=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.E-05,$,$);
#10=IFCSHAPEREPRESENTATION(#1,'Body','SweptSolid',());
#11=IFCSHAPEREPRESENTATION(#1,'Axis','Curve2D',());
#12=IFCSHAPEREPRESENTATION(#1,'Box','BoundingBox',());
#13=IFCSHAPEREPRESENTATION(#1,'FootPrint','GeometricSet',());
#14=IFCSHAPEREPRESENTATION(#1,$,'Brep',());
#15=IFCSHAPEREPRESENTATION(#1,'','Curve3D',());
#16=IFCSHAPEREPRESENTATION(#1,'body-fallback','Tessellation',());
#20=IFCSHAPEREPRESENTATION(#1,$,'MappedRepresentation',(#21));
#21=IFCMAPPEDITEM(#22,#24);
#22=IFCREPRESENTATIONMAP(#23,#10);
#23=IFCAXIS2PLACEMENT3D(#25,$,$);
#24=IFCCARTESIANTRANSFORMATIONOPERATOR3D($,$,#25,$,$);
#25=IFCCARTESIANPOINT((0.,0.,0.));
#26=IFCSHAPEREPRESENTATION(#1,$,'MappedRepresentation',(#27));
#27=IFCMAPPEDITEM(#28,#24);
#28=IFCREPRESENTATIONMAP(#23,#11);
#100=IFCPRODUCTDEFINITIONSHAPE($,$,(#11,#10));
#101=IFCPRODUCTDEFINITIONSHAPE($,$,(#11,#12,#13));
#102=IFCPRODUCTDEFINITIONSHAPE($,$,(#15,#14));
#103=IFCPRODUCTDEFINITIONSHAPE($,$,(#20));
#104=IFCPRODUCTDEFINITIONSHAPE($,$,(#26));
#105=IFCPRODUCTDEFINITIONSHAPE($,$,(#16));
#106=IFCPRODUCTDEFINITIONSHAPE($,$,());
#30=IFCSHAPEREPRESENTATION(#1,'Body','AdvancedBrep',());
#107=IFCPRODUCTDEFINITIONSHAPE($,$,(#16,#30));
#108=IFCPRODUCTDEFINITIONSHAPE($,$,(#14,#10));
#109=IFCPRODUCTDEFINITIONSHAPE($,$,(#11,#14));
#110=IFCPRODUCTDEFINITIONSHAPE($,$,(#12,#16));
#31=IFCSHAPEREPRESENTATION(#1,'FACETATION','Tessellation',());
#111=IFCPRODUCTDEFINITIONSHAPE($,$,(#31,#10));
ENDSEC;
";

    #[test]
    fn body_identifier_wins_over_order() {
        let t = table(REPS);
        let b = select_body(&t, 100).unwrap();
        assert_eq!(b.rep_id, 10);
        assert_eq!(b.rep_type.as_deref(), Some("SweptSolid"));
    }

    #[test]
    fn axis_box_footprint_are_not_bodies() {
        let t = table(REPS);
        assert_eq!(select_body(&t, 101), None);
        assert_eq!(select_body(&t, 106), None);
        assert_eq!(select_body(&t, 999), None);
    }

    #[test]
    fn identifier_less_solid_is_a_body() {
        let t = table(REPS);
        let b = select_body(&t, 102).unwrap();
        assert_eq!(b.rep_id, 14);
        assert_eq!(b.rep_type.as_deref(), Some("Brep"));
    }

    #[test]
    fn identifier_is_case_insensitive() {
        let t = table(REPS);
        let b = select_body(&t, 105).unwrap();
        assert_eq!(b.rep_type.as_deref(), Some("Tessellation"));
    }

    #[test]
    fn body_beats_an_earlier_fallback() {
        let t = table(REPS);
        let b = select_body(&t, 107).unwrap();
        assert_eq!(b.rep_id, 30);
        assert_eq!(b.rep_type.as_deref(), Some("AdvancedBrep"));
    }

    #[test]
    fn body_beats_an_earlier_identifier_less_solid() {
        let t = table(REPS);
        let b = select_body(&t, 108).unwrap();
        assert_eq!(b.rep_id, 10);
        assert_eq!(b.rep_type.as_deref(), Some("SweptSolid"));
    }

    #[test]
    fn identifier_less_solid_beats_first_rep_fallback() {
        // Deliberate: the pre-v35 mesher would have taken the Axis.
        let t = table(REPS);
        let b = select_body(&t, 109).unwrap();
        assert_eq!(b.rep_id, 14);
        assert_eq!(b.rep_type.as_deref(), Some("Brep"));
    }

    #[test]
    fn fallback_is_a_body_when_nothing_better() {
        let t = table(REPS);
        let b = select_body(&t, 110).unwrap();
        assert_eq!(b.rep_id, 16);
        assert_eq!(b.rep_type.as_deref(), Some("Tessellation"));
    }

    #[test]
    fn first_tier_one_wins_facetation_before_body() {
        let t = table(REPS);
        let b = select_body(&t, 111).unwrap();
        assert_eq!(b.rep_id, 31);
    }

    #[test]
    fn mapped_representation_resolves_its_source() {
        let t = table(REPS);
        let b = select_body(&t, 103).unwrap();
        assert_eq!(b.rep_id, 20);
        assert_eq!(b.rep_type.as_deref(), Some("MappedRepresentation"));
        // Mapped source is an Axis → not a body.
        assert_eq!(select_body(&t, 104), None);
    }
}
