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
* **quantities** — every IfcProduct carrying an IfcElementQuantity (own
  or via its type; all of them, every k-th above 20 000) vs
  ``m.quantities``: value per ``(qto_name, quantity_name)`` (numeric) and
  ``source`` (``instance`` shadows ``type`` per quantity). The harness'
  own walk is cross-checked against ``get_psets(qtos_only=True)``.
* **attributes** — every ``products_df`` row vs its entity (by step_id):
  entity, Name, ObjectType, Tag, PredefinedType, type guid/name/source
  (documented three-tier: RelDefinesByType > truthy ObjectType > none),
  ``''`` vs None distinct; plus coverage of every IfcProduct except
  IfcSite / IfcBuilding / IfcBuildingStorey.
* **storeys** — ``m.storeys`` vs IfcBuildingStorey: name, raw
  ``elevation``, ``elevation_m`` (× ``calculate_unit_scale``),
  ``building_guid`` (IfcRelAggregates parent).
* **containment** — ``storey_guid`` vs the GH #88 walk (spatial
  container, else IfcRelAggregates parent, up to a storey; voids / nests
  are NOT climbed — ``get_container`` disagreements on those are
  reported as information only) and ``parent_guid`` vs the
  IfcRelAggregates parent.

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


def _attr(el, name):
    """``getattr`` that tolerates a short STEP record (fewer arguments
    than the schema declares — ifcopenshell raises ``Index N is out of
    range``); an absent trailing argument reads as None, which is what
    ifcfast's indexer reports for it."""
    try:
        return getattr(el, name, None)
    except RuntimeError:
        return None


def _defs_of(el) -> list:
    """Every IfcPropertySetDefinition bound to ``el`` by
    IfcRelDefinesByProperties, set-valued RelatingPropertyDefinition
    (IFC4 IfcPropertySetDefinitionSet / inline tuple) expanded."""
    out = []
    for rel in getattr(el, "IsDefinedBy", None) or []:
        if rel.is_a("IfcRelDefinesByProperties"):
            d = rel.RelatingPropertyDefinition
            if isinstance(d, tuple):
                out.extend(d)
            elif d.is_a("IfcPropertySetDefinitionSet"):
                out.extend(d.wrappedValue)
            else:
                out.append(d)
    return out


# --------------------------------------------------------------------------
# quantities
# --------------------------------------------------------------------------


def _qto_flat(qto) -> dict:
    from ifcopenshell.util.element import get_property_definition

    return _flatten(get_property_definition(qto))


def quantities(ios, fast, max_products: int = 20000) -> dict[str, Any]:
    """Every IfcProduct carrying an IfcElementQuantity (own or via its
    type) vs ``m.quantities``: value per ``(qto_name, quantity_name)``
    and ``source`` (``instance`` shadows ``type`` per quantity, like
    ``get_psets``' ``update`` merge)."""
    from ifcopenshell.util.element import get_psets

    # truth: guid -> {(qto, q): (value, source)}
    truth_by_guid: dict[str, dict] = {}
    getpsets_disagree = 0
    getpsets_examples = []
    for el in ios.by_type("IfcProduct"):
        inst = [d for d in _defs_of(el) if d.is_a("IfcElementQuantity")]
        t = ifcopenshell_get_type(el)
        typ = [d for d in (t.HasPropertySets or []) if d.is_a("IfcElementQuantity")] if t else []
        if not inst and not typ:
            continue
        tr: dict = {}
        for src, defs in (("type", typ), ("instance", inst)):
            for d in defs:
                qn = d.Name if d.Name is not None else ""
                for k, v in _qto_flat(d).items():
                    tr[(qn, k)] = (_render(v), src)
        # cross-check the harness' own walk against get_psets (oracle API)
        gp = {}
        for qn, props in get_psets(el, qtos_only=True).items():
            for k, v in _flatten(props).items():
                gp[(qn if qn is not None else "", k)] = _render(v)
        if gp != {k: v for k, (v, _) in tr.items()}:
            getpsets_disagree += 1
            if len(getpsets_examples) < 3:
                getpsets_examples.append(el.GlobalId)
        truth_by_guid[el.GlobalId] = tr

    guids = sorted(truth_by_guid)
    step = 1
    if len(guids) > max_products:
        step = math.ceil(len(guids) / max_products)
        guids = guids[::step]
    chosen = set(guids)

    df = fast.quantities
    product_guids = set(fast.products_df.guid)
    ours: dict[str, dict] = defaultdict(dict)
    dup_rows = 0
    for r in df.itertuples():
        if r.guid not in chosen and not (step == 1 and r.guid in product_guids):
            continue
        key = (r.qto_name, r.quantity_name)
        if key in ours[r.guid]:
            dup_rows += 1
        ours[r.guid][key] = (_nn(r.value), r.source)

    compared = matched = missing = extra = value_mm = source_mm = 0
    source_counts: Counter = Counter()
    examples = []
    for g in sorted(chosen | set(ours)):
        tr = truth_by_guid.get(g, {})
        mine = ours.get(g, {})
        for key, (tv, tsrc) in tr.items():
            compared += 1
            source_counts[tsrc] += 1
            if key not in mine:
                missing += 1
                if len(examples) < 8:
                    examples.append(f"missing {g} {key} truth={tv!r} ({tsrc})")
                continue
            ov, osrc = mine[key]
            ok = True
            if not _values_equal(ov, tv):
                value_mm += 1
                ok = False
                if len(examples) < 8:
                    examples.append(f"value {g} {key} ours={ov!r} truth={tv!r}")
            if osrc != tsrc:
                source_mm += 1
                ok = False
                if len(examples) < 8:
                    examples.append(f"source {g} {key} ours={osrc!r} truth={tsrc!r}")
            matched += ok
        for key in mine:
            if key not in tr:
                extra += 1
                if len(examples) < 8:
                    examples.append(f"extra {g} {key} ours={mine[key]!r}")
    return {
        "products_with_quantities": len(truth_by_guid),
        "products_compared": len(chosen),
        "sampling_step": step,
        "quantities_compared": compared,
        "truth_by_source": dict(source_counts),
        "matched": matched,
        "missing_in_ifcfast": missing,
        "extra_in_ifcfast": extra,
        "value_mismatch": value_mm,
        "source_mismatch": source_mm,
        "ifcfast_duplicate_key_rows": dup_rows,
        "harness_walk_vs_get_psets_disagree": getpsets_disagree,
        "harness_walk_vs_get_psets_examples": getpsets_examples,
        "examples": examples,
    }


# --------------------------------------------------------------------------
# attributes (products_df vs entity attributes) + coverage
# --------------------------------------------------------------------------

_ATTR_FIELDS = ("guid", "entity", "name", "object_type", "tag",
                "predefined_type", "type_guid", "type_name", "type_source")
_SPATIAL_OWN_TABLE = ("IfcSite", "IfcBuilding", "IfcBuildingStorey")


def attributes(ios, fast) -> dict[str, Any]:
    """Every products_df row vs its STEP entity (looked up by step_id).

    ``type_name`` follows the documented three-tier resolution
    (model.py / AGENTS.md): IfcRelDefinesByType type's Name, else a
    truthy ``ObjectType`` (``type_source='objecttype'``), else None.
    IFC2X3 IfcSpace / IfcRoof trailing enums are not PredefinedType
    (GH #159) — counted under ``ifc2x3_trailing_enum_not_predefined``."""
    df = fast.products_df
    compared: Counter = Counter()
    mismatched: Counter = Counter()
    examples: dict[str, list] = defaultdict(list)
    ifc2x3_special = 0
    rows_seen: set[int] = set()

    def chk(field, g, ours, truth):
        compared[field] += 1
        if ours != truth:
            mismatched[field] += 1
            if len(examples[field]) < 5:
                examples[field].append(f"{g}: ours={ours!r} truth={truth!r}")

    for r in df.itertuples():
        sid = int(r.step_id)
        rows_seen.add(sid)
        el = ios.by_id(sid)
        g = r.guid
        chk("guid", g, g, el.GlobalId)
        chk("entity", g, r.entity, el.is_a())
        chk("name", g, _nn(r.name), el.Name)
        chk("object_type", g, _nn(r.object_type), _attr(el, "ObjectType"))
        chk("tag", g, _nn(r.tag), _attr(el, "Tag"))
        pt = _attr(el, "PredefinedType")
        if (ios.schema == "IFC2X3" and el.is_a() in ("IfcSpace", "IfcRoof")):
            ifc2x3_special += 1
        chk("predefined_type", g, _nn(r.predefined_type), pt)
        t = ifcopenshell_get_type(el)
        ot = _attr(el, "ObjectType")
        if t is not None:
            tg, tn, ts = t.GlobalId, t.Name, "ifctype"
        elif ot:
            tg, tn, ts = None, ot, "objecttype"
        else:
            tg, tn, ts = None, None, "none"
        chk("type_guid", g, _nn(r.type_guid), tg)
        chk("type_name", g, _nn(r.type_name), tn)
        chk("type_source", g, r.type_source, ts)

    # coverage
    truth_ids = {
        el.id(): el.is_a()
        for el in ios.by_type("IfcProduct")
        if not any(el.is_a(c) for c in _SPATIAL_OWN_TABLE)
    }
    missing_by_class = Counter(c for i, c in truth_ids.items() if i not in rows_seen)
    extra_by_class = Counter(
        ios.by_id(i).is_a() for i in rows_seen if i not in truth_ids
    )
    dup_rows = len(df) - len(rows_seen)
    return {
        "rows": len(df),
        "duplicate_step_id_rows": dup_rows,
        "compared": dict(compared),
        "mismatched": dict(mismatched),
        "ifc2x3_trailing_enum_not_predefined": ifc2x3_special,
        "coverage_truth_products": len(truth_ids),
        "coverage_missing_in_ifcfast_by_class": dict(missing_by_class),
        "coverage_extra_in_ifcfast_by_class": dict(extra_by_class),
        "examples": dict(examples),
    }


# --------------------------------------------------------------------------
# storeys
# --------------------------------------------------------------------------


def _num_eq(a, b) -> bool:
    if a is None or b is None:
        return a is None and b is None
    return math.isclose(float(a), float(b), rel_tol=1e-9, abs_tol=1e-9)


def storeys(ios, fast) -> dict[str, Any]:
    import ifcopenshell.util.unit

    scale = ifcopenshell.util.unit.calculate_unit_scale(ios)
    ours = {s.guid: s for s in fast.storeys}
    truth = {s.GlobalId: s for s in ios.by_type("IfcBuildingStorey")}
    compared: Counter = Counter()
    mismatched: Counter = Counter()
    examples: dict[str, list] = defaultdict(list)

    def chk(field, g, o, t, eq=lambda a, b: a == b):
        compared[field] += 1
        if not eq(o, t):
            mismatched[field] += 1
            if len(examples[field]) < 5:
                examples[field].append(f"{g}: ours={o!r} truth={t!r}")

    for g in sorted(set(ours) & set(truth)):
        o, s = ours[g], truth[g]
        chk("name", g, o.name, s.Name)
        chk("elevation", g, o.elevation, s.Elevation, _num_eq)
        tm = s.Elevation * scale if s.Elevation is not None else None
        chk("elevation_m", g, o.elevation_m, tm, _num_eq)
        b = None
        for rel in s.Decomposes or []:
            if rel.is_a("IfcRelAggregates") and rel.RelatingObject.is_a("IfcBuilding"):
                b = rel.RelatingObject.GlobalId
        chk("building_guid", g, o.building_guid, b)
    return {
        "unit_scale_ifcopenshell": scale,
        "unit_scale_ifcfast": fast.unit_scale,
        "storeys_truth": len(truth),
        "storeys_ifcfast": len(fast.storeys),
        "missing_in_ifcfast": sorted(set(truth) - set(ours)),
        "extra_in_ifcfast": sorted(set(ours) - set(truth)),
        "compared": dict(compared),
        "mismatched": dict(mismatched),
        "examples": dict(examples),
    }


# --------------------------------------------------------------------------
# containment (storey_guid transitive, parent_guid = IfcRelAggregates parent)
# --------------------------------------------------------------------------


def _agg_parent(el):
    """IfcRelAggregates parent only (IFC2X3 ``Decomposes`` also carries
    IfcRelNests, which ifcfast keeps in ``m.nests``, not here)."""
    for rel in getattr(el, "Decomposes", None) or []:
        if rel.is_a("IfcRelAggregates"):
            return rel.RelatingObject
    return None


def _truth_storey(el, budget: int = 16):
    """Storey the product resolves to: direct spatial container, walked
    up through IfcRelAggregates until an IfcBuildingStorey; a product
    with no containment inherits from its aggregate parent (GH #88).

    Returns (storey_or_None, how) with how in direct / via_container /
    via_aggregate / none."""
    cur = el
    how = None
    seen = set()
    for _ in range(budget):
        if cur.id() in seen:
            return None, "cycle"
        seen.add(cur.id())
        cis = getattr(cur, "ContainedInStructure", None) or []
        if cis:
            c = cis[0].RelatingStructure
            if c.is_a("IfcBuildingStorey"):
                return c, how or "direct"
            how = how or "via_container"
            cur = c
            continue
        p = _agg_parent(cur)
        if p is None:
            return None, "none"
        if p.is_a("IfcBuildingStorey"):
            return p, how or "via_aggregate"
        how = how or "via_aggregate"
        cur = p
    return None, "budget"


def containment(ios, fast) -> dict[str, Any]:
    from ifcopenshell.util.element import get_container

    compared = Counter()
    mismatched = Counter()
    examples: dict[str, list] = defaultdict(list)
    how_counts: Counter = Counter()
    transitive_mismatch = 0
    multi_container = 0
    ios_get_container_disagree = 0
    ios_gc_examples = []

    def chk(field, g, o, t):
        compared[field] += 1
        if o != t:
            mismatched[field] += 1
            if len(examples[field]) < 5:
                examples[field].append(f"{g}: ours={o!r} truth={t!r}")
            return False
        return True

    for r in fast.products_df.itertuples():
        el = ios.by_id(int(r.step_id))
        if len(getattr(el, "ContainedInStructure", None) or []) > 1:
            multi_container += 1
        st, how = _truth_storey(el)
        how_counts[how] += 1
        tg = st.GlobalId if st is not None else None
        ok = chk("storey_guid", r.guid, _nn(r.storey_guid), tg)
        if not ok and how == "via_aggregate":
            transitive_mismatch += 1
        if st is not None:
            chk("storey_name", r.guid, _nn(r.storey_name), st.Name)
        # informational: ifcopenshell's own resolver
        gc = get_container(el, ifc_class="IfcBuildingStorey")
        if (gc.GlobalId if gc is not None else None) != tg:
            ios_get_container_disagree += 1
            if len(ios_gc_examples) < 5:
                ios_gc_examples.append(
                    f"{r.guid}: get_container={gc.GlobalId if gc else None} walk={tg}")
        p = _agg_parent(el)
        pt = p.GlobalId if p is not None and (
            p.is_a("IfcProduct") or p.is_a("IfcProject")) else None
        chk("parent_guid", r.guid, _nn(r.parent_guid), pt)
    return {
        "compared": dict(compared),
        "mismatched": dict(mismatched),
        "truth_resolution": dict(how_counts),
        "storey_mismatch_on_transitive_only": transitive_mismatch,
        "products_with_multiple_containers": multi_container,
        "ifcopenshell_get_container_vs_walk_disagree": ios_get_container_disagree,
        "ifcopenshell_get_container_examples": ios_gc_examples,
        "examples": dict(examples),
    }


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
        "quantities": quantities(ios, fast),
        "attributes": attributes(ios, fast),
        "storeys": storeys(ios, fast),
        "containment": containment(ios, fast),
    }


def surface_status(rep: dict[str, Any]) -> dict[str, bool]:
    """Per-surface clean flag; ``is_clean`` is the conjunction. The corpus
    gate reads this so a known divergence can be registered per surface in
    ``tests/oracle/extraction_xfail.toml`` without masking the others."""
    q, a, s, c = (rep[k] for k in ("quantities", "attributes", "storeys", "containment"))
    return {
        "classifications": rep["classifications"]["mismatched_products"] == 0,
        "materials": rep["materials"]["mismatched_products"] == 0,
        "psets": (
            rep["psets"]["missing_in_ifcfast"] == 0
            and rep["psets"]["extra_in_ifcfast"] == 0
            and rep["psets"]["value_mismatch"] == 0
        ),
        "quantities": (
            q["missing_in_ifcfast"] == 0
            and q["extra_in_ifcfast"] == 0
            and q["value_mismatch"] == 0
            and q["source_mismatch"] == 0
        ),
        "attributes": (
            not any(a["mismatched"].values())
            and not a["coverage_missing_in_ifcfast_by_class"]
            and not a["coverage_extra_in_ifcfast_by_class"]
        ),
        "storeys": (
            not any(s["mismatched"].values())
            and not s["missing_in_ifcfast"]
            and not s["extra_in_ifcfast"]
        ),
        "containment": not any(c["mismatched"].values()),
    }


def is_clean(rep: dict[str, Any]) -> bool:
    return all(surface_status(rep).values())


def _summary_line(rep: dict[str, Any]) -> str:
    """model | surface compared/mismatched, one line per model."""
    q, a, s, c = (rep[k] for k in ("quantities", "attributes", "storeys", "containment"))
    qm = q["missing_in_ifcfast"] + q["extra_in_ifcfast"] + q["value_mismatch"] + q["source_mismatch"]
    am = sum(a["mismatched"].values()) + sum(a["coverage_missing_in_ifcfast_by_class"].values()) \
        + sum(a["coverage_extra_in_ifcfast_by_class"].values())
    sm = sum(s["mismatched"].values()) + len(s["missing_in_ifcfast"]) + len(s["extra_in_ifcfast"])
    return (
        f"{rep['model']}: quantities {q['quantities_compared']}/{qm} "
        f"attributes {sum(a['compared'].values())}/{am} "
        f"storeys {sum(s['compared'].values())}/{sm} "
        f"containment {sum(c['compared'].values())}/{sum(c['mismatched'].values())} "
        "(compared/mismatched)"
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
    for rep in reports:
        print(_summary_line(rep))
    if a.report:
        a.report.write_text(json.dumps(reports, indent=1, default=str))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
