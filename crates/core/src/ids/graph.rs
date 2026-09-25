//! The lazy per-facet data layer (design §2.2 "graph.rs"): one extra pass
//! over the EntityTable per facet family a plan needs, reusing the
//! extractors' discovery passes — never a second parser.
//!
//! | Facet | Source | Built when |
//! |---|---|---|
//! | property | [`PropertyGraph`] (`GraphScope::ALL`) + [`UnitTable`] + `IfcPreDefinedPropertySet` records via the schema tables | a property facet is present |
//! | classification | `extractors::classifications::collect` | a classification facet is present |
//! | material | `extractors::materials::collect` | a material facet is present |
//!
//! What each view returns is the IfcTester-shaped reading of the data
//! (`docs/ids/facet-semantics-slice2.md`); the facet checks themselves
//! live in `eval.rs`.

use std::collections::{HashMap, HashSet};

use super::attrs::{read_attr, AttrValue, Record};
use super::eval::Ctx;
use super::schema_tables::{AttrDef, AttrKind};
use super::IdsError;
use crate::entity_table::EntityTable;
use crate::extractors::classifications::{self, ClassificationIndex};
use crate::extractors::materials::{self, MaterialScan};
use crate::extractors::property_graph::{GraphScope, PropDef, PropertyGraph, Source};
use crate::units::UnitTable;

/// Which facet families the plans use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GraphNeeds {
    pub properties: bool,
    pub classifications: bool,
    pub materials: bool,
}

/// Everything the property / classification / material facets read.
pub struct Graph<'t> {
    pub props: Option<PropData<'t>>,
    pub classes: Option<ClassData>,
    pub materials: Option<MaterialData>,
}

impl<'t> Graph<'t> {
    pub fn build(table: &'t EntityTable<'_>, needs: GraphNeeds) -> Graph<'t> {
        Graph {
            props: needs.properties.then(|| PropData::build(table)),
            classes: needs.classifications.then(|| ClassData::build(table)),
            materials: needs.materials.then(|| MaterialData::build(table)),
        }
    }
}

// --------------------------------------------------------------------------
// Properties
// --------------------------------------------------------------------------

pub struct PropData<'t> {
    pub graph: PropertyGraph<'t>,
    pub units: UnitTable,
    /// object → property definitions declared on it (IfcRelDefinesByProperties
    /// targets in file order, a type object's HasPropertySets, a material's
    /// or profile's properties), deduplicated. Any definition kind: sets,
    /// quantity sets and `IfcPreDefinedPropertySet`s alike.
    by_object: HashMap<u64, Vec<u64>>,
}

/// A property as the IDS property facet sees it.
#[derive(Debug, Clone, Copy)]
pub enum PropSrc<'g, 't> {
    /// An `IfcProperty` / `IfcPhysicalQuantity` definition.
    Def(&'g PropDef<'t>),
    /// An attribute of an `IfcPreDefinedPropertySet` (P16), read on demand.
    Predefined {
        set: u64,
        attr: &'static AttrDef,
        /// The attribute's declared type (`IFCDOORPANELOPERATIONENUM`), its
        /// dataType (ambiguity register A18).
        declared: Option<&'static str>,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct PropView<'g, 't> {
    pub name: &'g str,
    pub src: PropSrc<'g, 't>,
    pub source: Source,
}

/// One named property set of an object, after type inheritance.
#[derive(Debug, Clone)]
pub struct PsetView<'g, 't> {
    pub name: String,
    pub props: Vec<PropView<'g, 't>>,
}

impl<'t> PropData<'t> {
    fn build(table: &'t EntityTable<'_>) -> PropData<'t> {
        let graph = PropertyGraph::build_scoped(table, GraphScope::ALL);
        let units = UnitTable::from_table(table);
        let mut by_object: HashMap<u64, Vec<u64>> = HashMap::new();
        let pairs = graph
            .defines
            .iter()
            .copied()
            .chain(
                graph
                    .type_sets
                    .iter()
                    .flat_map(|(t, ids)| ids.iter().map(move |s| (*t, *s))),
            )
            .chain(graph.material_sets.iter().copied());
        for (obj, def) in pairs {
            let v = by_object.entry(obj).or_default();
            if !v.contains(&def) {
                v.push(def);
            }
        }
        PropData {
            graph,
            units,
            by_object,
        }
    }

    /// The property sets of `rec`, merged per property (ifcopenshell
    /// `get_psets`, `util/element.py:159-233`): the type's sets first,
    /// then the object's own; a set of the same name merges property by
    /// property and the later (occurrence) value wins (P3, A14). A type
    /// object sees its own sets only (P19).
    pub fn psets_for<'g>(
        &'g self,
        ctx: &Ctx<'t, '_>,
        rec: &Record,
    ) -> Result<Vec<PsetView<'g, 't>>, IdsError> {
        let mut out: Vec<PsetView<'g, 't>> = Vec::new();
        if !ctx.t.is_type_object(rec.class) {
            if let Some(&ty) = ctx.type_of.get(&rec.id) {
                self.merge_defs(ctx, ty, Source::Type, &mut out)?;
            }
        }
        self.merge_defs(ctx, rec.id, Source::Instance, &mut out)?;
        Ok(out)
    }

    fn merge_defs<'g>(
        &'g self,
        ctx: &Ctx<'t, '_>,
        object: u64,
        source: Source,
        out: &mut Vec<PsetView<'g, 't>>,
    ) -> Result<(), IdsError> {
        let Some(defs) = self.by_object.get(&object) else {
            return Ok(());
        };
        for def in defs {
            let (name, props) = if let Some(set) = self.graph.sets.get(def) {
                let family = set.kind.family();
                let props: Vec<PropView<'g, 't>> = set
                    .children
                    .iter()
                    .filter_map(|pid| self.graph.props.get(pid))
                    .filter(|d| d.class.family() == family)
                    .map(|d| PropView {
                        name: d.name.as_str(),
                        src: PropSrc::Def(d),
                        source,
                    })
                    .collect();
                (set.name.clone(), props)
            } else {
                match predefined_set(ctx, *def)? {
                    Some(v) => (
                        v.0,
                        v.1.into_iter()
                            .map(|(attr, declared)| PropView {
                                name: attr.name,
                                src: PropSrc::Predefined {
                                    set: *def,
                                    attr,
                                    declared,
                                },
                                source,
                            })
                            .collect(),
                    ),
                    // Not a property set at all (a template, a dangling ref).
                    None => continue,
                }
            };
            let slot = match out.iter().position(|s| s.name == name) {
                Some(i) => i,
                None => {
                    out.push(PsetView {
                        name,
                        props: Vec::new(),
                    });
                    out.len() - 1
                }
            };
            let dest = &mut out[slot].props;
            for p in props {
                match dest.iter().position(|q| q.name == p.name) {
                    Some(i) => dest[i] = p,
                    None => dest.push(p),
                }
            }
        }
        Ok(())
    }

    /// `IfcPropertyEnumeration.Unit` of an enumerated value's reference.
    pub fn enumeration_unit(&self, ctx: &Ctx, enumeration: u64) -> Result<Option<u64>, IdsError> {
        match ctx.class_of(enumeration) {
            Some("IFCPROPERTYENUMERATION") => {
                match read_attr(ctx.table, enumeration, 2, AttrKind::Ref)? {
                    AttrValue::Ref(u) => Ok(Some(u)),
                    _ => Ok(None),
                }
            }
            _ => Ok(None),
        }
    }
}

type PredefSet = (String, Vec<(&'static AttrDef, Option<&'static str>)>);

/// An `IfcPreDefinedPropertySet` subtype: its Name and the attributes from
/// STEP index 4 on that can hold a value (entity references are not
/// properties, as IfcTester's `get_info` filter, `facet.py:914-919`).
fn predefined_set(ctx: &Ctx, def: u64) -> Result<Option<PredefSet>, IdsError> {
    let Some(class) = ctx.class_of(def) else {
        return Ok(None);
    };
    let Some(declared) = ctx.t.predef_pset_attrs(class) else {
        return Ok(None);
    };
    let name = match read_attr(ctx.table, def, 2, AttrKind::String)? {
        AttrValue::Str(s) => s,
        _ => String::new(),
    };
    let attrs = ctx
        .t
        .attrs(class)
        .iter()
        .filter(|a| a.pos >= 4 && !a.derived && a.kind != AttrKind::Ref)
        .map(|a| {
            let d = declared.iter().find(|(n, _)| *n == a.name).map(|(_, t)| *t);
            (a, d)
        })
        .collect();
    Ok(Some((name, attrs)))
}

// --------------------------------------------------------------------------
// Classifications
// --------------------------------------------------------------------------

pub struct ClassData {
    pub(crate) index: ClassificationIndex,
    /// object → RelatingClassification ids, relations in file order.
    by_object: HashMap<u64, Vec<u64>>,
    /// resource → IfcExternalReferenceRelationship.RelatingReference ids.
    external: HashMap<u64, Vec<u64>>,
}

/// One classification reference (or a directly associated
/// IfcClassification) that counts for an object.
#[derive(Debug, Clone)]
pub struct ClassRef<'g> {
    pub id: u64,
    /// `Identification` / `ItemReference`; `None` for an IfcClassification.
    pub value: Option<&'g str>,
    /// The chain root (`None`: the chain never reaches an IfcClassification).
    pub root: Option<u64>,
    /// `IfcClassification.Name` of the chain root.
    pub system: Option<&'g str>,
    pub source: Source,
}

impl<'g> ClassRef<'g> {
    /// IfcTester's `systems` entry for this reference: `None` when the
    /// reference has no root (filtered out), else the root's Name (which
    /// may itself be null).
    pub fn system_known(&self) -> Option<Option<&'g str>> {
        self.root.map(|_| self.system)
    }
}

impl ClassData {
    fn build(table: &EntityTable) -> ClassData {
        let index = classifications::collect(table);
        let mut by_object: HashMap<u64, Vec<u64>> = HashMap::new();
        for (o, c) in &index.rel_pairs {
            by_object.entry(*o).or_default().push(*c);
        }
        let mut external: HashMap<u64, Vec<u64>> = HashMap::new();
        for (o, c) in &index.external_ref_pairs {
            external.entry(*o).or_default().push(*c);
        }
        ClassData {
            index,
            by_object,
            external,
        }
    }

    fn is_classification(&self, id: u64) -> bool {
        self.index.refs.contains_key(&id) || self.index.systems.contains_key(&id)
    }

    /// Root IfcClassification of a reference (or the classification
    /// itself): `ReferencedSource` walked up, cycle-guarded (A21).
    fn root(&self, id: u64) -> Option<u64> {
        let mut cur = id;
        let mut seen: Vec<u64> = Vec::new();
        loop {
            if self.index.systems.contains_key(&cur) {
                return Some(cur);
            }
            if seen.contains(&cur) || seen.len() > 64 {
                return None;
            }
            seen.push(cur);
            cur = self.index.refs.get(&cur)?.parent_id?;
        }
    }

    /// Leaf references of `rec` (ifcopenshell `get_references`,
    /// `util/classification.py:24-58`): non-rooted resources through
    /// `HasExternalReferences` (C9, no inheritance); IfcRoot objects
    /// through `IfcRelAssociatesClassification`, occurrences inheriting
    /// their type's references per system (C6, A20).
    fn leaves(&self, ctx: &Ctx, rec: &Record) -> Vec<(u64, Source)> {
        if !ctx.t.is_root(rec.class) {
            return self
                .external
                .get(&rec.id)
                .into_iter()
                .flatten()
                .copied()
                .filter(|c| self.is_classification(*c))
                .map(|c| (c, Source::Instance))
                .collect();
        }
        let own: Vec<u64> = self
            .by_object
            .get(&rec.id)
            .into_iter()
            .flatten()
            .copied()
            .filter(|c| self.is_classification(*c))
            .collect();
        let ty = if ctx.t.is_type_object(rec.class) {
            None
        } else {
            ctx.type_of.get(&rec.id).copied()
        };
        let type_refs: Vec<u64> = ty
            .and_then(|t| self.by_object.get(&t))
            .into_iter()
            .flatten()
            .copied()
            .filter(|c| self.is_classification(*c))
            .collect();
        let own_systems: HashSet<Option<u64>> = own.iter().map(|c| self.root(*c)).collect();
        let mut out: Vec<(u64, Source)> = type_refs
            .into_iter()
            .filter(|c| !own_systems.contains(&self.root(*c)))
            .map(|c| (c, Source::Type))
            .collect();
        out.extend(own.into_iter().map(|c| (c, Source::Instance)));
        out
    }

    /// Every reference that counts: the leaves plus each leaf's ancestor
    /// references (C5; the root IfcClassification is not an ancestor
    /// reference). Deduplicated, ascending step id.
    pub fn refs_for<'g>(&'g self, ctx: &Ctx, rec: &Record) -> Vec<ClassRef<'g>> {
        let mut seen: HashMap<u64, Source> = HashMap::new();
        for (leaf, src) in self.leaves(ctx, rec) {
            let mut cur = Some(leaf);
            let mut depth = 0;
            while let Some(id) = cur {
                if depth > 64 {
                    break;
                }
                depth += 1;
                if !self.is_classification(id) {
                    break;
                }
                let slot = seen.entry(id).or_insert(src);
                if src == Source::Instance {
                    *slot = Source::Instance;
                }
                cur = match self.index.refs.get(&id) {
                    Some(r) => match r.parent_id {
                        Some(p) if !self.index.systems.contains_key(&p) => Some(p),
                        _ => None,
                    },
                    None => None,
                };
            }
        }
        let mut ids: Vec<(u64, Source)> = seen.into_iter().collect();
        ids.sort_unstable_by_key(|(id, _)| *id);
        ids.into_iter()
            .map(|(id, source)| {
                let value = self
                    .index
                    .refs
                    .get(&id)
                    .and_then(|r| r.identification.as_deref());
                let root = self.root(id);
                let system = root
                    .and_then(|s| self.index.systems.get(&s))
                    .and_then(|s| s.name.as_deref());
                ClassRef {
                    id,
                    value,
                    root,
                    system,
                    source,
                }
            })
            .collect()
    }
}

// --------------------------------------------------------------------------
// Materials
// --------------------------------------------------------------------------

pub struct MaterialData {
    pub(crate) scan: MaterialScan,
    /// object → its FIRST IfcRelAssociatesMaterial.RelatingMaterial
    /// (ifcopenshell `get_material`, `util/element.py:704-741`).
    first: HashMap<u64, u64>,
}

impl MaterialData {
    fn build(table: &EntityTable) -> MaterialData {
        let scan = materials::collect(table);
        let mut first: HashMap<u64, u64> = HashMap::new();
        for (o, m) in &scan.rel_pairs {
            first.entry(*o).or_insert(*m);
        }
        MaterialData { scan, first }
    }

    /// The material of `rec`: its own first association, else its type's
    /// (all or nothing, M3 / A22). A usage resolves to its set (M1).
    pub fn material_of(&self, ctx: &Ctx, rec: &Record) -> Option<(u64, Source)> {
        let found = match self.first.get(&rec.id) {
            Some(m) => Some((*m, Source::Instance)),
            None if !ctx.t.is_type_object(rec.class) => ctx
                .type_of
                .get(&rec.id)
                .and_then(|t| self.first.get(t))
                .map(|m| (*m, Source::Type)),
            None => None,
        };
        found.map(|(m, s)| {
            let set = self
                .scan
                .index
                .layer_set_usages
                .get(&m)
                .copied()
                .unwrap_or(m);
            (set, s)
        })
    }

    /// The candidate strings of a material definition (M2, A23):
    /// deduplicated, sorted, `None` dropped.
    pub fn strings<'g>(&'g self, m: u64) -> Vec<&'g str> {
        let ix = &self.scan.index;
        let mut out: Vec<&'g str> = Vec::new();
        let mat = |id: Option<u64>, out: &mut Vec<&'g str>| {
            if let Some(r) = id.and_then(|i| ix.materials.get(&i)) {
                out.extend(r.name.as_deref());
                out.extend(r.category.as_deref());
            }
        };
        if ix.materials.contains_key(&m) {
            mat(Some(m), &mut out);
        } else if let Some(list) = ix.material_lists.get(&m) {
            for id in list {
                mat(Some(*id), &mut out);
            }
        } else if let Some(layer_ids) = ix.layer_sets.get(&m) {
            out.extend(ix.set_names.get(&m).and_then(|n| n.as_deref()));
            for l in layer_ids.iter().filter_map(|i| ix.layers.get(i)) {
                out.extend(l.name_override.as_deref());
                out.extend(l.category_override.as_deref());
                mat(l.material_ref, &mut out);
            }
        } else if let Some(items) = ix
            .constituent_sets
            .get(&m)
            .map(|v| (v, &ix.constituents))
            .or_else(|| ix.profile_sets.get(&m).map(|v| (v, &ix.profiles)))
        {
            out.extend(ix.set_names.get(&m).and_then(|n| n.as_deref()));
            for c in items.0.iter().filter_map(|i| items.1.get(i)) {
                out.extend(c.name_override.as_deref());
                out.extend(c.category_override.as_deref());
                mat(c.material_ref, &mut out);
            }
        } else if let Some(l) = ix.layers.get(&m) {
            out.extend(l.name_override.as_deref());
            out.extend(l.category_override.as_deref());
            mat(l.material_ref, &mut out);
        } else if let Some(c) = ix.constituents.get(&m).or_else(|| ix.profiles.get(&m)) {
            out.extend(c.name_override.as_deref());
            out.extend(c.category_override.as_deref());
            mat(c.material_ref, &mut out);
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}
