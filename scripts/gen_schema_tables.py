"""One-shot codegen: per-schema IFC tables for the native IDS engine.

Writes ``crates/core/src/ids/schema_tables.rs`` — static, sorted Rust
tables the IDS engine (docs/plans/2026-09-24_ids-validation-design.md
§2.4/§2.5) reads instead of guessing from the STEP token stream:

* ``ENTITIES``          every entity name, UPPERCASE STEP-token form, sorted
* ``IS_TYPE_OBJECT``    sorted subset: IfcTypeObject and its subtypes
* ``IS_ROOT``           sorted subset: IfcRoot and its subtypes (GlobalId @ 0)
* ``SUPERTYPE``         (entity, direct supertype), sorted by entity
* ``ATTRS``             (entity, flattened explicit attributes in STEP
                        positional order, inherited first)
* ``PREDEF_POS`` / ``OBJTYPE_POS`` / ``ELEMTYPE_POS``
                        STEP argument index of PredefinedType / ObjectType /
                        ElementType for entities that have them
* ``PREDEF_ENUM``       allowed PredefinedType literals per entity
* ``MEASURE_UNIT_TYPE`` (dataType, Option<unit type>) for every Ifc*Measure
                        defined type plus every defined type reachable
                        from the IfcValue select (the IDS dataType
                        vocabulary); ``None`` = no unit applies
* ``PREDEF_PSET_ATTR_TYPE`` (entity, [(attribute, declared type)]) for every
                        IfcPreDefinedPropertySet subtype: its attributes
                        from STEP index 4 on whose type is a named type
                        (the IDS property facet reads these attributes as
                        properties; the declared type is their dataType)

Same sanctioned pattern as ``scripts/gen_schema_supertypes.py``:
ifcopenshell is used at GENERATION time only, the output is committed,
and ``tests/test_schema_tables_drift.py`` regenerates and byte-compares.

Run (ifcopenshell must be exactly the pinned version):

    .venv/bin/python scripts/gen_schema_tables.py
    .venv/bin/python scripts/gen_schema_tables.py --out /some/other/path.rs

Anything the classifier does not recognise raises — nothing silently
defaults to ``Other``. Every ``Other`` classification is logged to
stderr with its reason.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path
from typing import NoReturn

import ifcopenshell

PINNED_IFCOPENSHELL = "0.8.5"

# Same three names as scripts/gen_schema_supertypes.py. ifcopenshell
# 0.8.5 resolves "IFC4X3" to IFC4X3_ADD2 (asserted in `load_schema`).
SCHEMAS: tuple[tuple[str, str, str], ...] = (
    # (ifcopenshell name, expected schema.name(), Rust module / Schema variant)
    ("IFC2X3", "IFC2X3", "Ifc2x3"),
    ("IFC4", "IFC4", "Ifc4"),
    ("IFC4X3", "IFC4X3_ADD2", "Ifc4x3"),
)

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_OUT = REPO_ROOT / "crates" / "core" / "src" / "ids" / "schema_tables.rs"

GEN_COMMAND = ".venv/bin/python scripts/gen_schema_tables.py"

# ---------------------------------------------------------------------------
# Measure → unit type
# ---------------------------------------------------------------------------
#
# Auto rule (== ifcopenshell.util.unit.get_measure_unit_type, which IfcTester
# uses): strip "Ifc", "Measure", "Non", "Positive", "Negative", uppercase,
# append "UNIT". The row is accepted automatically ONLY if the result is a
# literal of the schema's IfcUnitEnum ∪ IfcDerivedUnitEnum. Every other
# Ifc*Measure must be listed here or generation fails.
#
# value: None → the measure carries no unit type; str → the unit literal
# (asserted to exist in the schema's unit enums).
HAND_MAPPED_MEASURES: dict[str, tuple[str | None, str]] = {
    "IfcThermalConductivityMeasure": (
        "THERMALCONDUCTANCEUNIT",
        "IfcDerivedUnitEnum spells it THERMALCONDUCTANCEUNIT (W/(m·K)); "
        "the auto rule yields the non-existent THERMALCONDUCTIVITYUNIT",
    ),
    "IfcSectionalAreaIntegralMeasure": (
        "SECTIONAREAINTEGRALUNIT",
        "IfcDerivedUnitEnum spells it SECTIONAREAINTEGRALUNIT (m^5); "
        "the auto rule yields SECTIONALAREAINTEGRALUNIT",
    ),
    "IfcCountMeasure": (None, "dimensionless count"),
    "IfcRatioMeasure": (None, "dimensionless ratio"),
    "IfcNormalisedRatioMeasure": (None, "dimensionless ratio in [0,1]"),
    "IfcPositiveRatioMeasure": (None, "dimensionless ratio > 0"),
    "IfcNumericMeasure": (
        None,
        "untyped number; ifcopenshell maps it to USERDEFINED "
        "(bSI IFC4.3.x-development#71) — no IfcUnitEnum unit type",
    ),
    "IfcDescriptiveMeasure": (None, "STRING-typed descriptive measure"),
    "IfcContextDependentMeasure": (
        None,
        "unit is supplied per-use by IfcContextDependentUnit; no fixed unit type",
    ),
    "IfcMonetaryMeasure": (
        None,
        "currency is IfcMonetaryUnit, which is outside IfcUnitEnum/IfcDerivedUnitEnum",
    ),
}

# ---------------------------------------------------------------------------
# Attribute kinds
# ---------------------------------------------------------------------------

KIND_CODE = {
    # AttrKind variant → the one-letter alias used in the generated rows
    "String": "S",
    "Enum": "E",
    "Int": "I",
    "Real": "R",
    "Bool": "B",
    "Logical": "L",
    "Ref": "F",
    "List": "V",
    "Select": "X",
    "Other": "O",
}

SIMPLE_KIND = {
    "string": "String",
    "integer": "Int",
    "real": "Real",
    "number": "Real",
    "boolean": "Bool",
    "logical": "Logical",
}

# (schema, entity, attribute, simple type) → reason; filled while generating.
OTHER_LOG: list[tuple[str, str, str, str]] = []


def die(msg: str) -> NoReturn:
    raise SystemExit(f"gen_schema_tables: {msg}")


def simple_kind(simple, where: str) -> str:
    """Map an ifcopenshell simple_type to a kind, raising on anything new."""
    s = str(simple).strip("<>")
    if s in SIMPLE_KIND:
        return SIMPLE_KIND[s]
    if s == "binary":
        OTHER_LOG.append((*where.split("|"), "binary"))
        return "Other"
    die(f"{where}: unknown simple type {simple!r}")


def attr_kind(t, where: str) -> str:
    """Classify an attribute's declared type.

    named_type → resolve: entity → Ref, select → Select, enumeration →
    Enum, type_declaration → recurse into its underlying type.
    aggregation_type → List. simple_type → primitive.
    """
    agg = t.as_aggregation_type()
    if agg is not None:
        return "List"
    simple = t.as_simple_type()
    if simple is not None:
        return simple_kind(simple, where)
    named = t.as_named_type()
    if named is None:
        die(f"{where}: parameter type {t!r} is neither aggregation, simple nor named")
    decl = named.declared_type()
    if decl.as_entity() is not None:
        return "Ref"
    if decl.as_select_type() is not None:
        return "Select"
    if decl.as_enumeration_type() is not None:
        return "Enum"
    td = decl.as_type_declaration()
    if td is not None:
        return attr_kind(td.declared_type(), where)
    die(f"{where}: named type {decl!r} has an unknown declaration flavour")


# ---------------------------------------------------------------------------
# Per-schema extraction
# ---------------------------------------------------------------------------


def load_schema(name: str, expected: str):
    sch = ifcopenshell.schema_by_name(name)
    if sch.name() != expected:
        die(f"schema_by_name({name!r}) resolved to {sch.name()!r}, expected {expected!r}")
    return sch


def is_subtype(entity, ancestor_name: str) -> bool:
    e = entity
    while e is not None:
        if e.name() == ancestor_name:
            return True
        e = e.supertype()
    return False


def unit_literals(sch) -> set[str]:
    out: set[str] = set()
    for en in ("IfcUnitEnum", "IfcDerivedUnitEnum"):
        out |= set(sch.declaration_by_name(en).as_enumeration_type().enumeration_items())
    out.discard("USERDEFINED")
    return out


def value_types(sch) -> set[str]:
    """Every defined type reachable from the IfcValue select, recursively."""
    out: set[str] = set()
    stack = [sch.declaration_by_name("IfcValue")]
    while stack:
        d = stack.pop()
        sel = d.as_select_type()
        if sel is not None:
            stack.extend(sel.select_list())
            continue
        if d.as_type_declaration() is None:
            die(f"IfcValue member {d.name()} is not a defined type")
        out.add(d.name())
    return out


def extract(schema_name: str, expected: str) -> dict:
    sch = load_schema(schema_name, expected)
    entities = sorted(sch.entities(), key=lambda e: e.name().upper())

    names = [e.name().upper() for e in entities]
    if len(set(names)) != len(names):
        die(f"{schema_name}: duplicate uppercase entity names")

    type_objects = [e.name().upper() for e in entities if is_subtype(e, "IfcTypeObject")]
    roots = [e.name().upper() for e in entities if is_subtype(e, "IfcRoot")]
    supertype = [
        (e.name().upper(), e.supertype().name().upper())
        for e in entities
        if e.supertype() is not None
    ]

    attrs: list[tuple[str, list[tuple[str, int, str, bool]]]] = []
    predef_pos: list[tuple[str, int]] = []
    objtype_pos: list[tuple[str, int]] = []
    elemtype_pos: list[tuple[str, int]] = []
    predef_enum: list[tuple[str, list[str]]] = []

    for e in entities:
        uc = e.name().upper()
        all_attrs = e.all_attributes()
        derived = e.derived()
        if len(all_attrs) != len(derived):
            die(f"{schema_name}.{e.name()}: all_attributes/derived length mismatch")
        rows = []
        for pos, (a, der) in enumerate(zip(all_attrs, derived)):
            where = f"{schema_name}|{e.name()}|{a.name()}"
            kind = attr_kind(a.type_of_attribute(), where)
            rows.append((a.name(), pos, kind, bool(der)))
            if a.name() == "PredefinedType":
                named = a.type_of_attribute().as_named_type()
                enum = named.declared_type().as_enumeration_type() if named else None
                if enum is None:
                    die(f"{where}: PredefinedType is not an enumeration")
                predef_pos.append((uc, pos))
                predef_enum.append((uc, list(enum.enumeration_items())))
            elif a.name() == "ObjectType":
                objtype_pos.append((uc, pos))
            elif a.name() == "ElementType":
                elemtype_pos.append((uc, pos))
        if uc in roots and (not rows or rows[0][0] != "GlobalId"):
            die(f"{schema_name}.{e.name()}: IfcRoot subtype without GlobalId at position 0")
        attrs.append((uc, rows))

    # Measure / dataType → unit type
    units = unit_literals(sch)
    measure_names = {
        d.name()
        for d in sch.declarations()
        if d.as_type_declaration() is not None and d.name().endswith("Measure")
    }
    vocab = measure_names | value_types(sch)
    measures: list[tuple[str, str | None]] = []
    hand_rows: list[tuple[str, str | None, str]] = []
    for m in sorted(vocab, key=str.upper):
        if m in HAND_MAPPED_MEASURES:
            unit, why = HAND_MAPPED_MEASURES[m]
            if unit is not None and unit not in units:
                die(f"{schema_name}: hand-mapped {m} → {unit} not in unit enums")
            measures.append((m.upper(), unit))
            hand_rows.append((m, unit, why))
            continue
        if m not in measure_names:
            # A non-measure IfcValue member (IfcLabel, IfcBoolean, IfcTime, …):
            # a valid dataType with no unit. Checked before the auto rule so
            # e.g. IfcTime never picks up TIMEUNIT.
            measures.append((m.upper(), None))
            continue
        auto = m
        for text in ("Ifc", "Measure", "Non", "Positive", "Negative"):
            auto = auto.replace(text, "")
        auto = auto.upper() + "UNIT"
        if auto not in units:
            die(f"{schema_name}: {m} has no auto unit type ({auto}) and is not hand-mapped")
        measures.append((m.upper(), auto))
    for m in HAND_MAPPED_MEASURES:
        if m not in vocab:
            die(f"{schema_name}: hand-mapped {m} does not exist in schema")

    # IfcPreDefinedPropertySet subtypes: attributes from index 4 on (after
    # GlobalId, OwnerHistory, Name, Description) with a named declared type.
    predef_pset: list[tuple[str, list[tuple[str, str]]]] = []
    for e in entities:
        if not is_subtype(e, "IfcPreDefinedPropertySet"):
            continue
        rows_t: list[tuple[str, str]] = []
        for pos, a in enumerate(e.all_attributes()):
            if pos < 4:
                continue
            named = a.type_of_attribute().as_named_type()
            if named is None:
                continue
            rows_t.append((a.name(), named.declared_type().name().upper()))
        predef_pset.append((e.name().upper(), rows_t))

    return {
        "entities": names,
        "type_objects": type_objects,
        "roots": roots,
        "supertype": supertype,
        "attrs": attrs,
        "predef_pos": predef_pos,
        "objtype_pos": objtype_pos,
        "elemtype_pos": elemtype_pos,
        "predef_enum": predef_enum,
        "measures": measures,
        "predef_pset": predef_pset,
        "hand_rows": hand_rows,
        "measure_count": len(measure_names),
    }


# ---------------------------------------------------------------------------
# Rendering
# ---------------------------------------------------------------------------

PRELUDE = '''\
//! Per-schema IFC tables for the native IDS engine.
//!
//! Generated by scripts/gen_schema_tables.py from ifcopenshell {version} — DO NOT EDIT BY HAND
//!
//! Regenerate with:
//!
//! ```text
//! {command}
//! ```
//!
//! `tests/test_schema_tables_drift.py` regenerates and byte-compares.
//!
//! The `gen` module at the bottom is data; the accessor API in this
//! prelude is the hand-maintained part of the generator template.
//!
//! Conventions:
//! - Entity names are the UPPERCASE STEP token form (`IFCWALLSTANDARDCASE`).
//!   Lookups accept any ASCII case.
//! - Attribute positions are STEP argument indices: explicit attributes,
//!   inherited first. Attributes redeclared as DERIVE in a subtype keep
//!   their slot (written `*` in STEP) and carry `derived: true`.
//! - Attribute names are the schema's PascalCase and match exactly.
//! - `MEASURE_UNIT_TYPE` covers every `Ifc*Measure` defined type plus every
//!   defined type reachable from the `IfcValue` select (the IDS dataType
//!   vocabulary). `None` means the type carries no unit type.

use super::ir::Schema;
use std::cmp::Ordering;

/// Kind of an explicit attribute, derived from its declared type with
/// named types resolved to their underlying type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AttrKind {
    String,
    Enum,
    Int,
    Real,
    Bool,
    Logical,
    /// Entity reference.
    Ref,
    /// Aggregation (LIST / SET / ARRAY / BAG), including defined types over one.
    List,
    Select,
    /// BINARY only; see the generator's stderr log.
    Other,
}

/// One explicit attribute in STEP positional order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttrDef {
    pub name: &'static str,
    pub pos: u16,
    pub kind: AttrKind,
    /// Redeclared as DERIVE in this entity (value is `*` in STEP).
    pub derived: bool,
}

/// All tables for one schema. Every slice is sorted by its first column
/// (uppercase entity / type name).
pub struct SchemaTables {
    pub entities: &'static [&'static str],
    pub type_objects: &'static [&'static str],
    pub roots: &'static [&'static str],
    pub supertype: &'static [(&'static str, &'static str)],
    pub attrs: &'static [(&'static str, &'static [AttrDef])],
    pub predef_pos: &'static [(&'static str, u16)],
    pub objtype_pos: &'static [(&'static str, u16)],
    pub elemtype_pos: &'static [(&'static str, u16)],
    pub predef_enum: &'static [(&'static str, &'static [&'static str])],
    pub measure_unit_type: &'static [(&'static str, Option<&'static str>)],
    pub predef_pset_attr_type: &'static [(&'static str, &'static [(&'static str, &'static str)])],
}

/// The tables for `schema`.
pub fn tables(schema: Schema) -> &'static SchemaTables {
    match schema {
        Schema::Ifc2x3 => &IFC2X3,
        Schema::Ifc4 => &IFC4,
        Schema::Ifc4x3 => &IFC4X3,
    }
}

/// ASCII-case-insensitive compare of a table key (already uppercase)
/// against a caller-supplied name.
fn cmp_ci(key: &str, name: &str) -> Ordering {
    let (a, b) = (key.as_bytes(), name.as_bytes());
    for (x, y) in a.iter().zip(b.iter()) {
        match x.cmp(&y.to_ascii_uppercase()) {
            Ordering::Equal => continue,
            o => return o,
        }
    }
    a.len().cmp(&b.len())
}

fn find_name(table: &'static [&'static str], name: &str) -> Option<usize> {
    table.binary_search_by(|k| cmp_ci(k, name)).ok()
}

fn find_pair<T: Copy>(table: &'static [(&'static str, T)], name: &str) -> Option<T> {
    table
        .binary_search_by(|(k, _)| cmp_ci(k, name))
        .ok()
        .map(|i| table[i].1)
}

impl SchemaTables {
    pub fn has_entity(&self, entity: &str) -> bool {
        find_name(self.entities, entity).is_some()
    }

    /// The canonical (uppercase, `'static`) spelling of `entity`.
    pub fn canonical(&self, entity: &str) -> Option<&'static str> {
        find_name(self.entities, entity).map(|i| self.entities[i])
    }

    /// IfcTypeObject or one of its subtypes.
    pub fn is_type_object(&self, entity: &str) -> bool {
        find_name(self.type_objects, entity).is_some()
    }

    /// IfcRoot or one of its subtypes (GlobalId at STEP position 0).
    pub fn is_root(&self, entity: &str) -> bool {
        find_name(self.roots, entity).is_some()
    }

    /// Direct supertype (uppercase), `None` for roots and unknown names.
    pub fn supertype(&self, entity: &str) -> Option<&'static str> {
        find_pair(self.supertype, entity)
    }

    /// Reflexive: `is_subtype_of(x, x)` is true for any known entity.
    pub fn is_subtype_of(&self, entity: &str, ancestor: &str) -> bool {
        let mut cur = match self.canonical(entity) {
            Some(c) => c,
            None => return false,
        };
        loop {
            if cmp_ci(cur, ancestor) == Ordering::Equal {
                return true;
            }
            match self.supertype(cur) {
                Some(s) => cur = s,
                None => return false,
            }
        }
    }

    /// Flattened explicit attributes in STEP order; empty for unknown names.
    pub fn attrs(&self, entity: &str) -> &'static [AttrDef] {
        find_pair(self.attrs, entity).unwrap_or(&[])
    }

    /// STEP position of attribute `name` (exact, PascalCase) on `entity`.
    pub fn attr_pos(&self, entity: &str, name: &str) -> Option<u16> {
        self.attrs(entity)
            .iter()
            .find(|a| a.name == name)
            .map(|a| a.pos)
    }

    pub fn predef_pos(&self, entity: &str) -> Option<u16> {
        find_pair(self.predef_pos, entity)
    }

    pub fn objtype_pos(&self, entity: &str) -> Option<u16> {
        find_pair(self.objtype_pos, entity)
    }

    pub fn elemtype_pos(&self, entity: &str) -> Option<u16> {
        find_pair(self.elemtype_pos, entity)
    }

    /// Allowed PredefinedType literals (uppercase, schema order).
    pub fn predef_enum(&self, entity: &str) -> Option<&'static [&'static str]> {
        find_pair(self.predef_enum, entity)
    }

    /// `None`: not a known measure / dataType in this schema.
    /// `Some(None)`: known, carries no unit type.
    /// `Some(Some(u))`: IfcUnitEnum / IfcDerivedUnitEnum literal `u`.
    pub fn unit_type_for_measure(&self, measure: &str) -> Option<Option<&'static str>> {
        find_pair(self.measure_unit_type, measure)
    }

    /// `IfcPreDefinedPropertySet` subtypes only: the attributes from STEP
    /// index 4 on that have a named declared type, as (attribute, declared
    /// type UPPERCASE). `None` when `entity` is not such a subtype.
    pub fn predef_pset_attrs(
        &self,
        entity: &str,
    ) -> Option<&'static [(&'static str, &'static str)]> {
        find_pair(self.predef_pset_attr_type, entity)
    }
}

'''

TESTS = '''
#[cfg(test)]
mod tests {
    use super::*;

    fn all() -> [&'static SchemaTables; 3] {
        [
            tables(Schema::Ifc2x3),
            tables(Schema::Ifc4),
            tables(Schema::Ifc4x3),
        ]
    }

    fn sorted<T>(v: &[T], key: impl Fn(&T) -> &str) -> bool {
        v.windows(2).all(|w| key(&w[0]) < key(&w[1]))
    }

    #[test]
    fn tables_are_strictly_sorted() {
        for t in all() {
            assert!(sorted(t.entities, |s| s));
            assert!(sorted(t.type_objects, |s| s));
            assert!(sorted(t.roots, |s| s));
            assert!(sorted(t.supertype, |p| p.0));
            assert!(sorted(t.attrs, |p| p.0));
            assert!(sorted(t.predef_pos, |p| p.0));
            assert!(sorted(t.objtype_pos, |p| p.0));
            assert!(sorted(t.elemtype_pos, |p| p.0));
            assert!(sorted(t.predef_enum, |p| p.0));
            assert!(sorted(t.measure_unit_type, |p| p.0));
            assert!(sorted(t.predef_pset_attr_type, |p| p.0));
            assert_eq!(t.attrs.len(), t.entities.len());
        }
    }

    #[test]
    fn positions_match_attrs() {
        for t in all() {
            for (e, _) in t.attrs {
                assert_eq!(t.predef_pos(e), t.attr_pos(e, "PredefinedType"));
                assert_eq!(t.objtype_pos(e), t.attr_pos(e, "ObjectType"));
                assert_eq!(t.elemtype_pos(e), t.attr_pos(e, "ElementType"));
                for (i, a) in t.attrs(e).iter().enumerate() {
                    assert_eq!(a.pos as usize, i);
                }
            }
            for r in t.roots {
                assert_eq!(t.attr_pos(r, "GlobalId"), Some(0));
            }
        }
    }

    #[test]
    fn spot_checks() {
        let t4 = tables(Schema::Ifc4);
        assert!(t4.has_entity("IfcWall"));
        assert!(t4.has_entity("IFCWALLSTANDARDCASE"));
        assert!(!t4.has_entity("IFCWAL"));
        assert_eq!(t4.predef_pos("IFCWALL"), Some(8));
        assert_eq!(t4.objtype_pos("IFCWALL"), Some(4));
        assert!(t4.is_root("IFCWALL"));
        assert!(!t4.is_type_object("IFCWALL"));
        assert!(t4.is_type_object("IFCWALLTYPE"));
        assert!(t4.is_subtype_of("IFCWALLSTANDARDCASE", "IfcBuildingElement"));
        assert!(t4.is_subtype_of("IFCWALL", "IFCWALL"));
        assert!(!t4.is_subtype_of("IFCWALL", "IFCSLAB"));
        assert_eq!(t4.supertype("IFCROOT"), None);
        assert!(t4.predef_enum("IFCWALL").unwrap().contains(&"USERDEFINED"));
        assert_eq!(
            t4.unit_type_for_measure("IFCLENGTHMEASURE"),
            Some(Some("LENGTHUNIT"))
        );
        assert_eq!(
            t4.unit_type_for_measure("IfcThermalTransmittanceMeasure"),
            Some(Some("THERMALTRANSMITTANCEUNIT"))
        );
        assert_eq!(t4.unit_type_for_measure("IFCCOUNTMEASURE"), Some(None));
        assert_eq!(t4.unit_type_for_measure("IFCLABEL"), Some(None));
        assert_eq!(t4.unit_type_for_measure("IFCNOSUCHMEASURE"), None);
        let panel = t4.predef_pset_attrs("IFCDOORPANELPROPERTIES").unwrap();
        assert!(panel.contains(&("PanelOperation", "IFCDOORPANELOPERATIONENUM")));
        assert!(t4.predef_pset_attrs("IFCPROPERTYSET").is_none());
        assert!(tables(Schema::Ifc2x3).predef_pset_attr_type.is_empty());
        // IFC2X3 IfcReinforcingBar has BarRole, not PredefinedType.
        let t2 = tables(Schema::Ifc2x3);
        assert_eq!(t2.predef_pos("IFCREINFORCINGBAR"), None);
        assert!(t4.predef_pos("IFCREINFORCINGBAR").is_some());
        // IFC4 IfcDoor: PredefinedType precedes OperationType (see #74).
        let door = t4.predef_pos("IFCDOOR").unwrap();
        assert_eq!(t4.attr_pos("IFCDOOR", "OperationType"), Some(door + 1));
    }
}
'''


def rs_str(s: str) -> str:
    if any(c in s for c in '"\\\n') or not s.isascii():
        die(f"unexpected character in identifier {s!r}")
    return f'"{s}"'


def wrap(items: list[str], indent: str, width: int = 100) -> list[str]:
    """Pack comma-separated items into lines of at most `width` chars."""
    lines: list[str] = []
    cur = indent
    for it in items:
        piece = it + ","
        if cur != indent and len(cur) + 1 + len(piece) > width:
            lines.append(cur)
            cur = indent + piece
        else:
            cur = cur + ("" if cur == indent else " ") + piece
    if cur != indent:
        lines.append(cur)
    return lines


def render_schema(mod: str, d: dict) -> list[str]:
    ind = "        "
    out: list[str] = []
    out.append(f"    pub mod {mod} {{")
    out.append("        use super::*;")
    out.append("")

    out.append(f"        pub static ENTITIES: &[&str] = &[")
    out += wrap([rs_str(n) for n in d["entities"]], ind + "    ")
    out.append("        ];")
    out.append("")
    out.append(f"        pub static IS_TYPE_OBJECT: &[&str] = &[")
    out += wrap([rs_str(n) for n in d["type_objects"]], ind + "    ")
    out.append("        ];")
    out.append("")
    out.append(f"        pub static IS_ROOT: &[&str] = &[")
    out += wrap([rs_str(n) for n in d["roots"]], ind + "    ")
    out.append("        ];")
    out.append("")
    out.append(f"        pub static SUPERTYPE: &[(&str, &str)] = &[")
    out += wrap([f"({rs_str(a)}, {rs_str(b)})" for a, b in d["supertype"]], ind + "    ")
    out.append("        ];")
    out.append("")

    out.append(f"        pub static ATTRS: &[(&str, &[AttrDef])] = &[")
    for ent, rows in d["attrs"]:
        cells = [
            f"{'d' if der else 'a'}({rs_str(n)}, {p}, {KIND_CODE[k]})"
            for n, p, k, der in rows
        ]
        head = f"{ind}    ({rs_str(ent)}, &["
        if not cells:
            out.append(head + "]),")
            continue
        out.append(head)
        out += wrap(cells, ind + "        ")
        out.append(f"{ind}    ]),")
    out.append("        ];")
    out.append("")

    for key, name in (
        ("predef_pos", "PREDEF_POS"),
        ("objtype_pos", "OBJTYPE_POS"),
        ("elemtype_pos", "ELEMTYPE_POS"),
    ):
        out.append(f"        pub static {name}: &[(&str, u16)] = &[")
        out += wrap([f"({rs_str(e)}, {p})" for e, p in d[key]], ind + "    ")
        out.append("        ];")
        out.append("")

    out.append(f"        pub static PREDEF_ENUM: &[(&str, &[&str])] = &[")
    for ent, lits in d["predef_enum"]:
        row = f"{ind}    ({rs_str(ent)}, &[{', '.join(rs_str(x) for x in lits)}]),"
        if len(row) <= 100:
            out.append(row)
        else:
            out.append(f"{ind}    ({rs_str(ent)}, &[")
            out += wrap([rs_str(x) for x in lits], ind + "        ")
            out.append(f"{ind}    ]),")
    out.append("        ];")
    out.append("")

    hand = {m.upper(): why for m, _, why in d["hand_rows"]}
    out.append(f"        pub static MEASURE_UNIT_TYPE: &[(&str, Option<&str>)] = &[")
    for m, unit in d["measures"]:
        u = "None" if unit is None else f"Some({rs_str(unit)})"
        row = f"{ind}    ({rs_str(m)}, {u}),"
        if m in hand:
            row += f" // hand-mapped: {hand[m]}"
        out.append(row)
    out.append("        ];")
    out.append("")

    out.append(f"        pub static PREDEF_PSET_ATTR_TYPE: &[(&str, &[(&str, &str)])] = &[")
    for ent, rows_t in d["predef_pset"]:
        cells = [f"({rs_str(n)}, {rs_str(t)})" for n, t in rows_t]
        head = f"{ind}    ({rs_str(ent)}, &["
        if not cells:
            out.append(head + "]),")
            continue
        out.append(head)
        out += wrap(cells, ind + "        ")
        out.append(f"{ind}    ]),")
    out.append("        ];")
    out.append("    }")
    return out


def render() -> str:
    if ifcopenshell.version != PINNED_IFCOPENSHELL:
        die(
            f"ifcopenshell {ifcopenshell.version} != pinned {PINNED_IFCOPENSHELL}; "
            "the committed tables are pinned to that version"
        )
    OTHER_LOG.clear()
    data = [(mod, extract(name, expected)) for name, expected, mod in SCHEMAS]

    parts: list[str] = [
        PRELUDE.replace("{version}", ifcopenshell.version).replace("{command}", GEN_COMMAND)
    ]
    for mod, _ in data:
        const = mod.upper()
        m = mod.lower()
        parts.append(
            f"static {const}: SchemaTables = SchemaTables {{\n"
            f"    entities: gen::{m}::ENTITIES,\n"
            f"    type_objects: gen::{m}::IS_TYPE_OBJECT,\n"
            f"    roots: gen::{m}::IS_ROOT,\n"
            f"    supertype: gen::{m}::SUPERTYPE,\n"
            f"    attrs: gen::{m}::ATTRS,\n"
            f"    predef_pos: gen::{m}::PREDEF_POS,\n"
            f"    objtype_pos: gen::{m}::OBJTYPE_POS,\n"
            f"    elemtype_pos: gen::{m}::ELEMTYPE_POS,\n"
            f"    predef_enum: gen::{m}::PREDEF_ENUM,\n"
            f"    measure_unit_type: gen::{m}::MEASURE_UNIT_TYPE,\n"
            f"    predef_pset_attr_type: gen::{m}::PREDEF_PSET_ATTR_TYPE,\n"
            f"}};\n\n"
        )

    body: list[str] = [
        "// ---------------------------------------------------------------------------",
        "// Generated data. Kind aliases: S=String E=Enum I=Int R=Real B=Bool L=Logical",
        "// F=Ref V=List X=Select O=Other. `a(name, pos, kind)`; `d(..)` = derived slot.",
        "// ---------------------------------------------------------------------------",
        "#[rustfmt::skip]",
        "#[allow(dead_code)]",
        "mod gen {",
        "    use super::{AttrDef, AttrKind};",
        "",
        "    const fn a(name: &'static str, pos: u16, kind: AttrKind) -> AttrDef {",
        "        AttrDef { name, pos, kind, derived: false }",
        "    }",
        "    const fn d(name: &'static str, pos: u16, kind: AttrKind) -> AttrDef {",
        "        AttrDef { name, pos, kind, derived: true }",
        "    }",
        "    const S: AttrKind = AttrKind::String;",
        "    const E: AttrKind = AttrKind::Enum;",
        "    const I: AttrKind = AttrKind::Int;",
        "    const R: AttrKind = AttrKind::Real;",
        "    const B: AttrKind = AttrKind::Bool;",
        "    const L: AttrKind = AttrKind::Logical;",
        "    const F: AttrKind = AttrKind::Ref;",
        "    const V: AttrKind = AttrKind::List;",
        "    const X: AttrKind = AttrKind::Select;",
        "    const O: AttrKind = AttrKind::Other;",
        "",
    ]
    for i, (mod, d) in enumerate(data):
        body += render_schema(mod.lower(), d)
        if i != len(data) - 1:
            body.append("")
    body.append("}")

    text = "".join(parts) + "\n".join(body) + "\n" + TESTS

    # Stash counts for the size report.
    render.stats = [(mod, d) for mod, d in data]  # type: ignore[attr-defined]
    return text


def main(argv: list[str] | None = None) -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--out", type=Path, default=DEFAULT_OUT)
    ap.add_argument("--stats", action="store_true", help="print counts to stderr")
    args = ap.parse_args(argv)

    text = render()
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(text, encoding="utf-8")

    for sch, ent, attr, simple in OTHER_LOG:
        print(f"Other: {sch}.{ent}.{attr} ({simple})", file=sys.stderr)
    if args.stats:
        for mod, d in render.stats:  # type: ignore[attr-defined]
            n_attr = sum(len(r) for _, r in d["attrs"])
            print(
                f"{mod}: entities={len(d['entities'])} type_objects={len(d['type_objects'])} "
                f"roots={len(d['roots'])} supertype={len(d['supertype'])} attr_rows={n_attr} "
                f"predef_pos={len(d['predef_pos'])} objtype_pos={len(d['objtype_pos'])} "
                f"elemtype_pos={len(d['elemtype_pos'])} predef_enum={len(d['predef_enum'])} "
                f"measure_rows={len(d['measures'])} (Ifc*Measure={d['measure_count']})",
                file=sys.stderr,
            )
            for m, u, why in d["hand_rows"]:
                print(f"  hand: {m} -> {u}  # {why}", file=sys.stderr)
    print(f"wrote {args.out} ({len(text.encode())} bytes)", file=sys.stderr)


if __name__ == "__main__":
    main()
