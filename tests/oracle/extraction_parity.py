"""Real-model extraction parity vs ifcopenshell: classifications, materials, psets.

The differential behind GH #216 / #217 / #219 / #213, run over whole
real files rather than tiny fixtures (the gap those reports fell through:
every committed fixture agreed, the reporter's models did not).

Per model it compares, against ifcopenshell as the oracle:

* **classifications** — every product's *direct* references
  (``IfcRelAssociatesClassification``; ifcfast rows with
  ``assignment_source == "instance"``) as the multiset of
  ``(identification, name, location)``, with ``''`` and ``None`` kept
  distinct (GH #217). IFC2X3 ``ItemReference`` / IFC4 ``Identification``
  are read positionally (attribute 1).
* **materials** — for every product, ifcopenshell
  ``get_material(el, should_skip_usage=True)`` projected to ifcfast's rows
  ``(role, index, material_name, item_name, set_name)``:
  ``material_name`` = the item's ``Material.Name`` (GH #219),
  ``item_name`` = the layer / constituent / profile's own ``Name``,
  ``set_name`` = ``LayerSetName`` / set ``Name`` (GH #213). Only products
  with exactly one material association in ifcfast are compared
  (``get_material`` reads the first; ifcfast reports every one).
* **psets** — a deterministic sample of products (every k-th, plus every
  product carrying a predefined property set) through
  ``get_psets(el)`` minus its quantity sets, flattened to
  ``(pset, prop) -> value``; ifcfast's ``m.psets`` rows for the same guid.
  Values are compared after rendering ifcopenshell's natively typed
  value the way the psets table documents it (bool → ``"True"``,
  entity → ``"#id"``, tuple → ``", "``-joined, numbers numerically).
  Complex properties (nested dicts) are flattened ``"Outer.Inner"`` like
  ifcfast does. Predefined-set rows (GH #216) are counted separately.

Run (ONE ifcopenshell-loading process at a time — RAM)::

    python -m tests.oracle.extraction_parity MODEL.ifc [MODEL2.ifc ...] \\
        [--sample 200] [--report out.json]

Exit 1 when any compared item mismatches. Prints the counts per surface.
"""

from __future__ import annotations

import argparse
import json
import math
import sys
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any

import ifcfast

def _is_predefined(defn) -> bool:
    """IfcPropertySetDefinition that get_property_definition reads by attributes.

    (IfcElementQuantity is the only concrete quantity set in every schema.)
    """
    return defn.is_a("IfcPropertySetDefinition") and not (
        defn.is_a("IfcPropertySet") or defn.is_a("IfcElementQuantity")
    )


def _nn(x):
    """pandas null (NaN / NA / None — pandas 3 types string columns as
    ``str`` with NaN for missing) → None; everything else unchanged, so
    ``''`` stays ``''``."""
    if x is None:
        return None
    try:
        import pandas as pd

        if pd.isna(x):
            return None
    except (TypeError, ValueError):
        pass
    return x


# --------------------------------------------------------------------------
# classifications
# --------------------------------------------------------------------------


def classifications(ios, fast) -> dict[str, Any]:
    ours: dict[str, Counter] = defaultdict(Counter)
    df = fast.classifications
    for r in df[df.assignment_source == "instance"].itertuples():
        ours[r.guid][(_nn(r.identification), _nn(r.name), _nn(r.location))] += 1

    # Every rooted object, not only products: the table also lists a type
    # object's own references under the type's guid.
    truth: dict[str, Counter] = defaultdict(Counter)
    n_empty = 0
    for rel in ios.by_type("IfcRelAssociatesClassification"):
        ref = rel.RelatingClassification
        if not ref.is_a("IfcClassificationReference"):
            continue
        ident = ref[1]
        if ident == "":
            n_empty += 1
        key = (ident, ref.Name, ref.Location)
        for obj in rel.RelatedObjects or []:
            g = getattr(obj, "GlobalId", None)
            if g is not None:
                truth[g][key] += 1

    matched = mismatched = 0
    examples = []
    for g in set(ours) | set(truth):
        if ours.get(g, Counter()) == truth.get(g, Counter()):
            matched += sum(truth.get(g, Counter()).values())
        else:
            mismatched += 1
            if len(examples) < 5:
                examples.append({"guid": g, "ours": list(ours.get(g, {}).items()),
                                 "truth": list(truth.get(g, {}).items())})
    empty_rows = int((df.identification == "").sum())
    return {
        "references_with_empty_ident_in_file_rels": n_empty,
        "ifcfast_rows_identification_empty": empty_rows,
        "matched_refs": matched,
        "mismatched_products": mismatched,
        "examples": [str(e) for e in examples],
    }


# --------------------------------------------------------------------------
# materials
# --------------------------------------------------------------------------


def _truth_material_rows(mat) -> list[tuple]:
    def mname(m):
        return m.Name if m is not None else None

    if mat.is_a("IfcMaterial"):
        return [("direct", -1, mat.Name, None, None)]
    if mat.is_a("IfcMaterialList"):
        return [("list", i, m.Name, None, None) for i, m in enumerate(mat.Materials or [])]
    if mat.is_a("IfcMaterialLayerSet"):
        out = []
        for i, layer in enumerate(mat.MaterialLayers or []):
            item = layer.Name if hasattr(layer, "Name") else None
            out.append(("layer", i, mname(layer.Material), item, mat.LayerSetName))
        return out
    if mat.is_a("IfcMaterialConstituentSet"):
        return [
            ("constituent", i, mname(c.Material), c.Name, mat.Name)
            for i, c in enumerate(mat.MaterialConstituents or [])
        ]
    if mat.is_a("IfcMaterialProfileSet"):
        return [
            ("profile", i, mname(p.Material), p.Name, mat.Name)
            for i, p in enumerate(mat.MaterialProfiles or [])
        ]
    return [("unhandled:" + mat.is_a(), -1, None, None, None)]


def materials(ios, fast) -> dict[str, Any]:
    from ifcopenshell.util.element import get_material

    df = fast.materials
    ours: dict[str, list] = defaultdict(list)
    for r in df.itertuples():
        ours[r.guid].append(
            (r.role, int(r.layer_index), _nn(r.material_name), _nn(r.item_name), _nn(r.set_name))
        )

    # products with exactly one association (instance or inherited)
    n_assoc: Counter = Counter()
    for rel in ios.by_type("IfcRelAssociatesMaterial"):
        for o in rel.RelatedObjects or []:
            n_assoc[o.id()] += 1

    roles = Counter()
    matched = mismatched = skipped_multi = 0
    examples = []
    for g in ours:
        el = ios.by_guid(g)
        if n_assoc.get(el.id(), 0) > 1:
            skipped_multi += 1
            continue
        mat = get_material(el, should_skip_usage=True)
        truth = _truth_material_rows(mat) if mat is not None else []
        mine = ours[g]
        for row in mine:
            roles[row[0]] += 1
        if mine == truth:
            matched += len(truth)
        else:
            mismatched += 1
            if len(examples) < 5:
                examples.append({"guid": g, "ours": mine, "truth": truth})
    return {
        "rows_by_role": dict(roles),
        "matched_rows": matched,
        "mismatched_products": mismatched,
        "skipped_multi_association_products": skipped_multi,
        "examples": [str(e) for e in examples],
    }


# --------------------------------------------------------------------------
# psets
# --------------------------------------------------------------------------


def _render(v):
    import ifcopenshell

    if v is None:
        return None
    if isinstance(v, bool):
        return "True" if v else "False"
    if isinstance(v, ifcopenshell.entity_instance):
        return f"#{v.id()}"
    if isinstance(v, (tuple, list)):
        parts = [_render(x) for x in v if x is not None]
        return ", ".join(str(p) for p in parts) if parts else None
    return v


def _values_equal(ours, truth) -> bool:
    if ours is None or truth is None:
        return ours is None and truth is None
    if isinstance(truth, (int, float)) and not isinstance(truth, bool):
        try:
            o = float(ours)
        except (TypeError, ValueError):
            return False
        return math.isclose(o, float(truth), rel_tol=1e-9, abs_tol=1e-12)
    return str(ours) == str(truth)


def _flatten(d: dict, prefix: str = "") -> dict:
    out = {}
    for k, v in d.items():
        if k == "id":
            continue
        if isinstance(v, dict):
            inner = v.get("properties", v)
            out.update(_flatten(inner, f"{prefix}{k}."))
        else:
            out[f"{prefix}{k}"] = v
    return out


def psets(ios, fast, sample: int) -> dict[str, Any]:
    from ifcopenshell.util.element import get_psets

    products = list(fast.products_df.guid)
    step = max(len(products) // max(sample, 1), 1)
    chosen = set(products[::step][:sample])

    # every product (or its type) carrying a predefined set
    predef_guids: set[str] = set()
    for rel in ios.by_type("IfcRelDefinesByProperties"):
        d = rel.RelatingPropertyDefinition
        defs = d if isinstance(d, tuple) else (d,)
        if any(_is_predefined(x) for x in defs):
            for o in rel.RelatedObjects or []:
                if getattr(o, "GlobalId", None):
                    predef_guids.add(o.GlobalId)
    for rel in ios.by_type("IfcRelDefinesByType"):
        t = rel.RelatingType
        if any(_is_predefined(x) for x in (t.HasPropertySets or [])):
            for o in rel.RelatedObjects or []:
                predef_guids.add(o.GlobalId)
    product_set = set(products)
    chosen |= predef_guids & product_set

    df = fast.psets
    df = df[df.guid.isin(chosen)]
    ours: dict[str, dict] = defaultdict(dict)
    for r in df.itertuples():
        ours[r.guid][(r.pset_name, r.prop_name)] = _nn(r.value)

    total = matched = 0
    missing_in_ours = extra_in_ours = value_mismatch = 0
    predef_total = predef_matched = 0
    examples = []
    for g in sorted(chosen):
        el = ios.by_guid(g)
        full = get_psets(el)
        qto_names = set(get_psets(el, qtos_only=True))
        predef_names = set()
        defs = []
        for rel in getattr(el, "IsDefinedBy", None) or []:
            if rel.is_a("IfcRelDefinesByProperties"):
                d = rel.RelatingPropertyDefinition
                defs.extend(d if isinstance(d, tuple) else (d,))
        t = ifcopenshell_get_type(el)
        if t is not None:
            defs.extend(t.HasPropertySets or [])
        for d in defs:
            if _is_predefined(d):
                predef_names.add(d.Name or "")
        truth = {}
        for pset, props in full.items():
            if pset in qto_names:
                continue
            for prop, v in _flatten(props).items():
                truth[(pset if pset is not None else "", prop)] = _render(v)
        mine = ours.get(g, {})
        for key, tv in truth.items():
            total += 1
            is_pre = key[0] in predef_names
            predef_total += is_pre
            if key not in mine:
                missing_in_ours += 1
                if len(examples) < 8:
                    examples.append(f"missing {g} {key} truth={tv!r}")
                continue
            if _values_equal(mine[key], tv):
                matched += 1
                predef_matched += is_pre
            else:
                value_mismatch += 1
                if len(examples) < 8:
                    examples.append(f"value {g} {key} ours={mine[key]!r} truth={tv!r}")
        for key in mine:
            if key not in truth:
                extra_in_ours += 1
                if len(examples) < 8:
                    examples.append(f"extra {g} {key} ours={mine[key]!r}")
    return {
        "sampled_products": len(chosen),
        "products_with_predefined_sets": len(predef_guids & product_set),
        "properties_compared": total,
        "matched": matched,
        "missing_in_ifcfast": missing_in_ours,
        "extra_in_ifcfast": extra_in_ours,
        "value_mismatch": value_mismatch,
        "predefined_properties": predef_total,
        "predefined_matched": predef_matched,
        "examples": examples,
    }


def ifcopenshell_get_type(el):
    from ifcopenshell.util.element import get_type

    return get_type(el)


# --------------------------------------------------------------------------


def run_model(path: Path, sample: int) -> dict[str, Any]:
    import ifcopenshell

    fast = ifcfast.open(path, use_cache=False, write_cache=False)
    ios = ifcopenshell.open(str(path))
    return {
        "model": path.name,
        "schema": ios.schema,
        "ifcopenshell": ifcopenshell.version,
        "classifications": classifications(ios, fast),
        "materials": materials(ios, fast),
        "psets": psets(ios, fast, sample),
    }


def is_clean(rep: dict[str, Any]) -> bool:
    return (
        rep["classifications"]["mismatched_products"] == 0
        and rep["materials"]["mismatched_products"] == 0
        and rep["psets"]["missing_in_ifcfast"] == 0
        and rep["psets"]["extra_in_ifcfast"] == 0
        and rep["psets"]["value_mismatch"] == 0
    )


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("models", nargs="+", type=Path)
    ap.add_argument("--sample", type=int, default=200)
    ap.add_argument("--report", type=Path)
    a = ap.parse_args(argv)
    reports = []
    ok = True
    for p in a.models:
        rep = run_model(p, a.sample)
        reports.append(rep)
        ok &= is_clean(rep)
        print(json.dumps(rep, indent=1, default=str))
    if a.report:
        a.report.write_text(json.dumps(reports, indent=1, default=str))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
