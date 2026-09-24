//! Seed-facet planner (design §2.1, §5): the candidate step-ids of a
//! specification come from EntityTable type tokens, never from the
//! indexer's product whitelist — a class the whitelist does not carry
//! would otherwise drop out of applicability and the spec would pass
//! silently.
//!
//! The seed mirrors IfcTester's broad phase, the `filter` of the FIRST
//! applicability facet (`ifctester/ids.py:288-291`):
//!
//! * Entity, plain name: every record of exactly that class
//!   (`by_type(name, include_subtypes=False)`, `facet.py:205-207`).
//! * Entity, IFC2X3 name that is not a class but `<name>TYPE` is: the
//!   occurrences of every type object of class `<name>TYPE` or a subtype
//!   (`by_type(f"{name}Type")` — subtypes included — then
//!   `get_types`, `facet.py:208-216`).
//! * Entity, restriction: every record whose class matches
//!   (`facet.py:217-225`, over the classes present in the file).
//! * Attribute (no entity facet): every record whose class has an
//!   attribute the name matches, inherited attributes included
//!   (`Attribute.filter`, `facet.py:277-303`: the declaring entity is
//!   collected with `include_subtypes=True`). Duplicates are removed;
//!   IfcTester can collect a record twice when a name restriction matches
//!   two attributes declared at different levels (ambiguity register A7).
//!
//! The entity facet's `predefinedType`, and every other applicability
//! facet, are then applied per candidate by `eval`.

use std::collections::HashSet;

use super::compile::{AttrName, CFacet, EntityName};
use super::eval::Ctx;

/// Candidate step-ids for `first`, sorted ascending, unique.
pub fn seed(ctx: &Ctx, first: &CFacet) -> Vec<u64> {
    let table = ctx.table;
    let mut out: Vec<u64> = match first {
        CFacet::Entity(e) => match &e.name {
            EntityName::Exact(n) => table
                .iter()
                .filter(|(_, ty, _)| ty.eq_ignore_ascii_case(n.as_bytes()))
                .map(|(id, _, _)| id)
                .collect(),
            EntityName::Set(set) => {
                if set.is_empty() {
                    Vec::new()
                } else {
                    table
                        .order()
                        .iter()
                        .copied()
                        .filter(|id| {
                            ctx.class_of(*id)
                                .is_some_and(|c| set.binary_search(&c).is_ok())
                        })
                        .collect()
                }
            }
            EntityName::Mapped2x3 { type_name, .. } => {
                let mut occ = Vec::new();
                for id in table.order() {
                    let Some(c) = ctx.class_of(*id) else { continue };
                    if ctx.t.is_subtype_of(c, type_name) {
                        if let Some(o) = ctx.occurrences.get(id) {
                            occ.extend_from_slice(o);
                        }
                    }
                }
                occ
            }
        },
        CFacet::Attribute(a) => {
            let classes: HashSet<&'static str> = ctx
                .t
                .entities
                .iter()
                .copied()
                .filter(|c| ctx.t.attrs(c).iter().any(|d| attr_matches(&a.name, d.name)))
                .collect();
            if classes.is_empty() {
                Vec::new()
            } else {
                table
                    .order()
                    .iter()
                    .copied()
                    .filter(|id| ctx.class_of(*id).is_some_and(|c| classes.contains(c)))
                    .collect()
            }
        }
    };
    out.sort_unstable();
    out.dedup();
    out
}

fn attr_matches(name: &AttrName, attr: &str) -> bool {
    name.matches(attr)
}
