"""One-shot codegen: dump per-schema IFC entity supertype maps.

ifcfast's `classify.py` used to soft-import ``ifcopenshell`` to resolve
an entity's ancestor chain (needed when the classifier's explicit
COUNT/MEASURE/LINEAR/SKIP whitelists miss an entity and the fallback
has to ask "does X ultimately descend from IfcFlowTerminal?").

The IFC schemas are static published standards — IFC2x3, IFC4, IFC4x3.
Their inheritance chains never change at runtime. So we can extract
them ONCE at build time, commit the result as a literal Python dict,
and drop the runtime ``ifcopenshell`` dependency entirely.

Run this script when a new IFC schema lands (or when ``ifcopenshell``
gains a new supported schema version):

    .venv/bin/python scripts/gen_schema_supertypes.py

It writes TWO files from the same schema walk, so they cannot disagree:

* ``python/ifcfast/data/schema_supertypes.py`` (``--py-out``) — the
  Python tables below.
* ``crates/core/src/schema_products.rs`` (``--rust-out``) — the Rust
  tier-1 product whitelist, the ``Tag`` attribute position per product
  class, and the STEP-token → ifcopenshell-spelling table for every
  entity (GH #201). The whitelist used to be a hand list in
  ``indexer.rs``; it drifted three times (GH #178, #186, #201), each
  time dropping a real product class from every table with no warning.
  ``tests/test_schema_codegen_drift_201.py`` regenerates both files and
  byte-compares them.

The generated module exports ``SUPERTYPE: dict[str, dict[str, str]]``
keyed by schema name → entity name → immediate supertype name.
``IfcRoot`` (and any other top-level entity) has no entry — the
classifier reads "missing" as "no further parent".

It also exports ``ALL_ENTITIES: frozenset[str]`` — every entity
declaration name across all supported schemas, *including* the
supertype-less roots (IfcPerson, IfcOwnerHistory, IfcGridAxis, …) that
SUPERTYPE structurally omits. Entity-name validation (GH #71) must
check this set, not SUPERTYPE: a valid root entity is not a typo
(PR #85 review F1).

And ``ABSTRACT: dict[str, frozenset[str]]`` — per schema, the entities
declared ABSTRACT (never instantiated in a file). The product whitelist
drops a class only when it is abstract in EVERY schema that declares it.
"""

from __future__ import annotations

import argparse
import io
import sys
from contextlib import redirect_stdout
from pathlib import Path

import ifcopenshell

SCHEMAS: tuple[str, ...] = ("IFC2X3", "IFC4", "IFC4X3")

REPO = Path(__file__).resolve().parent.parent
PY_OUT = REPO / "python" / "ifcfast" / "data" / "schema_supertypes.py"
RUST_OUT = REPO / "crates" / "core" / "src" / "schema_products.rs"
GEN_COMMAND = ".venv/bin/python scripts/gen_schema_supertypes.py"

#: IfcProduct subtypes the tier-1 indexer routes to their OWN tables
#: (spatial structure: storeys / site / building, and IfcSpace, which it
#: dispatches separately and then also emits as a product row). Every
#: other concrete IfcProduct subtype is a whitelisted product.
INDEXER_DISPATCHED_ELSEWHERE: frozenset[str] = frozenset(
    {"IfcSite", "IfcBuilding", "IfcBuildingStorey", "IfcSpace"}
)


def supertypes_for(schema: str) -> dict[str, str]:
    """Return ``{entity_name: supertype_name}`` for every entity in the
    schema that has a supertype. Entities at the inheritance root
    (``IfcRoot`` is the obvious one, plus a handful of grammar-internal
    abstracts) are omitted — the classifier reads missing keys as
    "no further parent" and terminates the walk."""
    sch = ifcopenshell.schema_by_name(schema)
    out: dict[str, str] = {}
    for decl in sch.declarations():
        # The schema's declaration table includes TYPE / ENUM / SELECT
        # declarations too. Only entity declarations have a supertype
        # chain worth following. `as_entity()` returns None for the
        # non-entity flavours.
        try:
            entity = decl.as_entity()
        except AttributeError:
            continue
        if entity is None:
            continue
        sup = entity.supertype()
        if sup is None:
            continue
        out[entity.name()] = sup.name()
    return out


def entity_names_for(schema: str) -> set[str]:
    """Every entity declaration name in the schema — including the
    supertype-less roots that ``supertypes_for`` omits."""
    sch = ifcopenshell.schema_by_name(schema)
    out: set[str] = set()
    for decl in sch.declarations():
        try:
            entity = decl.as_entity()
        except AttributeError:
            continue
        if entity is None:
            continue
        out.add(entity.name())
    return out


def _entities(schema: str):
    sch = ifcopenshell.schema_by_name(schema)
    for decl in sch.declarations():
        try:
            entity = decl.as_entity()
        except AttributeError:
            continue
        if entity is not None:
            yield entity


def _descends_from(entity, root: str) -> bool:
    cur = entity
    while cur is not None:
        if cur.name() == root:
            return True
        cur = cur.supertype()
    return False


def abstract_for(schema: str) -> set[str]:
    """Entities declared ABSTRACT in ``schema``."""
    return {e.name() for e in _entities(schema) if e.is_abstract()}


def product_whitelist() -> list[str]:
    """Tier-1 product whitelist: every entity that descends from
    ``IfcProduct`` in ANY supported schema and is concrete in at least one
    of them, minus :data:`INDEXER_DISPATCHED_ELSEWHERE`. Title case,
    sorted."""
    concrete: set[str] = set()
    for schema in SCHEMAS:
        for e in _entities(schema):
            if _descends_from(e, "IfcProduct") and not e.is_abstract():
                concrete.add(e.name())
    return sorted(concrete - INDEXER_DISPATCHED_ELSEWHERE)


def tag_positions(products: list[str]) -> dict[str, int]:
    """STEP position of the ``Tag`` attribute for each whitelisted product
    that has one (``IfcElement`` subtypes: 7; ``IfcProxy``: 8). Fails
    loudly if a class puts it at different positions in different
    schemas — the indexer keys on the class alone."""
    wanted = set(products)
    out: dict[str, int] = {}
    for schema in SCHEMAS:
        for e in _entities(schema):
            if e.name() not in wanted:
                continue
            names = [a.name() for a in e.all_attributes()]
            if "Tag" not in names:
                continue
            pos = names.index("Tag")
            prev = out.setdefault(e.name(), pos)
            if prev != pos:
                raise SystemExit(
                    f"{e.name()}.Tag is at STEP position {prev} in one schema "
                    f"and {pos} in {schema}; the indexer cannot key on class"
                )
    return out


def canonical_names() -> dict[str, str]:
    """STEP token (uppercase) → ifcopenshell spelling, for every entity in
    every supported schema. Fails loudly on a spelling conflict."""
    out: dict[str, str] = {}
    for schema in SCHEMAS:
        for e in _entities(schema):
            prev = out.setdefault(e.name().upper(), e.name())
            if prev != e.name():
                raise SystemExit(f"spelling conflict: {prev} vs {e.name()}")
    return out


def _chunked_strings(items: list[str], indent: str, width: int = 96) -> list[str]:
    lines: list[str] = []
    cur = indent
    for it in items:
        piece = it + ","
        if len(cur) + len(piece) + 1 > width and cur.strip():
            lines.append(cur.rstrip())
            cur = indent
        cur += piece + " "
    if cur.strip():
        lines.append(cur.rstrip())
    return lines


def render_rust() -> str:
    products = product_whitelist()
    tags = tag_positions(products)
    names = canonical_names()
    o: list[str] = []
    o.append("//! Tier-1 product whitelist + entity spelling table, generated from the")
    o.append("//! IFC schemas (GH #201).")
    o.append("//!")
    o.append(
        f"//! Generated by scripts/gen_schema_supertypes.py from ifcopenshell "
        f"{ifcopenshell.version} — DO NOT EDIT BY HAND."
    )
    o.append("//!")
    o.append("//! Regenerate with:")
    o.append("//!")
    o.append("//! ```text")
    o.append(f"//! {GEN_COMMAND}")
    o.append("//! ```")
    o.append("//!")
    o.append("//! `tests/test_schema_codegen_drift_201.py` regenerates and byte-compares.")
    o.append("//!")
    o.append("//! [`PRODUCT_TYPES`] is every entity that descends from `IfcProduct` in")
    o.append(f"//! {' / '.join(SCHEMAS)} and is concrete in at least one of them, minus")
    o.append("//! the classes the indexer routes to their own tables:")
    o.append(
        "//! "
        + ", ".join(f"`{n}`" for n in sorted(INDEXER_DISPATCHED_ELSEWHERE))
        + "."
    )
    o.append("//! It replaced a hand list that drifted three times (GH #178, #186, #201).")
    o.append("")
    o.append("#[rustfmt::skip]")
    o.append("mod gen {")
    o.append("    /// Whitelisted product classes, uppercase STEP tokens, sorted.")
    o.append("    pub(crate) const PRODUCT_TYPES: &[&[u8]] = &[")
    o.extend(_chunked_strings(
            [f'b"{p.upper()}"' for p in sorted(products, key=str.upper)], "        "
        ))
    o.append("    ];")
    o.append("")
    o.append("    /// STEP position of `Tag` per product class that has the attribute.")
    o.append("    /// A class absent here has no `Tag` (spatial elements carry `LongName`")
    o.append("    /// at position 7 instead, GH #159).")
    o.append("    pub(crate) const TAG_POSITION: &[(&[u8], usize)] = &[")
    o.extend(
        _chunked_strings(
            [f'(b"{p.upper()}", {tags[p]})' for p in sorted(tags, key=str.upper)],
            "        ",
        )
    )
    o.append("    ];")
    o.append("")
    o.append("    /// STEP token → ifcopenshell spelling for every entity in every schema.")
    o.append("    pub(crate) const ENTITY_NAMES: &[(&[u8], &str)] = &[")
    o.extend(
        _chunked_strings(
            [f'(b"{u}", "{names[u]}")' for u in sorted(names)], "        "
        )
    )
    o.append("    ];")
    o.append("}")
    o.append("")
    o.append("pub(crate) use gen::{ENTITY_NAMES, PRODUCT_TYPES, TAG_POSITION};")
    o.append("")
    return "\n".join(o)


def render_python() -> str:
    buf = io.StringIO()
    with redirect_stdout(buf):
        _print_python()
    return buf.getvalue()


def main(argv: list[str] | None = None) -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--py-out", type=Path, default=PY_OUT)
    ap.add_argument("--rust-out", type=Path, default=RUST_OUT)
    args = ap.parse_args(argv)
    args.py_out.write_text(render_python(), encoding="utf-8")
    args.rust_out.write_text(render_rust(), encoding="utf-8")
    print(f"wrote {args.py_out}", file=sys.stderr)
    print(f"wrote {args.rust_out}", file=sys.stderr)


def _print_python() -> None:
    print('"""Per-schema IFC entity → immediate-supertype map.')
    print()
    print("Generated by scripts/gen_schema_supertypes.py — DO NOT EDIT BY HAND.")
    print("Re-run that script when ifcopenshell's schema bundle changes.")
    print()
    print(f"Source: ifcopenshell {ifcopenshell.version}")
    print(f"Schemas: {', '.join(SCHEMAS)}")
    print('"""')
    print()
    print("from __future__ import annotations")
    print()
    print("SUPERTYPE: dict[str, dict[str, str]] = {")
    for schema in SCHEMAS:
        m = supertypes_for(schema)
        print(f"    {schema!r}: {{")
        for entity in sorted(m):
            print(f"        {entity!r}: {m[entity]!r},")
        print("    },")
    print("}")
    print()
    print("# Every entity name across all supported schemas, including the")
    print("# supertype-less roots SUPERTYPE omits. The vocabulary for")
    print("# entity-name validation (GH #71, PR #85 review F1).")
    print("ALL_ENTITIES: frozenset[str] = frozenset({")
    union: set[str] = set()
    for schema in SCHEMAS:
        union |= entity_names_for(schema)
    for entity in sorted(union):
        print(f"    {entity!r},")
    print("})")
    print()
    print("# Per schema: entities declared ABSTRACT (never instantiated). The")
    print("# tier-1 product whitelist drops a class only when it is abstract in")
    print("# every schema that declares it (GH #201).")
    print("ABSTRACT: dict[str, frozenset[str]] = {")
    for schema in SCHEMAS:
        print(f"    {schema!r}: frozenset({{")
        for entity in sorted(abstract_for(schema)):
            print(f"        {entity!r},")
        print("    }),")
    print("}")


if __name__ == "__main__":
    main()
