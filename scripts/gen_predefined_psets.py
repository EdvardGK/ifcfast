"""One-shot codegen: predefined property-set tables for `m.psets` (GH #216).

Writes ``crates/core/src/extractors/predefined_psets.rs``: per schema
(IFC2X3 / IFC4 / IFC4X3), every concrete entity that
``ifcopenshell.util.element.get_psets`` reads as a property set through
its attribute-walk branch — i.e. every concrete subtype of
``IfcPropertySetDefinition`` that is not ``IfcPropertySet`` and not a
quantity set (``IfcElementQuantity`` / ``IfcQuantitySet``). In IFC4+ that
is the ``IfcPreDefinedPropertySet`` family (``IfcDoorLiningProperties``,
``IfcDoorPanelProperties``, ``IfcWindow*Properties``,
``IfcPermeableCoveringProperties``,
``IfcReinforcementDefinitionProperties``); IFC2X3 has no such supertype and
hangs the same classes (plus the energy / sound / fluid-flow / service-life
ones) directly under ``IfcPropertySetDefinition``.

For each entity the table lists the explicit attributes from STEP index 4
on (after GlobalId / OwnerHistory / Name / Description), in positional
order, as ``(name, kind, declared type)``:

* ``kind`` drives how the Rust side renders the STEP value to the psets
  table's string ``value``;
* ``declared type`` is the attribute's named type in ifcopenshell
  title-case (``IfcPositiveLengthMeasure``, ``IfcDoorPanelOperationEnum``,
  ``IfcShapeAspect``) and becomes the row's ``value_type``. ``None`` for a
  SELECT (the row takes the value's own wrapper, as ``IfcPropertySingleValue``
  rows do) and for a bare simple type.

ifcopenshell is used at GENERATION time only; the output is committed and
``tests/test_predefined_psets_drift.py`` regenerates and byte-compares.

Run (ifcopenshell must be exactly the pinned version):

    .venv/bin/python scripts/gen_predefined_psets.py
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path
from typing import NoReturn

import ifcopenshell

PINNED_IFCOPENSHELL = "0.8.5"

SCHEMAS: tuple[tuple[str, str, str], ...] = (
    ("IFC2X3", "IFC2X3", "IFC2X3"),
    ("IFC4", "IFC4", "IFC4"),
    ("IFC4X3", "IFC4X3_ADD2", "IFC4X3"),
)

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_OUT = REPO_ROOT / "crates" / "core" / "src" / "extractors" / "predefined_psets.rs"
GEN_COMMAND = ".venv/bin/python scripts/gen_predefined_psets.py"

# Sets get_property_definition reads through a member list, not attributes.
EXCLUDED_ROOTS = ("IfcPropertySet", "IfcElementQuantity", "IfcQuantitySet")

SIMPLE_KIND = {
    "string": "String",
    "integer": "Int",
    "real": "Real",
    "number": "Real",
    "boolean": "Bool",
    "logical": "Logical",
}


def die(msg: str) -> NoReturn:
    print(f"gen_predefined_psets: {msg}", file=sys.stderr)
    sys.exit(1)


def resolve(t, where: str) -> tuple[str, str | None]:
    """(kind, declared type name) for an attribute's declared type."""
    agg = t.as_aggregation_type()
    if agg is not None:
        inner = agg.type_of_element()
        named = inner.as_named_type()
        return "List", (named.declared_type().name() if named is not None else None)
    simple = t.as_simple_type()
    if simple is not None:
        s = simple.declared_type()
        if s not in SIMPLE_KIND:
            die(f"{where}: unsupported simple type {s!r}")
        return SIMPLE_KIND[s], None
    named = t.as_named_type()
    if named is None:
        die(f"{where}: type {t!r} is neither aggregation, simple nor named")
    decl = named.declared_type()
    name = decl.name()
    if decl.as_entity() is not None:
        return "Entity", name
    if decl.as_select_type() is not None:
        return "Select", None
    if decl.as_enumeration_type() is not None:
        return "Enum", name
    td = decl.as_type_declaration()
    if td is None:
        die(f"{where}: named type {name!r} of unknown declaration kind")
    kind, _ = resolve(td.declared_type(), where)
    if kind in ("Entity", "Select"):
        die(f"{where}: defined type {name!r} over {kind} is not expected")
    return kind, name


def subtypes(e) -> list:
    out = []
    for c in e.subtypes():
        out.append(c)
        out.extend(subtypes(c))
    return out


def is_under(e, names: tuple[str, ...]) -> bool:
    while e is not None:
        if e.name() in names:
            return True
        e = e.supertype()
    return False


def collect(schema_name: str, expected: str) -> list[tuple[str, list[tuple[str, str, str | None]]]]:
    sch = ifcopenshell.schema_by_name(schema_name)
    if sch.name() != expected:
        die(f"schema_by_name({schema_name!r}) resolved to {sch.name()!r}, expected {expected!r}")
    base = sch.declaration_by_name("IfcPropertySetDefinition")
    rows = []
    for e in subtypes(base):
        if e.is_abstract() or is_under(e, EXCLUDED_ROOTS):
            continue
        attrs = e.all_attributes()
        head = [a.name() for a in attrs[:4]]
        if head != ["GlobalId", "OwnerHistory", "Name", "Description"]:
            die(f"{expected}.{e.name()}: unexpected header {head}")
        cols = []
        for a in attrs[4:]:
            kind, decl = resolve(a.type_of_attribute(), f"{expected}.{e.name()}.{a.name()}")
            cols.append((a.name(), kind, decl))
        rows.append((e.name().upper(), cols))
    rows.sort(key=lambda r: r[0])
    return rows


def rust_opt(s: str | None) -> str:
    return "None" if s is None else f'Some("{s}")'


def render() -> str:
    out = [
        "//! Predefined property sets read into `m.psets` (GH #216).",
        "//!",
        f"//! Generated by scripts/gen_predefined_psets.py from ifcopenshell {PINNED_IFCOPENSHELL} — DO NOT EDIT BY HAND",
        "//!",
        "//! Regenerate with:",
        "//!",
        "//! ```text",
        f"//! {GEN_COMMAND}",
        "//! ```",
        "//!",
        "//! `tests/test_predefined_psets_drift.py` regenerates and byte-compares.",
        "//!",
        "//! Per schema: every concrete `IfcPropertySetDefinition` subtype that is",
        "//! neither `IfcPropertySet` nor a quantity set, with its explicit",
        "//! attributes from STEP index 4 on as (name, kind, declared type). Rows",
        "//! are sorted by the UPPERCASE entity name.",
        "",
        "/// How a predefined-set attribute's STEP value is rendered.",
        "#[derive(Clone, Copy, Debug, PartialEq, Eq)]",
        "pub enum PredefKind {",
        "    String,",
        "    Enum,",
        "    Int,",
        "    Real,",
        "    Bool,",
        "    Logical,",
        "    /// Entity reference (`#id`).",
        "    Entity,",
        "    /// SELECT: a typed wrapper (`IFCPOWERMEASURE(1.)`) or a reference.",
        "    Select,",
        "    /// LIST / SET of members.",
        "    List,",
        "}",
        "",
        "/// One attribute: (name, kind, declared type in ifcopenshell title-case).",
        "pub type PredefAttr = (&'static str, PredefKind, Option<&'static str>);",
        "",
    ]
    for _, expected, const in SCHEMAS:
        rows = collect(_, expected)
        out.append(f"/// {expected}.")
        out.append("#[rustfmt::skip]")
        out.append(f"pub static {const}: &[(&str, &[PredefAttr])] = &[")
        for ent, cols in rows:
            out.append(f'    ("{ent}", &[')
            for name, kind, decl in cols:
                out.append(f'        ("{name}", PredefKind::{kind}, {rust_opt(decl)}),')
            out.append("    ]),")
        out.append("];")
        out.append("")
    return "\n".join(out)


def main(argv: list[str] | None = None) -> None:
    if ifcopenshell.version != PINNED_IFCOPENSHELL:
        die(f"ifcopenshell {ifcopenshell.version} != pinned {PINNED_IFCOPENSHELL}")
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--out", type=Path, default=DEFAULT_OUT)
    args = ap.parse_args(argv)
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(render(), encoding="utf-8")


if __name__ == "__main__":
    main()
