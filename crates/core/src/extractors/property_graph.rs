//! Typed property records: the shared first pass behind `m.psets`,
//! `m.quantities` and the IDS property facet.
//!
//! # PropertyGraph is the typed truth; PsetTable is a flattened view
//!
//! Three consumers read the same IFC property structure:
//!
//! - [`super::psets`] emits `PsetTable`, the public long-format table.
//!   It is **lossy by design**: every value is flattened to one string
//!   (list members joined with `", "`, bounded values as `"lower..upper"`,
//!   table values as `"d=>v, …"`), units are dropped, and only
//!   `IfcPropertySet`s reached from a product appear.
//! - [`super::quantities`] emits `QuantityTable` (`IfcElementQuantity`).
//! - The IDS property facet (GH #192) needs what the table throws away:
//!   list boundaries, each value's IfcValue wrapper (`dataType` must equal
//!   it), each property's own `Unit`, and the properties of
//!   `IfcMaterial`s, which the IDS property facet also applies to.
//!
//! So the discovery pass lives here, once. [`PropertyGraph::build`] walks
//! the [`EntityTable`] and records, without formatting anything:
//!
//! - every property definition ([`PropDef`]) by STEP id, with its values
//!   as [`TypedValue`]s that borrow the source bytes (no owned copy of the
//!   buffer, and no decode cost unless a consumer asks via
//!   [`TypedValue::raw`]);
//! - every property-set-like container ([`SetDef`]): `IfcPropertySet`,
//!   `IfcElementQuantity`, IFC4 `IfcMaterialProperties` and IFC2X3
//!   `IfcExtendedMaterialProperties` (GH #193 D4);
//! - the edges: `IfcRelDefinesByProperties` (object → set, file order),
//!   `IfcRelDefinesByType` (object → type), `HasPropertySets`
//!   (type → sets) and material → material-property sets.
//!
//! `psets::build` / `quantities::build` read the graph and format their
//! rows exactly as they always have (their string rules, row order,
//! `source` column and `unhandled:IFCXXX` marker rows are unchanged — the
//! tables are bitwise identical to the pre-graph extractors). The IDS
//! side reads [`PropertyGraph::records_for`], which applies the same
//! instance-wins type inheritance but hands back typed [`PropRecord`]s.
//!
//! What is not collected yet: `IfcPreDefinedPropertySet` subtypes
//! (`IfcDoorLiningProperties`, …) and IFC2X3's attribute-style material
//! property classes (`IfcGeneralMaterialProperties`, …). Their "properties"
//! are entity attributes, read through `ids::attrs` when a spec needs them.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use crate::entity_table::EntityTable;
use crate::lexer::{parse_field, split_top_level_args, Field};

/// Nesting cap for `IfcComplexProperty` / `IfcPhysicalComplexQuantity`.
/// The schema allows arbitrary recursion; real exports rarely go past 2-3
/// levels. A bounded walk protects against cyclic files. Both public
/// tables have always used 8.
pub const COMPLEX_MAX_DEPTH: usize = 8;

/// What kind of container a [`SetDef`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PsetKind {
    /// `IfcPropertySet`.
    PropertySet,
    /// `IfcElementQuantity`.
    ElementQuantity,
    /// IFC4 `IfcMaterialProperties` / IFC2X3 `IfcExtendedMaterialProperties`.
    MaterialProperties,
}

impl PsetKind {
    /// Which property family the set's members belong to. A member of the
    /// other family (an `IfcQuantityLength` listed in an `IfcPropertySet`)
    /// is not part of the set, the same as the public tables have always
    /// treated it.
    pub fn family(self) -> Family {
        match self {
            PsetKind::PropertySet | PsetKind::MaterialProperties => Family::Property,
            PsetKind::ElementQuantity => Family::Quantity,
        }
    }
}

/// `IfcProperty` vs `IfcPhysicalQuantity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Family {
    Property,
    Quantity,
}

/// The IFC class of a property definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PropClass {
    SingleValue,
    EnumeratedValue,
    ListValue,
    BoundedValue,
    TableValue,
    ReferenceValue,
    /// `IfcComplexProperty`: a named group; members in [`PropDef::children`].
    Complex,
    /// An `IfcProperty*Value` class with no parser here (e.g. a future
    /// schema's). The PsetTable shows it as an `unhandled:IFCXXX` row.
    UnhandledProperty,
    QuantityLength,
    QuantityArea,
    QuantityVolume,
    QuantityCount,
    QuantityWeight,
    QuantityTime,
    /// `IfcPhysicalComplexQuantity`; members in [`PropDef::children`].
    ComplexQuantity,
    /// An `IfcQuantity*` class with no parser here (IFC4X3
    /// `IfcQuantityNumber`, …).
    UnhandledQuantity,
}

impl PropClass {
    pub fn family(self) -> Family {
        match self {
            PropClass::SingleValue
            | PropClass::EnumeratedValue
            | PropClass::ListValue
            | PropClass::BoundedValue
            | PropClass::TableValue
            | PropClass::ReferenceValue
            | PropClass::Complex
            | PropClass::UnhandledProperty => Family::Property,
            _ => Family::Quantity,
        }
    }

    pub fn is_complex(self) -> bool {
        matches!(self, PropClass::Complex | PropClass::ComplexQuantity)
    }

    /// The measure type a quantity's value implicitly carries
    /// (`IfcQuantityLength.LengthValue : IfcLengthMeasure`).
    pub fn quantity_measure(self) -> Option<&'static str> {
        Some(match self {
            PropClass::QuantityLength => "IFCLENGTHMEASURE",
            PropClass::QuantityArea => "IFCAREAMEASURE",
            PropClass::QuantityVolume => "IFCVOLUMEMEASURE",
            PropClass::QuantityCount => "IFCCOUNTMEASURE",
            PropClass::QuantityWeight => "IFCMASSMEASURE",
            PropClass::QuantityTime => "IFCTIMEMEASURE",
            _ => return None,
        })
    }
}

/// Where a record came from, relative to the object it is reported on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    /// Declared on the object itself: `IfcRelDefinesByProperties`, a
    /// type object's own `HasPropertySets`, or a material's properties.
    Instance,
    /// Inherited from the object's type (`IfcRelDefinesByType` →
    /// `HasPropertySets`), not shadowed by an instance property.
    Type,
}

/// One IfcValue as it sits in the file. Borrowed, not decoded: `src` is
/// the whole STEP field (`IFCLENGTHMEASURE(2.5)`, `'text'`, `$`), so the
/// public tables can format it with their unchanged rules and IDS can
/// decode it on demand with [`TypedValue::raw`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TypedValue<'a> {
    /// The IfcValue wrapper as written (`IFCLENGTHMEASURE`; compare
    /// case-insensitively). For quantities, whose values are bare numbers,
    /// the measure the attribute is declared as (`IFCLENGTHMEASURE` for
    /// `IfcQuantityLength`). `None` for an unwrapped or null value.
    pub ifc_type: Option<&'a str>,
    /// The STEP field, untrimmed. Empty when the field is absent.
    pub src: &'a [u8],
}

/// A decoded IfcValue payload.
#[derive(Debug, Clone, PartialEq)]
pub enum RawValue {
    /// `$`, `*`, or an absent field.
    Null,
    Str(String),
    /// A number literal with a decimal point or exponent.
    Real(f64),
    /// A number literal without one that fits an `i64`.
    Int(i64),
    /// `.T.` / `.F.` inside `IFCBOOLEAN`.
    Bool(bool),
    /// `.T.` / `.F.` / `.U.` inside `IFCLOGICAL`; `None` is UNKNOWN.
    Logical(Option<bool>),
    /// Any other enumeration literal, dots stripped.
    Enum(String),
    /// `#id` (an `IfcPropertyReferenceValue` target).
    Ref(u64),
    /// Anything else, as written (nested lists, malformed literals).
    Other(String),
}

impl<'a> TypedValue<'a> {
    fn wrapped(src: &'a [u8]) -> TypedValue<'a> {
        let t = trim(src);
        let ifc_type =
            split_type_wrapper(t).and_then(|(ty, _)| std::str::from_utf8(ty).ok().map(str::trim));
        TypedValue { ifc_type, src }
    }

    /// The payload bytes: inside the wrapper when there is one.
    pub fn inner(&self) -> &'a [u8] {
        let t = trim(self.src);
        match split_type_wrapper(t) {
            Some((_, inner)) => trim(inner),
            None => t,
        }
    }

    /// Decode the payload.
    pub fn raw(&self) -> RawValue {
        let inner = self.inner();
        let wrapper = self.ifc_type.unwrap_or("");
        let is_bool = wrapper.eq_ignore_ascii_case("IFCBOOLEAN");
        let is_logical = wrapper.eq_ignore_ascii_case("IFCLOGICAL");
        match parse_field(inner) {
            Field::Null | Field::Star => RawValue::Null,
            Field::String(s) => RawValue::Str(s),
            Field::Ref(id) => RawValue::Ref(id),
            Field::Number(n) => {
                let lit = std::str::from_utf8(trim(inner)).unwrap_or("");
                let integral = !lit.bytes().any(|b| matches!(b, b'.' | b'e' | b'E'));
                match (integral, lit.parse::<i64>()) {
                    (true, Ok(i)) => RawValue::Int(i),
                    _ => RawValue::Real(n),
                }
            }
            Field::Enum(e) => {
                let s = String::from_utf8_lossy(e).into_owned();
                match (s.as_str(), is_bool, is_logical) {
                    ("T", true, _) => RawValue::Bool(true),
                    ("F", true, _) => RawValue::Bool(false),
                    ("T", _, true) => RawValue::Logical(Some(true)),
                    ("F", _, true) => RawValue::Logical(Some(false)),
                    ("U", _, true) => RawValue::Logical(None),
                    _ => RawValue::Enum(s),
                }
            }
            Field::List(_) | Field::Other(_) => {
                RawValue::Other(String::from_utf8_lossy(inner).into_owned())
            }
        }
    }
}

/// One property or quantity definition, by STEP id. Shared by every set
/// (and so every object) that lists it.
///
/// `values` layout by class:
///
/// | class                         | `values`                                   |
/// |-------------------------------|--------------------------------------------|
/// | SingleValue                   | `[NominalValue]`                           |
/// | EnumeratedValue / ListValue   | the list members, in order, nulls kept     |
/// | BoundedValue                  | `[LowerBound, UpperBound, SetPoint]`       |
/// | TableValue                    | DefinedValues (DefiningValues in `defining_values`) |
/// | ReferenceValue                | `[PropertyReference]`                      |
/// | Quantity*                     | `[<kind>Value]`, `ifc_type` = the measure  |
/// | Complex*, Unhandled*          | `[]`                                       |
///
/// An absent field is a `TypedValue` with empty `src` (decodes to Null).
/// A list field that is `$` or not a list gives no members.
#[derive(Debug, Clone)]
pub struct PropDef<'a> {
    pub step: u64,
    /// The entity type token as written (`IFCPROPERTYSINGLEVALUE`).
    pub entity: &'a [u8],
    /// `Name`; `""` when null.
    pub name: String,
    pub class: PropClass,
    pub values: Vec<TypedValue<'a>>,
    /// TableValue only: DefiningValues.
    pub defining_values: Vec<TypedValue<'a>>,
    /// The property's own `Unit` (Single / List / Bounded / quantities;
    /// Table: DefinedUnit). `None` → the project unit of the value's
    /// measure applies.
    pub unit_step: Option<u64>,
    /// TableValue only: DefiningUnit.
    pub defining_unit_step: Option<u64>,
    /// EnumeratedValue only: EnumerationReference (`IfcPropertyEnumeration`,
    /// which carries the allowed values and their Unit).
    pub enumeration_ref: Option<u64>,
    /// Complex classes: member definition ids, in order.
    pub children: Vec<u64>,
    /// Dense 0-based insertion index, `< graph.props.len()`: lets a
    /// consumer keep per-definition data in a `Vec` instead of a second
    /// hash map.
    pub ord: usize,
}

/// A property-set-like container.
#[derive(Debug, Clone)]
pub struct SetDef {
    pub step: u64,
    pub kind: PsetKind,
    /// `Name`; `""` when null.
    pub name: String,
    /// Member definition ids, in order (`HasProperties`, `Quantities`,
    /// `Properties` / `ExtendedProperties`).
    pub children: Vec<u64>,
    /// MaterialProperties only: the material it describes.
    pub material: Option<u64>,
}

/// Which parts of the pass to run. The public tables each ask only for
/// their own family so their cost stays what it was; IDS asks for [`GraphScope::ALL`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphScope {
    pub properties: bool,
    pub quantities: bool,
    pub material_properties: bool,
}

impl GraphScope {
    pub const ALL: GraphScope = GraphScope {
        properties: true,
        quantities: true,
        material_properties: true,
    };
    pub const PROPERTIES: GraphScope = GraphScope {
        properties: true,
        quantities: false,
        material_properties: false,
    };
    pub const QUANTITIES: GraphScope = GraphScope {
        properties: false,
        quantities: true,
        material_properties: false,
    };
}

/// The typed property structure of one file. See the module docs.
#[derive(Debug)]
pub struct PropertyGraph<'a> {
    pub scope: GraphScope,
    /// Every property / quantity definition by STEP id.
    pub props: HashMap<u64, PropDef<'a>>,
    /// Every set-like container by STEP id.
    pub sets: HashMap<u64, SetDef>,
    /// `IfcRelDefinesByProperties` as (object, definition) pairs: relations
    /// in file order, RelatedObjects in order, definitions in order. The
    /// definition may be of any kind (or not in `sets` at all); consumers
    /// filter by [`SetDef::kind`].
    pub defines: Vec<(u64, u64)>,
    /// `IfcRelDefinesByType`: object → type (a later relation wins).
    pub object_type: HashMap<u64, u64>,
    /// `IfcTypeObject.HasPropertySets` (non-empty only): type → definitions.
    pub type_sets: HashMap<u64, Vec<u64>>,
    /// (material, material-properties set), file order.
    pub material_sets: Vec<(u64, u64)>,
    /// `QuantityTable.unit_step_id` fallback: the `IfcSIUnit` per unit
    /// type reachable from any `IfcUnitAssignment` (GH #43 / #76 item 4).
    /// SI-only by that table's long-standing contract; general unit
    /// resolution is `crate::units::UnitTable`.
    pub(crate) quantity_default_units: HashMap<&'static str, u64>,
    by_object: OnceLock<HashMap<u64, Vec<u64>>>,
}

/// One effective property of one object, typed. Borrowed from the graph.
#[derive(Debug, Clone)]
pub struct PropRecord<'g, 'a> {
    pub object_step: u64,
    pub set: &'g SetDef,
    /// Names of the enclosing complex properties / quantities, outermost
    /// first; empty for a direct member of the set.
    pub path: Vec<&'g str>,
    pub prop: &'g PropDef<'a>,
    pub source: Source,
}

impl<'g, 'a> PropRecord<'g, 'a> {
    pub fn pset_step(&self) -> u64 {
        self.set.step
    }
    pub fn pset_name(&self) -> &'g str {
        &self.set.name
    }
    pub fn kind(&self) -> PsetKind {
        self.set.kind
    }
    /// The property's own name (without the complex path).
    pub fn prop_name(&self) -> &'g str {
        &self.prop.name
    }
    pub fn prop_class(&self) -> PropClass {
        self.prop.class
    }
    pub fn values(&self) -> &'g [TypedValue<'a>] {
        &self.prop.values
    }
    pub fn unit_step(&self) -> Option<u64> {
        self.prop.unit_step
    }
}

impl<'a> PropertyGraph<'a> {
    /// Everything: properties, quantities and material properties.
    pub fn build(table: &'a EntityTable<'_>) -> PropertyGraph<'a> {
        PropertyGraph::build_scoped(table, GraphScope::ALL)
    }

    /// One pass over the table, collecting only what `scope` asks for.
    /// The edges (`defines`, `object_type`, `type_sets`) are always
    /// collected.
    pub fn build_scoped(table: &'a EntityTable<'_>, scope: GraphScope) -> PropertyGraph<'a> {
        let want_props = scope.properties || scope.material_properties;
        let mut g = PropertyGraph {
            scope,
            props: HashMap::with_capacity(8192),
            sets: HashMap::with_capacity(2048),
            defines: Vec::with_capacity(16_384),
            object_type: HashMap::with_capacity(16_384),
            type_sets: HashMap::with_capacity(256),
            material_sets: Vec::new(),
            quantity_default_units: HashMap::new(),
            by_object: OnceLock::new(),
        };
        // Quantity unit fallback inputs (see `quantity_default_units`).
        let mut project_unit_refs: HashSet<u64> = HashSet::with_capacity(16);
        let mut si_unit_by_type: HashMap<String, Vec<u64>> = HashMap::with_capacity(16);

        for (step, t, args) in table.iter() {
            if !is_candidate(t) {
                // Geometry and everything else: only type objects matter.
                if is_type_object(t) {
                    // IfcTypeObject.HasPropertySets is attribute 6 (index 5)
                    // on every subtype, IFC2X3 and IFC4 alike.
                    let f = split_top_level_args(args);
                    let ids = ref_list_at(&f, 5);
                    if !ids.is_empty() {
                        g.type_sets.insert(step, ids);
                    }
                }
                continue;
            }
            if t.eq_ignore_ascii_case(b"IFCPROPERTYSET") {
                if !scope.properties {
                    continue;
                }
                // (GlobalId, OwnerHistory, Name, Description, HasProperties)
                let f = split_top_level_args(args);
                g.insert_set(
                    step,
                    PsetKind::PropertySet,
                    string_at(&f, 2),
                    ref_list_at(&f, 4),
                );
            } else if t.eq_ignore_ascii_case(b"IFCELEMENTQUANTITY") {
                if !scope.quantities {
                    continue;
                }
                // (GlobalId, OwnerHistory, Name, Description, MethodOfMeasurement, Quantities)
                let f = split_top_level_args(args);
                g.insert_set(
                    step,
                    PsetKind::ElementQuantity,
                    string_at(&f, 2),
                    ref_list_at(&f, 5),
                );
            } else if let Some(class) = property_class(t) {
                if want_props {
                    let f = split_top_level_args(args);
                    let ord = g.props.len();
                    g.props.insert(step, property_def(step, t, class, &f, ord));
                }
            } else if let Some(class) = quantity_class(t) {
                if scope.quantities {
                    let f = split_top_level_args(args);
                    let ord = g.props.len();
                    g.props.insert(step, quantity_def(step, t, class, &f, ord));
                }
            } else if t.eq_ignore_ascii_case(b"IFCRELDEFINESBYPROPERTIES") {
                // (GlobalId, OwnerHistory, Name, Description, RelatedObjects,
                //  RelatingPropertyDefinition). RelatingPropertyDefinition is
                // an IfcPropertySetDefinitionSelect in IFC4: a single ref, an
                // inline list, or the typed IFCPROPERTYSETDEFINITIONSET((…))
                // wrapper (GH #76 item 5). RelatedObjects may be a bare ref
                // on some IFC2X3 exporters.
                let f = split_top_level_args(args);
                let def_ids = relating_def_refs(f.get(5).copied());
                if def_ids.is_empty() {
                    continue;
                }
                let relateds = match f.get(4).copied().map(parse_field) {
                    Some(Field::List(body)) => parse_ref_list(body),
                    Some(Field::Ref(id)) => vec![id],
                    _ => continue,
                };
                for obj in relateds {
                    for d in &def_ids {
                        g.defines.push((obj, *d));
                    }
                }
            } else if t.eq_ignore_ascii_case(b"IFCRELDEFINESBYTYPE") {
                // (GlobalId, OwnerHistory, Name, Description, RelatedObjects, RelatingType)
                let f = split_top_level_args(args);
                let type_id = match f.get(5).copied().map(parse_field) {
                    Some(Field::Ref(id)) => id,
                    _ => continue,
                };
                let relateds = match f.get(4).copied().map(parse_field) {
                    Some(Field::List(body)) => parse_ref_list(body),
                    Some(Field::Ref(id)) => vec![id],
                    _ => continue,
                };
                for obj in relateds {
                    g.object_type.insert(obj, type_id);
                }
            } else if t.eq_ignore_ascii_case(b"IFCMATERIALPROPERTIES") {
                if !scope.material_properties {
                    continue;
                }
                // IFC4: (Name, Description, Properties, Material). The class
                // is abstract in IFC2X3, so it never appears there.
                let f = split_top_level_args(args);
                g.insert_set(
                    step,
                    PsetKind::MaterialProperties,
                    string_at(&f, 0),
                    ref_list_at(&f, 2),
                );
                g.link_material(step, ref_at(&f, 3));
            } else if t.eq_ignore_ascii_case(b"IFCEXTENDEDMATERIALPROPERTIES") {
                if !scope.material_properties {
                    continue;
                }
                // IFC2X3: (Material, ExtendedProperties, Description, Name).
                let f = split_top_level_args(args);
                g.insert_set(
                    step,
                    PsetKind::MaterialProperties,
                    string_at(&f, 3),
                    ref_list_at(&f, 1),
                );
                g.link_material(step, ref_at(&f, 0));
            } else if t.eq_ignore_ascii_case(b"IFCUNITASSIGNMENT") {
                if !scope.quantities {
                    continue;
                }
                // (Units). Every assignment counts (union), GH #43.
                let f = split_top_level_args(args);
                project_unit_refs.extend(ref_list_at(&f, 0));
            } else if t.eq_ignore_ascii_case(b"IFCSIUNIT") {
                if !scope.quantities {
                    continue;
                }
                // (Dimensions, UnitType, Prefix, Name). Keep every SIUnit per
                // type in file order; assignment membership picks after the
                // pass, so a dangling duplicate cannot shadow the assigned
                // one (GH #76 item 4).
                let f = split_top_level_args(args);
                if let Some(ut) = f.get(1).copied().and_then(parse_enum_uppercase) {
                    si_unit_by_type.entry(ut).or_default().push(step);
                }
            }
        }

        if !project_unit_refs.is_empty() {
            for (unit_type, steps) in &si_unit_by_type {
                let Some(canonical) = canonical_quantity_unit_type(unit_type) else {
                    continue;
                };
                if let Some(assigned) = steps.iter().find(|id| project_unit_refs.contains(id)) {
                    g.quantity_default_units.insert(canonical, *assigned);
                }
            }
        }
        g
    }

    fn insert_set(&mut self, step: u64, kind: PsetKind, name: Option<String>, children: Vec<u64>) {
        self.sets.insert(
            step,
            SetDef {
                step,
                kind,
                name: name.unwrap_or_default(),
                children,
                material: None,
            },
        );
    }

    fn link_material(&mut self, set_step: u64, material: Option<u64>) {
        if let Some(m) = material {
            if let Some(s) = self.sets.get_mut(&set_step) {
                s.material = Some(m);
            }
            self.material_sets.push((m, set_step));
        }
    }

    /// Visit every leaf of `set` depth-first in member order, passing the
    /// enclosing complex names. Members of the other family and unknown
    /// ids are skipped; complex nesting stops at [`COMPLEX_MAX_DEPTH`].
    pub fn walk_set_leaves<'g, F>(&'g self, set: &'g SetDef, visit: &mut F)
    where
        F: FnMut(&[&'g str], &'g PropDef<'a>),
    {
        let family = set.kind.family();
        let mut path: Vec<&'g str> = Vec::new();
        for pid in &set.children {
            self.walk(*pid, family, &mut path, 0, visit);
        }
    }

    fn walk<'g, F>(
        &'g self,
        pid: u64,
        family: Family,
        path: &mut Vec<&'g str>,
        depth: usize,
        visit: &mut F,
    ) where
        F: FnMut(&[&'g str], &'g PropDef<'a>),
    {
        let Some(def) = self.props.get(&pid) else {
            return;
        };
        if def.class.family() != family {
            return;
        }
        if def.class.is_complex() {
            if depth >= COMPLEX_MAX_DEPTH {
                return;
            }
            path.push(def.name.as_str());
            for c in &def.children {
                self.walk(*c, family, path, depth + 1, visit);
            }
            path.pop();
        } else {
            visit(path, def);
        }
    }

    /// Definitions declared directly on each object: relation targets,
    /// a type object's own `HasPropertySets`, a material's property sets.
    /// Only ids present in `sets` are kept; duplicates are dropped.
    fn direct_sets(&self) -> &HashMap<u64, Vec<u64>> {
        self.by_object.get_or_init(|| {
            let mut m: HashMap<u64, Vec<u64>> = HashMap::new();
            let pairs = self
                .defines
                .iter()
                .copied()
                .chain(
                    self.type_sets
                        .iter()
                        .flat_map(|(t, ids)| ids.iter().map(move |s| (*t, *s))),
                )
                .chain(self.material_sets.iter().copied());
            for (obj, set) in pairs {
                if !self.sets.contains_key(&set) {
                    continue;
                }
                let v = m.entry(obj).or_default();
                if !v.contains(&set) {
                    v.push(set);
                }
            }
            m
        })
    }

    /// The effective properties of `object_step` (a product, a type object
    /// or a material), typed: every leaf of every set declared on it
    /// ([`Source::Instance`]), then the leaves of its type's sets that no
    /// instance leaf shadows ([`Source::Type`]). Shadowing is by (property
    /// family, set name, complex path, property name), the rule the public
    /// tables use. Order: instance sets in declaration order, then type
    /// sets in `HasPropertySets` order.
    pub fn records_for<'g>(&'g self, object_step: u64) -> Vec<PropRecord<'g, 'a>> {
        let mut out: Vec<PropRecord<'g, 'a>> = Vec::new();
        let mut seen: HashSet<(Family, &'g str, Vec<&'g str>, &'g str)> = HashSet::new();
        if let Some(sets) = self.direct_sets().get(&object_step) {
            for sid in sets {
                let Some(set) = self.sets.get(sid) else {
                    continue;
                };
                self.walk_set_leaves(set, &mut |path, prop| {
                    seen.insert((
                        set.kind.family(),
                        set.name.as_str(),
                        path.to_vec(),
                        prop.name.as_str(),
                    ));
                    out.push(PropRecord {
                        object_step,
                        set,
                        path: path.to_vec(),
                        prop,
                        source: Source::Instance,
                    });
                });
            }
        }
        let type_sets = self
            .object_type
            .get(&object_step)
            .and_then(|t| self.type_sets.get(t));
        if let Some(ids) = type_sets {
            for sid in ids {
                let Some(set) = self.sets.get(sid) else {
                    continue;
                };
                self.walk_set_leaves(set, &mut |path, prop| {
                    let key = (
                        set.kind.family(),
                        set.name.as_str(),
                        path.to_vec(),
                        prop.name.as_str(),
                    );
                    if seen.contains(&key) {
                        return;
                    }
                    out.push(PropRecord {
                        object_step,
                        set,
                        path: path.to_vec(),
                        prop,
                        source: Source::Type,
                    });
                });
            }
        }
        out
    }
}

// ---------------------------------------------------------------------
// Pass-1 record parsing
// ---------------------------------------------------------------------

/// Cheap pre-filter: could `t` be one of the entities the pass reads
/// (other than a type object)? Most of a file is geometry; this keeps the
/// per-entity cost to a byte test and a short compare. Every name the
/// pass dispatches on must pass it.
fn is_candidate(t: &[u8]) -> bool {
    if t.len() < 8 || !t[..3].eq_ignore_ascii_case(b"IFC") {
        return false;
    }
    let starts = |p: &[u8]| t.len() >= p.len() && t[..p.len()].eq_ignore_ascii_case(p);
    match t[3].to_ascii_uppercase() {
        b'P' => starts(b"IFCPROPERTY") || t.eq_ignore_ascii_case(b"IFCPHYSICALCOMPLEXQUANTITY"),
        b'C' => t.eq_ignore_ascii_case(b"IFCCOMPLEXPROPERTY"),
        b'Q' => starts(b"IFCQUANTITY"),
        b'R' => starts(b"IFCRELDEFINESBY"),
        b'E' => {
            t.eq_ignore_ascii_case(b"IFCELEMENTQUANTITY")
                || t.eq_ignore_ascii_case(b"IFCEXTENDEDMATERIALPROPERTIES")
        }
        b'M' => t.eq_ignore_ascii_case(b"IFCMATERIALPROPERTIES"),
        b'U' => t.eq_ignore_ascii_case(b"IFCUNITASSIGNMENT"),
        b'S' => t.eq_ignore_ascii_case(b"IFCSIUNIT"),
        _ => false,
    }
}

fn property_class(t: &[u8]) -> Option<PropClass> {
    Some(if t.eq_ignore_ascii_case(b"IFCPROPERTYSINGLEVALUE") {
        PropClass::SingleValue
    } else if t.eq_ignore_ascii_case(b"IFCPROPERTYENUMERATEDVALUE") {
        PropClass::EnumeratedValue
    } else if t.eq_ignore_ascii_case(b"IFCPROPERTYLISTVALUE") {
        PropClass::ListValue
    } else if t.eq_ignore_ascii_case(b"IFCPROPERTYBOUNDEDVALUE") {
        PropClass::BoundedValue
    } else if t.eq_ignore_ascii_case(b"IFCPROPERTYTABLEVALUE") {
        PropClass::TableValue
    } else if t.eq_ignore_ascii_case(b"IFCCOMPLEXPROPERTY") {
        PropClass::Complex
    } else if t.eq_ignore_ascii_case(b"IFCPROPERTYREFERENCEVALUE") {
        PropClass::ReferenceValue
    } else if is_unhandled_simple_property(t) {
        PropClass::UnhandledProperty
    } else {
        return None;
    })
}

fn quantity_class(t: &[u8]) -> Option<PropClass> {
    if let Some(c) = simple_quantity_class(t) {
        return Some(c);
    }
    if t.eq_ignore_ascii_case(b"IFCPHYSICALCOMPLEXQUANTITY") {
        Some(PropClass::ComplexQuantity)
    } else if is_unhandled_quantity(t) {
        Some(PropClass::UnhandledQuantity)
    } else {
        None
    }
}

fn simple_quantity_class(t: &[u8]) -> Option<PropClass> {
    Some(if t.eq_ignore_ascii_case(b"IFCQUANTITYAREA") {
        PropClass::QuantityArea
    } else if t.eq_ignore_ascii_case(b"IFCQUANTITYLENGTH") {
        PropClass::QuantityLength
    } else if t.eq_ignore_ascii_case(b"IFCQUANTITYVOLUME") {
        PropClass::QuantityVolume
    } else if t.eq_ignore_ascii_case(b"IFCQUANTITYCOUNT") {
        PropClass::QuantityCount
    } else if t.eq_ignore_ascii_case(b"IFCQUANTITYWEIGHT") {
        PropClass::QuantityWeight
    } else if t.eq_ignore_ascii_case(b"IFCQUANTITYTIME") {
        PropClass::QuantityTime
    } else {
        return None;
    })
}

/// Any `IfcProperty*Value` class. Every IfcSimpleProperty leaf is named
/// with the `…Value` suffix and no other IFC entity is, which makes this
/// a safe "a property class we have no parser for" probe (GH #38).
fn is_unhandled_simple_property(t: &[u8]) -> bool {
    t.len() > 16
        && t[..11].eq_ignore_ascii_case(b"IFCPROPERTY")
        && t[t.len() - 5..].eq_ignore_ascii_case(b"VALUE")
}

/// An `IfcQuantity*` class other than the six known ones (the prefix is
/// unique to IfcPhysicalSimpleQuantity leaves; GH #159).
/// `IfcPhysicalComplexQuantity` has its own class.
pub(crate) fn is_unhandled_quantity(t: &[u8]) -> bool {
    const PREFIX: &[u8] = b"IFCQUANTITY";
    t.len() > PREFIX.len()
        && t[..PREFIX.len()].eq_ignore_ascii_case(PREFIX)
        && simple_quantity_class(t).is_none()
}

fn field<'a>(f: &[&'a [u8]], idx: usize) -> &'a [u8] {
    f.get(idx).copied().unwrap_or(b"")
}

fn property_def<'a>(
    step: u64,
    t: &'a [u8],
    class: PropClass,
    f: &[&'a [u8]],
    ord: usize,
) -> PropDef<'a> {
    let mut d = PropDef {
        step,
        entity: t,
        name: string_at(f, 0).unwrap_or_default(),
        class,
        values: Vec::new(),
        defining_values: Vec::new(),
        unit_step: None,
        defining_unit_step: None,
        enumeration_ref: None,
        children: Vec::new(),
        ord,
    };
    match class {
        // (Name, Description, NominalValue, Unit)
        PropClass::SingleValue => {
            d.values.push(TypedValue::wrapped(field(f, 2)));
            d.unit_step = ref_at(f, 3);
        }
        // (Name, Description, EnumerationValues, EnumerationReference)
        PropClass::EnumeratedValue => {
            d.values = list_members(f.get(2).copied())
                .into_iter()
                .map(TypedValue::wrapped)
                .collect();
            d.enumeration_ref = ref_at(f, 3);
        }
        // (Name, Description, ListValues, Unit)
        PropClass::ListValue => {
            d.values = list_members(f.get(2).copied())
                .into_iter()
                .map(TypedValue::wrapped)
                .collect();
            d.unit_step = ref_at(f, 3);
        }
        // (Name, Description, UpperBoundValue, LowerBoundValue, Unit, SetPointValue)
        PropClass::BoundedValue => {
            d.values = vec![
                TypedValue::wrapped(field(f, 3)),
                TypedValue::wrapped(field(f, 2)),
                TypedValue::wrapped(field(f, 5)),
            ];
            d.unit_step = ref_at(f, 4);
        }
        // (Name, Description, DefiningValues, DefinedValues, Expression,
        //  DefiningUnit, DefinedUnit, CurveInterpolation)
        PropClass::TableValue => {
            d.defining_values = list_members(f.get(2).copied())
                .into_iter()
                .map(TypedValue::wrapped)
                .collect();
            d.values = list_members(f.get(3).copied())
                .into_iter()
                .map(TypedValue::wrapped)
                .collect();
            d.defining_unit_step = ref_at(f, 5);
            d.unit_step = ref_at(f, 6);
        }
        // (Name, Description, UsageName, PropertyReference)
        PropClass::ReferenceValue => {
            d.values.push(TypedValue {
                ifc_type: None,
                src: field(f, 3),
            });
        }
        // (Name, Description, UsageName, HasProperties). No GlobalId.
        PropClass::Complex => {
            d.children = ref_list_at(f, 3);
        }
        _ => {}
    }
    d
}

fn quantity_def<'a>(
    step: u64,
    t: &'a [u8],
    class: PropClass,
    f: &[&'a [u8]],
    ord: usize,
) -> PropDef<'a> {
    let mut d = PropDef {
        step,
        entity: t,
        name: string_at(f, 0).unwrap_or_default(),
        class,
        values: Vec::new(),
        defining_values: Vec::new(),
        unit_step: None,
        defining_unit_step: None,
        enumeration_ref: None,
        children: Vec::new(),
        ord,
    };
    match class {
        // (Name, Description, HasQuantities, Discrimination, Quality, Usage)
        PropClass::ComplexQuantity => d.children = ref_list_at(f, 2),
        PropClass::UnhandledQuantity => {}
        // (Name, Description, Unit, <kind>Value[, Formula])
        _ => {
            d.values.push(TypedValue {
                ifc_type: class.quantity_measure(),
                src: field(f, 3),
            });
            d.unit_step = ref_at(f, 2);
        }
    }
    d
}

/// The members of a `LIST OF IfcValue` field. No members for `$`, `*`,
/// an absent field, or a field that is not `( … )`. Null members are kept
/// (a table's two lists pair up by position).
pub(crate) fn list_members(raw: Option<&[u8]>) -> Vec<&[u8]> {
    let Some(raw) = raw else {
        return Vec::new();
    };
    let t = trim(raw);
    if t.is_empty() || t == b"$" || t == b"*" {
        return Vec::new();
    }
    match (t.first(), t.last()) {
        (Some(&b'('), Some(&b')')) if t.len() >= 2 => split_top_level_args(&t[1..t.len() - 1]),
        _ => Vec::new(),
    }
}

/// `TYPENAME(inner)` → (TYPENAME, inner). `None` unless the field starts
/// with `IFC` (any case), has a `(` and ends with `)`.
pub(crate) fn split_type_wrapper(field: &[u8]) -> Option<(&[u8], &[u8])> {
    if field.len() < 5 || !field[..3].eq_ignore_ascii_case(b"IFC") {
        return None;
    }
    let open = field.iter().position(|&b| b == b'(')?;
    if *field.last()? != b')' {
        return None;
    }
    Some((&field[..open], &field[open + 1..field.len() - 1]))
}

pub(crate) fn trim(s: &[u8]) -> &[u8] {
    let mut start = 0;
    while start < s.len() && (s[start] as char).is_whitespace() {
        start += 1;
    }
    let mut end = s.len();
    while end > start && (s[end - 1] as char).is_whitespace() {
        end -= 1;
    }
    &s[start..end]
}

/// IfcTypeObject or any subclass, by name: the `IFC…TYPE` suffix rule,
/// the IFC2X3 `IfcDoorStyle` / `IfcWindowStyle`, and the bare
/// `IfcTypeProduct` / `IfcTypeObject` Revit emits (#69). The same rule as
/// `indexer::index`.
fn is_type_object(t: &[u8]) -> bool {
    let suffix_ok = t.len() > 7
        && t[..3].eq_ignore_ascii_case(b"IFC")
        && t[t.len() - 4..].eq_ignore_ascii_case(b"TYPE");
    let ifc2x3_style =
        t.eq_ignore_ascii_case(b"IFCDOORSTYLE") || t.eq_ignore_ascii_case(b"IFCWINDOWSTYLE");
    let bare_base =
        t.eq_ignore_ascii_case(b"IFCTYPEPRODUCT") || t.eq_ignore_ascii_case(b"IFCTYPEOBJECT");
    suffix_ok || ifc2x3_style || bare_base
}

/// The quantity fallback's accepted unit types, pinned to `&'static str`.
fn canonical_quantity_unit_type(uppercase: &str) -> Option<&'static str> {
    match uppercase {
        "LENGTHUNIT" => Some("LENGTHUNIT"),
        "AREAUNIT" => Some("AREAUNIT"),
        "VOLUMEUNIT" => Some("VOLUMEUNIT"),
        "MASSUNIT" => Some("MASSUNIT"),
        "TIMEUNIT" => Some("TIMEUNIT"),
        _ => None,
    }
}

/// `.LENGTHUNIT.` → `"LENGTHUNIT"` (trimmed, uppercased); `None` for any
/// other shape.
fn parse_enum_uppercase(raw: &[u8]) -> Option<String> {
    let t = trim(raw);
    if t.len() < 2 || t.first() != Some(&b'.') || t.last() != Some(&b'.') {
        return None;
    }
    let s = std::str::from_utf8(&t[1..t.len() - 1]).ok()?;
    Some(s.to_ascii_uppercase())
}

fn string_at(fields: &[&[u8]], idx: usize) -> Option<String> {
    match parse_field(fields.get(idx)?) {
        Field::String(s) => Some(s),
        _ => None,
    }
}

fn ref_at(fields: &[&[u8]], idx: usize) -> Option<u64> {
    match parse_field(fields.get(idx)?) {
        Field::Ref(id) => Some(id),
        _ => None,
    }
}

fn ref_list_at(fields: &[&[u8]], idx: usize) -> Vec<u64> {
    match fields.get(idx).copied().map(parse_field) {
        Some(Field::List(body)) => parse_ref_list(body),
        _ => Vec::new(),
    }
}

fn parse_ref_list(body: &[u8]) -> Vec<u64> {
    split_top_level_args(body)
        .into_iter()
        .filter_map(|f| match parse_field(f) {
            Field::Ref(id) => Some(id),
            _ => None,
        })
        .collect()
}

/// `IfcRelDefinesByProperties.RelatingPropertyDefinition` → definition
/// ids. Accepts `#5`, `(#1,#2)` and `IFCPROPERTYSETDEFINITIONSET((#1,#2))`
/// (GH #76 item 5).
fn relating_def_refs(raw: Option<&[u8]>) -> Vec<u64> {
    let Some(raw) = raw else {
        return Vec::new();
    };
    match parse_field(raw) {
        Field::Ref(id) => vec![id],
        Field::List(body) => parse_ref_list(body),
        Field::Other(bytes) => relating_def_typed_wrapper_refs(bytes),
        _ => Vec::new(),
    }
}

/// Peel `IFCPROPERTYSETDEFINITIONSET((#1,#2))`: the entity-arg parens, then
/// the inner LIST value (or a single bare ref).
fn relating_def_typed_wrapper_refs(bytes: &[u8]) -> Vec<u64> {
    let t = crate::lexer::trim_ws(bytes);
    let prefix = b"IFCPROPERTYSETDEFINITIONSET";
    if t.len() <= prefix.len() || !t[..prefix.len()].eq_ignore_ascii_case(prefix) {
        return Vec::new();
    }
    let outer = crate::lexer::trim_ws(&t[prefix.len()..]);
    if outer.first() != Some(&b'(') || outer.last() != Some(&b')') {
        return Vec::new();
    }
    let inner = crate::lexer::trim_ws(&outer[1..outer.len() - 1]);
    match parse_field(inner) {
        Field::List(body) => parse_ref_list(body),
        Field::Ref(id) => vec![id],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(rel: &str) -> Vec<u8> {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
        std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
    }

    const PROPS_UNITS: &str = "tests/fixtures/ids/props_units.ifc";

    fn guid_map(table: &EntityTable) -> HashMap<u64, String> {
        let mut m = HashMap::new();
        for (sid, _t, args) in table.iter() {
            let f = split_top_level_args(args);
            if let Some(Field::String(s)) = f.first().map(|x| parse_field(x)) {
                if s.len() == 22 {
                    m.insert(sid, s);
                }
            }
        }
        m
    }

    fn find<'g, 'a>(recs: &'g [PropRecord<'g, 'a>], name: &str) -> &'g PropRecord<'g, 'a> {
        recs.iter()
            .find(|r| r.prop_name() == name)
            .unwrap_or_else(|| panic!("no record {name}"))
    }

    #[test]
    fn minimal_fixture_typed_records() {
        let buf = fixture("../../tests/fixtures/minimal.ifc");
        let table = EntityTable::build(&buf);
        let g = PropertyGraph::build(&table);
        let recs = g.records_for(30);
        let names: Vec<(&str, &str)> = recs
            .iter()
            .map(|r| (r.pset_name(), r.prop_name()))
            .collect();
        assert_eq!(
            names,
            vec![
                ("Pset_WallCommon", "IsExternal"),
                ("Pset_WallCommon", "LoadBearing"),
                ("Qto_WallBaseQuantities", "Length"),
                ("Qto_WallBaseQuantities", "NetSideArea"),
            ]
        );
        let ext = find(&recs, "IsExternal");
        assert_eq!(ext.kind(), PsetKind::PropertySet);
        assert_eq!(ext.prop_class(), PropClass::SingleValue);
        assert_eq!(ext.values()[0].ifc_type, Some("IFCBOOLEAN"));
        assert_eq!(ext.values()[0].raw(), RawValue::Bool(true));
        assert_eq!(
            find(&recs, "LoadBearing").values()[0].raw(),
            RawValue::Bool(false)
        );
        let len = find(&recs, "Length");
        assert_eq!(len.kind(), PsetKind::ElementQuantity);
        assert_eq!(len.prop_class(), PropClass::QuantityLength);
        assert_eq!(len.values()[0].ifc_type, Some("IFCLENGTHMEASURE"));
        assert_eq!(len.values()[0].raw(), RawValue::Real(3.0));
        assert!(recs.iter().all(|r| r.source == Source::Instance));
    }

    #[test]
    fn quantities_fixture_every_kind_and_unit_default() {
        let buf = fixture("../../tests/fixtures/quantities.ifc");
        let table = EntityTable::build(&buf);
        let g = PropertyGraph::build(&table);
        let classes: Vec<PropClass> = g.records_for(30).iter().map(|r| r.prop_class()).collect();
        assert_eq!(
            classes,
            vec![
                PropClass::QuantityLength,
                PropClass::QuantityArea,
                PropClass::QuantityVolume,
                PropClass::QuantityCount,
                PropClass::QuantityWeight,
                PropClass::QuantityTime,
            ]
        );
        let count = g.records_for(30)[3].values()[0];
        assert_eq!(count.ifc_type, Some("IFCCOUNTMEASURE"));
        assert_eq!(count.raw(), RawValue::Real(12.0));
        assert!(g.records_for(30).iter().all(|r| r.unit_step().is_none()));
        // Only #3 (METRE) is an assigned SIUnit of a quantity unit type.
        assert_eq!(g.quantity_default_units.get("LENGTHUNIT"), Some(&3));
        assert_eq!(g.quantity_default_units.len(), 1);
        assert_eq!(g.records_for(32).len(), 2);
    }

    #[test]
    fn props_units_fixture_keeps_what_the_table_flattens() {
        let buf = fixture(PROPS_UNITS);
        let table = EntityTable::build(&buf);
        let g = PropertyGraph::build(&table);
        let recs = g.records_for(30);

        // List: boundaries kept (the table joins to "A, B, C").
        let layers = find(&recs, "Layers");
        assert_eq!(layers.prop_class(), PropClass::ListValue);
        let raws: Vec<RawValue> = layers.values().iter().map(|v| v.raw()).collect();
        assert_eq!(
            raws,
            vec![RawValue::Str("A".into()), RawValue::Str("B, C".into())]
        );
        assert!(layers
            .values()
            .iter()
            .all(|v| v.ifc_type == Some("IFCLABEL")));

        // Bounded: [lower, upper, setpoint]; IFC2X3 has no setpoint.
        let temp = find(&recs, "Temp");
        assert_eq!(temp.prop_class(), PropClass::BoundedValue);
        assert_eq!(temp.values()[0].raw(), RawValue::Real(-5.0));
        assert_eq!(temp.values()[1].raw(), RawValue::Real(30.0));
        assert_eq!(temp.values()[2].raw(), RawValue::Null);
        assert_eq!(
            temp.values()[1].ifc_type,
            Some("IFCTHERMODYNAMICTEMPERATUREMEASURE")
        );

        // Enumerated: members + the enumeration it draws from.
        let status = find(&recs, "Status");
        assert_eq!(status.prop_class(), PropClass::EnumeratedValue);
        assert_eq!(status.values().len(), 1);
        assert_eq!(status.values()[0].raw(), RawValue::Str("NEW".into()));
        assert_eq!(status.prop.enumeration_ref, Some(58));

        // Table: both axes, DefinedUnit.
        let curve = find(&recs, "Curve");
        assert_eq!(curve.prop_class(), PropClass::TableValue);
        assert_eq!(curve.prop.defining_values.len(), 2);
        assert_eq!(curve.prop.defining_values[1].raw(), RawValue::Real(2.0));
        assert_eq!(curve.values()[0].ifc_type, Some("IFCPRESSUREMEASURE"));
        assert_eq!(curve.unit_step(), Some(23));
        assert_eq!(curve.prop.defining_unit_step, None);

        // A property with its own Unit.
        let pressure = find(&recs, "Pressure");
        assert_eq!(pressure.unit_step(), Some(23));
        assert_eq!(pressure.values()[0].raw(), RawValue::Real(2.5));

        // Complex: path carries the wrapper.
        let width = find(&recs, "Width");
        assert_eq!(width.path, vec!["Profile"]);

        // Quantity with its own unit; one without.
        let area = find(&recs, "NetSideArea");
        assert_eq!(area.unit_step(), Some(24));
        assert_eq!(area.values()[0].ifc_type, Some("IFCAREAMEASURE"));
        assert_eq!(area.values()[0].raw(), RawValue::Real(12_500_000.0));
        assert_eq!(find(&recs, "Length").unit_step(), None);

        // Type inheritance: the instance's FireRating shadows the type's;
        // IsExternal comes from the type.
        let fire: Vec<&PropRecord> = recs
            .iter()
            .filter(|r| r.prop_name() == "FireRating")
            .collect();
        assert_eq!(fire.len(), 1);
        assert_eq!(fire[0].source, Source::Instance);
        assert_eq!(fire[0].values()[0].raw(), RawValue::Str("EI60".into()));
        let ext = find(&recs, "IsExternal");
        assert_eq!(ext.source, Source::Type);
        assert_eq!(ext.pset_step(), 40);
        assert_eq!(recs.last().map(|r| r.prop_name()), Some("IsExternal"));
        assert_eq!(recs.len(), 10);

        // The type object itself: its own HasPropertySets, as Instance.
        let t = g.records_for(31);
        assert_eq!(t.len(), 2);
        assert!(t.iter().all(|r| r.source == Source::Instance));
        assert_eq!(
            find(&t, "FireRating").values()[0].raw(),
            RawValue::Str("EI30".into())
        );

        // IFC2X3 IfcExtendedMaterialProperties, keyed on the material.
        assert_eq!(g.material_sets, vec![(80, 81)]);
        let m = g.records_for(80);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].kind(), PsetKind::MaterialProperties);
        assert_eq!(m[0].pset_name(), "Pset_MaterialConcrete");
        assert_eq!(m[0].set.material, Some(80));
        assert_eq!(m[0].prop_name(), "CompressiveStrength");
        assert_eq!(m[0].unit_step(), Some(23));
        assert_eq!(m[1].values()[0].ifc_type, Some("IFCPOSITIVELENGTHMEASURE"));
    }

    /// The public tables read from the same graph, flattened: this is the
    /// lossiness the graph exists to avoid.
    #[test]
    fn props_units_fixture_public_tables_are_the_flat_view() {
        let buf = fixture(PROPS_UNITS);
        let table = EntityTable::build(&buf);
        let map = guid_map(&table);
        let g = PropertyGraph::build(&table);
        let p = crate::extractors::psets::build_from_graph(&g, &map);
        let row = |name: &str| {
            let i = p
                .prop_name
                .iter()
                .position(|n| n == name)
                .unwrap_or_else(|| panic!("no pset row {name}"));
            (
                p.value[i].clone(),
                p.value_type[i].clone(),
                p.source[i].clone(),
            )
        };
        assert_eq!(row("Layers").0.as_deref(), Some("A, B, C"));
        assert_eq!(row("Temp").0.as_deref(), Some("-5..30"));
        assert_eq!(row("Curve").0.as_deref(), Some("1=>10, 2=>20"));
        assert_eq!(row("Profile.Width").0.as_deref(), Some("200"));
        // Long-standing PsetTable behaviour, kept for byte identity:
        // defined types missing from the indexer's entity-name table are
        // title-cased as one word (ifcopenshell says IfcPressureMeasure).
        // The graph keeps the wrapper as written.
        assert_eq!(row("Pressure").1.as_deref(), Some("IfcPressuremeasure"));
        assert_eq!(row("IsExternal").2, "type");
        assert_eq!(p.prop_name.iter().filter(|n| *n == "FireRating").count(), 1);
        // Material properties never reach m.psets (no guid, not a product).
        assert!(!p.prop_name.iter().any(|n| n == "CompressiveStrength"));
        assert_eq!(p.len(), 8);
        // Same rows through the scoped build.
        let scoped = crate::extractors::psets::build(&table, &map);
        assert_eq!(scoped.prop_name, p.prop_name);
        assert_eq!(scoped.value, p.value);

        let q = crate::extractors::quantities::build_from_graph(&g, &map);
        assert_eq!(q.quantity_name, vec!["Length", "NetSideArea"]);
        // Length falls back to the assigned LENGTHUNIT; NetSideArea keeps its own.
        assert_eq!(q.unit_step_id, vec![Some(3), Some(24)]);
        assert_eq!(q.value, vec![Some("5000".into()), Some("12500000".into())]);
    }

    /// IFC4 `IfcMaterialProperties(Name, Description, Properties, Material)`.
    #[test]
    fn ifc4_material_properties_keyed_on_material() {
        let src = "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION((''),'2;1');\n\
FILE_NAME('t.ifc','2026-09-25T00:00:00',(''),(''),'ifcfast','ifcfast','');\n\
FILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n\
#1=IFCMATERIAL('Steel',$,'Steel');\n\
#2=IFCPROPERTYSINGLEVALUE('YieldStress',$,IFCPRESSUREMEASURE(355000000.),$);\n\
#3=IFCMATERIALPROPERTIES('Pset_MaterialSteel',$,(#2),#1);\n\
ENDSEC;\nEND-ISO-10303-21;\n";
        let table = EntityTable::build(src.as_bytes());
        let g = PropertyGraph::build(&table);
        let recs = g.records_for(1);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].pset_name(), "Pset_MaterialSteel");
        assert_eq!(recs[0].kind(), PsetKind::MaterialProperties);
        assert_eq!(recs[0].values()[0].raw(), RawValue::Real(355_000_000.0));
        // Scoped builds skip it.
        let scoped = PropertyGraph::build_scoped(&table, GraphScope::PROPERTIES);
        assert!(scoped.material_sets.is_empty());
    }

    #[test]
    fn raw_value_decoding() {
        let v = |src: &'static str| TypedValue::wrapped(src.as_bytes());
        assert_eq!(v("IFCINTEGER(42)").raw(), RawValue::Int(42));
        assert_eq!(v("IFCREAL(42.)").raw(), RawValue::Real(42.0));
        assert_eq!(v("IFCREAL(1.5E-3)").raw(), RawValue::Real(1.5e-3));
        assert_eq!(v("IFCLOGICAL(.U.)").raw(), RawValue::Logical(None));
        assert_eq!(v("IFCLOGICAL(.T.)").raw(), RawValue::Logical(Some(true)));
        assert_eq!(v("IFCBOOLEAN(.U.)").raw(), RawValue::Enum("U".into()));
        assert_eq!(v("IFCIDENTIFIER('x')").ifc_type, Some("IFCIDENTIFIER"));
        assert_eq!(v("$").raw(), RawValue::Null);
        assert_eq!(v("$").ifc_type, None);
        assert_eq!(v("").raw(), RawValue::Null);
        assert_eq!(v("'bare'").raw(), RawValue::Str("bare".into()));
        assert_eq!(v("'bare'").ifc_type, None);
    }

    #[test]
    fn complex_depth_is_capped_on_cycles() {
        let src = "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION((''),'2;1');\n\
FILE_NAME('t.ifc','2026-09-25T00:00:00',(''),(''),'ifcfast','ifcfast','');\n\
FILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n\
#10=IFCWALL('1Wall00000000000000001',$,'W',$,$,$,$,'t',.STANDARD.);\n\
#20=IFCCOMPLEXPROPERTY('Loop',$,$,(#20,#21));\n\
#21=IFCPROPERTYSINGLEVALUE('Leaf',$,IFCLABEL('x'),$);\n\
#22=IFCPROPERTYSET('2Pset00000000000000001',$,'P',$,(#20));\n\
#23=IFCRELDEFINESBYPROPERTIES('3Rel000000000000000001',$,$,$,(#10),#22);\n\
ENDSEC;\nEND-ISO-10303-21;\n";
        let table = EntityTable::build(src.as_bytes());
        let g = PropertyGraph::build(&table);
        let recs = g.records_for(10);
        // One Leaf per nesting level until the cap, deepest first.
        assert_eq!(recs.len(), COMPLEX_MAX_DEPTH);
        assert_eq!(recs[0].path.len(), COMPLEX_MAX_DEPTH);
        assert_eq!(recs[COMPLEX_MAX_DEPTH - 1].path, vec!["Loop"]);
    }
}
