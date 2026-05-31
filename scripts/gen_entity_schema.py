#!/usr/bin/env python3
"""Generate crates/core/src/ids/entity_schema.rs from IFC4 schema."""

from __future__ import annotations

from pathlib import Path

import ifcopenshell

sch = ifcopenshell.schema_by_name("IFC4")
parent: dict[str, str] = {}
for ent in sch.entities():
    try:
        decl = ent.as_entity()
    except Exception:
        continue
    name = decl.name()
    sup = decl.supertype()
    if sup:
        parent[name.upper()] = sup.name().upper()

out = Path(__file__).resolve().parents[1] / "crates/core/src/ids/entity_schema.rs"
lines = [
    "//! IFC4 entity supertype edges for IDS entity-facet subtype matching.",
    "",
    "use std::collections::HashMap;",
    "use std::sync::OnceLock;",
    "",
    "static IFC4_PARENT: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();",
    "",
    "fn ifc4_parent() -> &'static HashMap<&'static str, &'static str> {",
    "    IFC4_PARENT.get_or_init(|| {",
    "        let mut m = HashMap::new();",
]
for c, p in sorted(parent.items()):
    lines.append(f'        m.insert("{c}", "{p}");')
lines += [
    "        m",
    "    })",
    "}",
    "",
    "/// True when `actual` is exactly `facet` or an IFC4 schema subtype of `facet`.",
    "pub fn entity_is_subtype_or_equal(actual: &str, facet: &str) -> bool {",
    "    let actual = actual.trim().to_ascii_uppercase();",
    "    let facet = facet.trim().to_ascii_uppercase();",
    "    if actual == facet {",
    "        return true;",
    "    }",
    "    let map = ifc4_parent();",
    "    let mut cur = actual.as_str();",
    "    while let Some(&p) = map.get(cur) {",
    "        if p == facet.as_str() {",
    "            return true;",
    "        }",
    "        cur = p;",
    "    }",
    "    false",
    "}",
    "",
    "pub fn entity_matches_names(actual: &str, names: &[String], allow_subtypes: bool) -> bool {",
    "    if names.is_empty() {",
    "        return true;",
    "    }",
    "    for n in names {",
    "        let want = n.to_ascii_uppercase();",
    "        if allow_subtypes {",
    "            if entity_is_subtype_or_equal(actual, &want) {",
    "                return true;",
    "            }",
    "        } else if actual.eq_ignore_ascii_case(&want) {",
    "            return true;",
    "        }",
    "    }",
    "    false",
    "}",
]
out.write_text("\n".join(lines) + "\n", encoding="utf-8")
print(f"wrote {out} ({len(parent)} edges)")
