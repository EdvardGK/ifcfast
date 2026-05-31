"""Compile IfcTester IDS document to Rust ``CompiledIds`` JSON."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any, Literal

FacetKind = Literal[
    "entity",
    "attribute",
    "property",
    "classification",
    "material",
    "part_of",
]


def _cardinality(facet: Any) -> str:
    card = getattr(facet, "cardinality", None) or "required"
    return str(card).lower()


def _value_constraint_from_ids_value(value: Any) -> dict[str, Any] | None:
    if value is None:
        return None
    if isinstance(value, str):
        return {"kind": "simple", "text": value}
    simple = getattr(value, "simpleValue", None)
    if simple is not None:
        return {"kind": "simple", "text": str(simple)}
    options = getattr(value, "options", None)
    restriction = getattr(value, "restriction", None)
    if options is None and restriction is not None:
        options = getattr(restriction, "options", None)
    if options is None:
        return None
    options = options or {}
    if "enumeration" in options:
        raw = options["enumeration"]
        vals = raw if isinstance(raw, list) else [raw]
        return {"kind": "enumeration", "values": [str(v) for v in vals]}
    if "pattern" in options:
        from xmlschema.validators import identities

        raw = options["pattern"]
        pats = raw if isinstance(raw, list) else [raw]
        return {
            "kind": "pattern",
            "patterns": [identities.translate_pattern(str(p)) for p in pats],
        }
    bounds: dict[str, float | None] = {
        "min_inclusive": None,
        "max_inclusive": None,
        "min_exclusive": None,
        "max_exclusive": None,
    }
    for key, out_key in (
        ("minInclusive", "min_inclusive"),
        ("maxInclusive", "max_inclusive"),
        ("minExclusive", "min_exclusive"),
        ("maxExclusive", "max_exclusive"),
    ):
        if key in options:
            bounds[out_key] = float(options[key][0] if isinstance(options[key], list) else options[key])
    if any(v is not None for v in bounds.values()):
        return {"kind": "bounds", **bounds}
    length = min_len = max_len = None
    if "length" in options:
        v = options["length"]
        length = int(v[0] if isinstance(v, list) else v)
    if "minLength" in options:
        v = options["minLength"]
        min_len = int(v[0] if isinstance(v, list) else v)
    if "maxLength" in options:
        v = options["maxLength"]
        max_len = int(v[0] if isinstance(v, list) else v)
    if length is not None or min_len is not None or max_len is not None:
        if sum(x is not None for x in (length, min_len, max_len)) > 1:
            return {
                "kind": "length_bounds",
                "length": length,
                "min": min_len,
                "max": max_len,
            }
        if length is not None:
            return {"kind": "length", "length": length}
        if min_len is not None:
            return {"kind": "min_length", "min": min_len}
        if max_len is not None:
            return {"kind": "max_length", "max": max_len}
    return None


def _value_constraint(facet: Any) -> dict[str, Any] | None:
    return _value_constraint_from_ids_value(getattr(facet, "value", None))


def _attribute_names(facet: Any) -> tuple[str | None, list[str], dict[str, Any] | None]:
    an = getattr(facet, "name", None)
    if an is None:
        return "Name", [], None
    if isinstance(an, str):
        return an, [], None
    bc = _value_constraint_from_ids_value(an)
    if bc is not None:
        if bc.get("kind") == "enumeration":
            vals = bc["values"]
            return None, [str(v) for v in vals], None
        if bc.get("kind") == "simple":
            return str(bc["text"]), [], None
        return None, [], bc
    options = getattr(an, "options", None)
    if options:
        enum = options.get("enumeration")
        if enum is not None:
            vals = enum if isinstance(enum, list) else [enum]
            return None, [str(v) for v in vals], None
    simple = getattr(an, "simpleValue", None)
    if simple is not None:
        return str(simple), [], None
    return "Name", [], None


def _entity_names(facet: Any) -> list[str]:
    name = getattr(facet, "name", None)
    if name is None:
        return []
    if isinstance(name, str):
        return [name.upper()]
    options = getattr(name, "options", None)
    if options:
        enum = options.get("enumeration")
        if enum is not None:
            vals = enum if isinstance(enum, list) else [enum]
            return [str(v).upper() for v in vals]
    if hasattr(name, "enumeration"):
        enum = name.enumeration
        return [str(v).upper() for v in (enum if isinstance(enum, list) else [enum])]
    simple = getattr(name, "simpleValue", None)
    if simple is not None:
        return [str(simple).upper()]
    return []


def _entity_name_fields(facet: Any) -> tuple[list[str], dict[str, Any] | None]:
    name = getattr(facet, "name", None)
    if name is None:
        return [], None
    if isinstance(name, str):
        return [name.upper()], None
    vc = _value_constraint_from_ids_value(name)
    if vc is not None:
        if vc.get("kind") == "enumeration":
            return [str(v).upper() for v in vc["values"]], None
        if vc.get("kind") == "simple":
            return [str(vc["text"]).upper()], None
        return [], vc
    return _entity_names(facet), None


def _predefined_type_fields(facet: Any) -> tuple[str | None, dict[str, Any] | None]:
    pt = getattr(facet, "predefinedType", None)
    if pt is None:
        return None, None
    if isinstance(pt, str):
        return pt, None
    vc = _value_constraint_from_ids_value(pt)
    if vc is not None:
        if vc.get("kind") == "simple":
            return str(vc["text"]), None
        return None, vc
    return str(getattr(pt, "simpleValue", pt) or pt), None


def _facet_to_dict(facet: Any) -> dict[str, Any]:
    from ifctester.facet import Attribute, Classification, Entity, Material, PartOf, Property

    if isinstance(facet, Entity):
        entity_names, name_constraint = _entity_name_fields(facet)
        predefined, predefined_constraint = _predefined_type_fields(facet)
        return {
            "kind": "entity",
            "cardinality": _cardinality(facet),
            "entity_names": entity_names,
            "value": name_constraint,
            "predefined_type": predefined,
            "predefined_type_constraint": predefined_constraint,
        }
    if isinstance(facet, Attribute):
        attr_name, attr_names, attr_name_constraint = _attribute_names(facet)
        return {
            "kind": "attribute",
            "cardinality": _cardinality(facet),
            "attribute_name": attr_name,
            "attribute_names": attr_names,
            "attribute_name_constraint": attr_name_constraint,
            "value": _value_constraint(facet),
        }
    if isinstance(facet, Property):
        psets: list[str] = []
        property_set_constraint: dict[str, Any] | None = None
        ps = getattr(facet, "propertySet", None)
        if ps is not None:
            if isinstance(ps, str):
                psets = [ps]
            else:
                pc = _value_constraint_from_ids_value(ps)
                if pc is not None:
                    if pc.get("kind") == "enumeration":
                        psets = list(pc["values"])
                    elif pc.get("kind") == "simple":
                        psets = [str(pc["text"])]
                    else:
                        property_set_constraint = pc
                else:
                    options = getattr(ps, "options", None)
                    if options and options.get("enumeration") is not None:
                        enum = options["enumeration"]
                        psets = [str(v) for v in (enum if isinstance(enum, list) else [enum])]
                    elif hasattr(ps, "enumeration"):
                        enum = ps.enumeration
                        psets = [str(v) for v in (enum if isinstance(enum, list) else [enum])]
                    elif getattr(ps, "simpleValue", None) is not None:
                        psets = [str(ps.simpleValue)]
        base_names: list[str] = []
        base_name_constraint: dict[str, Any] | None = None
        bn = getattr(facet, "baseName", None)
        if bn is not None:
            if isinstance(bn, str):
                base_names = [bn]
            else:
                bc = _value_constraint_from_ids_value(bn)
                if bc is not None:
                    if bc.get("kind") == "enumeration":
                        base_names = list(bc["values"])
                    elif bc.get("kind") == "simple":
                        base_names = [str(bc["text"])]
                    else:
                        base_name_constraint = bc
                else:
                    options = getattr(bn, "options", None)
                    if options and options.get("enumeration") is not None:
                        enum = options["enumeration"]
                        base_names = [str(v) for v in (enum if isinstance(enum, list) else [enum])]
                    elif hasattr(bn, "enumeration"):
                        enum = bn.enumeration
                        base_names = [str(v) for v in (enum if isinstance(enum, list) else [enum])]
                    elif getattr(bn, "simpleValue", None) is not None:
                        base_names = [str(bn.simpleValue)]
        dt = getattr(facet, "dataType", None)
        data_type = None
        if dt is not None:
            if isinstance(dt, str):
                data_type = dt.upper()
            else:
                data_type = str(getattr(dt, "simpleValue", dt) or dt).upper()
        return {
            "kind": "property",
            "cardinality": _cardinality(facet),
            "property_set": psets[0] if len(psets) == 1 else None,
            "property_sets": psets,
            "property_set_constraint": property_set_constraint,
            "base_name": base_names[0] if len(base_names) == 1 else None,
            "base_names": base_names,
            "base_name_constraint": base_name_constraint,
            "data_type": data_type,
            "value": _value_constraint(facet),
        }
    if isinstance(facet, PartOf):
        rel = getattr(facet, "relation", None)
        relation = None
        if rel is not None:
            relation = str(getattr(rel, "simpleValue", rel) or rel).upper()
        names, name_constraint = _entity_name_fields(facet)
        predefined, predefined_constraint = _predefined_type_fields(facet)
        return {
            "kind": "part_of",
            "cardinality": _cardinality(facet),
            "partof_relation": relation,
            "entity_names": names,
            "value": name_constraint,
            "predefined_type": predefined,
            "predefined_type_constraint": predefined_constraint,
        }
    if isinstance(facet, Classification):
        return {
            "kind": "classification",
            "cardinality": _cardinality(facet),
            "classification_system": _value_constraint_from_ids_value(
                getattr(facet, "system", None)
            ),
            "value": _value_constraint(facet),
        }
    if isinstance(facet, Material):
        return {
            "kind": "material",
            "cardinality": _cardinality(facet),
            "value": _value_constraint(facet),
        }
    raise TypeError(f"Unsupported facet type: {type(facet)}")


def compile_ids_doc(ids_doc: Any, *, ids_path: str | None = None) -> dict[str, Any]:
    """Build ``CompiledIds`` dict from an IfcTester-loaded IDS document."""
    specs = []
    for spec in ids_doc.specifications:
        ifc_versions: list[str] = []
        raw_ver = getattr(spec, "ifcVersion", None) or []
        if isinstance(raw_ver, str):
            ifc_versions = [p.upper() for p in raw_ver.split() if p]
        else:
            for v in raw_ver:
                ifc_versions.extend(p.upper() for p in str(v).split() if p)
        max_occurs = getattr(spec, "maxOccurs", "unbounded")
        if max_occurs == "unbounded":
            max_occurs_out: Any = "unbounded"
        else:
            max_occurs_out = int(max_occurs)
        specs.append(
            {
                "name": spec.name or "Unnamed",
                "ifc_versions": ifc_versions,
                "min_occurs": int(getattr(spec, "minOccurs", 0) or 0),
                "max_occurs": max_occurs_out,
                "applicability": [_facet_to_dict(f) for f in spec.applicability],
                "requirements": [_facet_to_dict(f) for f in spec.requirements],
            }
        )
    return {
        "ids_path": ids_path or getattr(ids_doc, "filepath", None),
        "specifications": specs,
    }


def compile_ids_file(ids_path: str | Path) -> dict[str, Any]:
    from ifcfast.ids.loader import load_ids

    path = Path(ids_path)
    doc = load_ids(path)
    doc.filepath = str(path)
    return compile_ids_doc(doc, ids_path=str(path))


def compile_ids_to_json(ids_path: str | Path, out_path: str | Path | None = None) -> str:
    """Write compiled JSON; return JSON string."""
    payload = compile_ids_file(ids_path)
    text = json.dumps(payload, indent=2)
    if out_path is not None:
        Path(out_path).write_text(text, encoding="utf-8")
    return text


def main() -> None:
    import argparse

    parser = argparse.ArgumentParser(description="Compile .ids to CompiledIds JSON for Rust engine")
    parser.add_argument("ids_path", type=Path)
    parser.add_argument("-o", "--output", type=Path, default=None)
    args = parser.parse_args()
    out = args.output or args.ids_path.with_suffix(".compiled.json")
    compile_ids_to_json(args.ids_path, out)
    print(out)


if __name__ == "__main__":
    main()
