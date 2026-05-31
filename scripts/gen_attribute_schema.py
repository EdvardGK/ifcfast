#!/usr/bin/env python3
"""Generate IFC4/IFC2X3 attribute index JSON for native IDS attribute reads."""

from __future__ import annotations

import json
from pathlib import Path

import ifcopenshell

OUT_DIR = Path(__file__).resolve().parents[1] / "crates/core/src/ids/data"


def build_schema_map(schema_name: str) -> dict[str, dict[str, int]]:
    sch = ifcopenshell.schema_by_name(schema_name)
    out: dict[str, dict[str, int]] = {}
    for ent in sch.entities():
        try:
            decl = ent.as_entity()
        except Exception:
            continue
        name = decl.name().upper()
        attrs: dict[str, int] = {}
        for i, a in enumerate(decl.all_attributes()):
            attrs[a.name()] = i
        if attrs:
            out[name] = attrs
    return out


def main() -> None:
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    for label, schema in (("ifc4", "IFC4"), ("ifc2x3", "IFC2X3")):
        data = build_schema_map(schema)
        path = OUT_DIR / f"{label}_attrs.json"
        path.write_text(json.dumps(data), encoding="utf-8")
        print(f"wrote {path} ({len(data)} entity types)")


if __name__ == "__main__":
    main()
