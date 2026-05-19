#!/usr/bin/env python3
"""
Generate comprehensive ifcfast sidecar JSON for an IFC file.

The goal — trust first. The sidecars must include every measure
ifcfast can compute, with nothing silently dropped. Specifically:

  - All product rows (every IfcProduct subclass)
  - All psets (long format)
  - All quantities (Qto_* values, long format)
  - All material assignments + layer thicknesses (long format)
  - All classifications (long format)
  - All per-product mesh stats (drift table — surface area, mesh
    volume, AABB, placement drift)
  - All spaces with their step_id + guid (until Phase 2 promotes
    them to full Product rows in the Rust core)
  - All spatial-graph edges (contained_in, aggregates,
    storey_building)
  - Full type summary
  - Material layer set definitions

Output is a single bundle JSON the demo consumes, plus the legacy
sidecars (summary / qto / graph / types) regenerated to match the
shape the current workbench expects, with the silently-dropped data
restored.

Usage:
    python3 scripts/generate_sample_sidecars.py \\
        --ifc /path/to/duplex.ifc \\
        --out /path/to/output_dir \\
        --prefix duplex
"""

from __future__ import annotations

import argparse
import json
import math
import sys
from pathlib import Path

# Ensure we use the in-tree ifcfast (not whatever's pip-installed)
ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "python"))

import ifcfast  # noqa: E402
from ifcfast import _core  # noqa: E402


def _json_default(o):
    """Coerce numpy / pandas scalars + NaN/Inf to JSON-safe."""
    try:
        import numpy as np
        if isinstance(o, np.generic):
            v = o.item()
            if isinstance(v, float) and not math.isfinite(v):
                return None
            return v
    except ImportError:
        pass
    if isinstance(o, float) and not math.isfinite(o):
        return None
    raise TypeError(f"Object of type {type(o).__name__} is not JSON serializable")


def _df_to_records(df):
    """DataFrame → list of dicts with NaN → None."""
    if df is None or len(df) == 0:
        return []
    return json.loads(df.to_json(orient="records"))


def _build_per_product(model):
    """Join product rows with drift / mesh stats by guid.

    Each row gets `mesh_stats` (or null) so callers see at a glance
    whether ifcfast produced geometry for that product, and if so,
    every measure the mesh layer computed.
    """
    products = _df_to_records(model.products_df)
    drift_by_guid: dict[str, dict] = {}
    try:
        for rec in _df_to_records(model.drift):
            guid = rec.pop("guid", None)
            if guid is None:
                continue
            # entity already in product row; keep stats subset
            drift_by_guid[guid] = rec
    except Exception as exc:
        print(f"[warn] drift extraction failed: {exc}", file=sys.stderr)

    for p in products:
        p["mesh_stats"] = drift_by_guid.get(p.get("guid"))
    return products


def _raw_index(ifc_path):
    """Cached raw dict from _core.index_ifc — also used for spaces +
    buildings + sites + projects which the Python Model wrapper
    doesn't expose as accessors today."""
    try:
        return _core.index_ifc(str(ifc_path))
    except Exception as exc:
        print(f"[warn] raw index failed: {exc}", file=sys.stderr)
        return {}


def _spaces_from_raw(raw):
    """Until Phase 2 promotes spaces to full Products in the Rust core,
    this is the only place spaces are accessible."""
    sp = raw.get("spaces") or {}
    step_ids = sp.get("step_id", [])
    guids = sp.get("guid", [])
    return [
        {"step_id": int(s), "guid": g, "entity": "IfcSpace"}
        for s, g in zip(step_ids, guids)
    ]


def _container_collections(raw):
    """Buildings / sites / projects as lists of {guid, name?} dicts."""
    out: dict[str, list] = {"buildings": [], "sites": [], "projects": []}
    for key in out:
        coll = raw.get(key) or {}
        guids = coll.get("guid", [])
        names = coll.get("name", [None] * len(guids))
        for g, n in zip(guids, names):
            out[key].append({"guid": g, "name": n})
    return out


def _qto_aggregates(per_product):
    """Per-entity-class aggregates derived from products + mesh stats.

    Replaces the hand-rolled duplex.qto.json. Now sourced from real
    per-product data, so an entity class with no mesh stats reports
    null area + null volume (instead of pretending the mesh-layer
    couldn't handle anything for that class — which was the
    silent-drop pattern).
    """
    rows_by_entity: dict[str, dict] = {}
    for p in per_product:
        ent = p.get("entity")
        if not ent:
            continue
        row = rows_by_entity.setdefault(ent, {
            "entity": ent,
            "count": 0,
            "storeys": set(),
            "area_m2": 0.0,
            "volume_m3": 0.0,
            "triangles": 0,
            "products_with_mesh": 0,
            "products_without_mesh": 0,
        })
        row["count"] += 1
        if p.get("storey_name"):
            row["storeys"].add(p["storey_name"])
        ms = p.get("mesh_stats")
        if ms is None:
            row["products_without_mesh"] += 1
            continue
        row["products_with_mesh"] += 1
        sa = ms.get("surface_area")
        vol = ms.get("volume_abs")
        if isinstance(sa, (int, float)) and math.isfinite(sa):
            row["area_m2"] += sa
        if isinstance(vol, (int, float)) and math.isfinite(vol):
            row["volume_m3"] += vol
        tc = ms.get("triangle_count")
        if isinstance(tc, (int, float)) and math.isfinite(tc):
            row["triangles"] += int(tc)

    out = []
    for r in rows_by_entity.values():
        r["storeys"] = sorted(r["storeys"])
        # null when no mesh stats contributed — explicit "we didn't
        # compute this", not a misleading 0.
        if r["products_with_mesh"] == 0:
            r["area_m2"] = None
            r["volume_m3"] = None
            r["source"] = "none"
        else:
            r["source"] = "mesh"
        out.append(r)
    out.sort(key=lambda r: (-r["count"], r["entity"]))
    return out


def _build_graph(model, spaces, containers, mesh_stats_by_guid, pset_attrs_by_guid, rollup_by_guid):
    """Spatial graph shape the workbench consumes.

    Matches the existing duplex.graph.json field names, plus spaces
    are now represented (Phase 2 will let products include them
    directly; for now they're a sibling collection).

    Per-product columns enriched: typed (bool), type_name,
    type_source, materials (list), layer_set, predefined_type,
    object_type.
    """
    products = []
    psets_by_guid: dict[str, list] = {}
    mats_by_guid: dict[str, list] = {}
    layer_set_by_guid: dict[str, str | None] = {}
    typed_by_guid: dict[str, bool] = {}
    type_name_by_guid: dict[str, str | None] = {}
    type_source_by_guid: dict[str, str] = {}

    # Materials → per-product list + layer_set link
    layer_set_defs: dict[str, dict] = {}
    for m in _df_to_records(model.materials):
        guid = m.get("guid")
        if not guid:
            continue
        role = m.get("role")
        name = m.get("material_name") or ""
        if role == "layer":
            mats_by_guid.setdefault(guid, [])
            if name and name not in mats_by_guid[guid]:
                mats_by_guid[guid].append(name)
            # Layer-set name — for now we only have layer thicknesses;
            # group by guid as a synthetic set per product if needed
        elif role == "single" and name:
            mats_by_guid.setdefault(guid, [])
            if name not in mats_by_guid[guid]:
                mats_by_guid[guid].append(name)
        elif role == "set" and name:
            layer_set_by_guid[guid] = name

    # Layer-set definitions — group materials by set name
    for m in _df_to_records(model.materials):
        if m.get("role") != "layer":
            continue
        set_name = m.get("set_name") if "set_name" in m else None
        if not set_name:
            continue
        ls = layer_set_defs.setdefault(set_name, {
            "layers": [],
            "total_thickness_mm": 0.0,
        })
        ly = {
            "material": m.get("material_name") or "",
            "thickness_mm": float(m.get("layer_thickness_mm") or 0.0) * 1000.0
            if (m.get("layer_thickness_mm") or 0.0) < 1.0
            else float(m.get("layer_thickness_mm") or 0.0),
        }
        ls["layers"].append(ly)
        ls["total_thickness_mm"] += ly["thickness_mm"]

    # Typed-ness — native fields on products_df. ifcfast's Rust
    # indexer extracts IfcRelDefinesByType directly, so `type_source`
    # is one of {"ifctype", "objecttype", "none"} and `type_name` is
    # the relating type name (or the ObjectType fallback).
    for p in _df_to_records(model.products_df):
        guid = p["guid"]
        type_source = p.get("type_source") or "none"
        typed_by_guid[guid] = type_source == "ifctype"
        type_name_by_guid[guid] = p.get("type_name") or p.get("object_type")
        type_source_by_guid[guid] = type_source

    for p in _df_to_records(model.products_df):
        guid = p["guid"]
        ms = mesh_stats_by_guid.get(guid)
        roll = rollup_by_guid.get(guid)
        attrs = pset_attrs_by_guid.get(guid) or {}
        # Direct mesh stats — null when ifcfast-mesh produced no
        # geometry for this product itself.
        m3_direct = ms.get("volume_abs") if ms else None
        m2_direct = ms.get("surface_area") if ms else None
        lm_direct = ms.get("max_extent") if ms else None
        # Effective m3/m2/lm shown in the UI: direct when present,
        # rollup (from aggregated descendants) otherwise. `m_source`
        # tells the consumer which one this row is using so
        # double-counting / mis-attribution is impossible. The raw
        # direct + rollup numbers stay available below for callers
        # that want to roll up totals themselves.
        if m3_direct is not None or m2_direct is not None or lm_direct is not None:
            m_source = "direct"
            m3 = m3_direct; m2 = m2_direct; lm = lm_direct
        elif roll:
            m_source = "aggregate-rollup"
            m3 = roll.get("m3"); m2 = roll.get("m2"); lm = roll.get("lm")
        else:
            m_source = "none"
            m3 = None; m2 = None; lm = None
        products.append({
            "guid": guid,
            "entity": p["entity"],
            "name": p.get("name"),
            "predefined_type": p.get("predefined_type"),
            "object_type": p.get("object_type"),
            "tag": p.get("tag"),
            "storey_guid": p.get("storey_guid"),
            "parent_guid": p.get("parent_guid"),
            "typed": typed_by_guid.get(guid, False),
            "type_name": type_name_by_guid.get(guid),
            "type_source": type_source_by_guid.get(guid, "none"),
            "materials": mats_by_guid.get(guid, []),
            "layer_set": layer_set_by_guid.get(guid),
            "m3": m3,
            "m2": m2,
            "lm": lm,
            "m_source": m_source,
            "m3_direct": m3_direct,
            "m2_direct": m2_direct,
            "lm_direct": lm_direct,
            "is_external": attrs.get("IsExternal"),
            "load_bearing": attrs.get("LoadBearing"),
            "fire_rating": attrs.get("FireRating"),
        })

    # Storeys / buildings / sites / projects
    storeys = [
        {"guid": s.guid, "name": s.name, "elevation": s.elevation,
         "building_guid": getattr(s, "building_guid", None)}
        for s in model.storeys
    ]

    contained_in = [
        {"product_guid": r["product_guid"], "storey_guid": r["storey_guid"]}
        for r in _df_to_records(model.contained_in)
    ]
    aggregates = [
        {"child_guid": r["child_guid"], "parent_guid": r["parent_guid"],
         "parent_kind": r["parent_kind"]}
        for r in _df_to_records(model.aggregates)
    ]
    storey_building = [
        {"storey_guid": r["storey_guid"], "building_guid": r["building_guid"]}
        for r in _df_to_records(model.storey_building)
    ]

    voids = [
        {"opening_guid": r["opening_guid"], "host_guid": r["host_guid"]}
        for r in _df_to_records(model.voids)
    ]

    return {
        "project_name": getattr(model.header, "project_name", None),
        "schema": getattr(model.header, "schema", None),
        "products": products,
        "storeys": storeys,
        "spaces": spaces,
        "buildings": containers.get("buildings", []),
        "sites": containers.get("sites", []),
        "projects": containers.get("projects", []),
        "contained_in": contained_in,
        "aggregates": aggregates,
        "storey_building": storey_building,
        "voids": voids,
        "material_layer_sets": layer_set_defs,
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--ifc", required=True, type=Path)
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--prefix", default="model")
    args = ap.parse_args()

    args.out.mkdir(parents=True, exist_ok=True)

    print(f"Opening {args.ifc} …")
    model = ifcfast.open(str(args.ifc))

    print("Building per-product table …")
    raw = _raw_index(args.ifc)
    spaces = _spaces_from_raw(raw)
    containers = _container_collections(raw)
    per_product = _build_per_product(model)
    typed_count = sum(
        1 for p in per_product if (p.get("type_source") or "none") == "ifctype"
    )
    voids_count = len(model.voids)

    print(f"  {len(per_product)} products  ·  {len(spaces)} spaces (sibling)")
    print(f"  typing: {typed_count} ifctype · {len(per_product) - typed_count} other "
          f"(native ifcfast IfcRelDefinesByType extraction)")
    print(f"  voids: {voids_count} IfcRelVoidsElement edges (native ifcfast)")

    bundle = {
        "summary": model.summary(),
        "products": per_product,
        "psets": _df_to_records(model.psets),
        "quantities": _df_to_records(model.quantities),
        "materials": _df_to_records(model.materials),
        "classifications": _df_to_records(model.classifications),
        "spaces": spaces,
        "graph": {
            "contained_in": _df_to_records(model.contained_in),
            "aggregates": _df_to_records(model.aggregates),
            "storey_building": _df_to_records(model.storey_building),
        },
        "type_summary": model.type_summary(),
        "provenance": {
            "ifcfast_version": getattr(ifcfast, "__version__", "unknown"),
            "typedness_source": "ifcfast",
        },
    }

    # ---- bundle (the source of truth) ------------------------------
    bundle_path = args.out / f"{args.prefix}.bundle.json"
    bundle_path.write_text(
        json.dumps(bundle, default=_json_default, indent=2)
    )
    print(f"wrote {bundle_path}  ({bundle_path.stat().st_size / 1024:.1f} KB)")

    # ---- legacy sidecars (kept for current workbench compatibility) ----
    (args.out / f"{args.prefix}.summary.json").write_text(
        json.dumps(model.summary(), default=_json_default, indent=2)
    )
    (args.out / f"{args.prefix}.types.json").write_text(
        json.dumps(model.type_summary(), default=_json_default, indent=2)
    )
    (args.out / f"{args.prefix}.qto.json").write_text(
        json.dumps({
            "schema": model.header.schema,
            "products": len(per_product),
            "rows": _qto_aggregates(per_product),
        }, default=_json_default, indent=2)
    )
    # Per-product mesh stats keyed by guid — joined into graph.json
    # so qto-panel can show m³/m²/lm next to type/material rows.
    mesh_stats_by_guid: dict[str, dict] = {}
    for rec in _df_to_records(model.drift):
        g = rec.get("guid")
        if g:
            mesh_stats_by_guid[g] = rec

    # Aggregate-rollup pass. Some IFC entities (IfcRoof, IfcStair,
    # IfcCurtainWall, …) carry no body of their own — they're
    # semantic wrappers around children that hold the actual mesh.
    # Without rollup, ifcfast reports null QTO for the wrapper even
    # though the user can clearly see geometry in the viewer
    # (because the *children* are rendered). That's a trust-killer:
    # "this renders, but you say there's no quantity" → exactly the
    # silent-attribution-gap pattern we explicitly avoid.
    #
    # Rollup walks aggregates child→parent and accumulates m3/m2 of
    # every descendant. Stored separately from `direct` mesh so
    # class-level totals (sum across products) can still pick the
    # direct value to avoid double-counting (slab's volume already
    # counts under IfcSlab; we don't want to also count it under
    # IfcRoof when totalling the whole model).
    parent_of: dict[str, str] = {}
    children_of: dict[str, list[str]] = {}
    for a in _df_to_records(model.aggregates):
        child = a.get("child_guid"); parent = a.get("parent_guid")
        if not child or not parent: continue
        parent_of[child] = parent
        children_of.setdefault(parent, []).append(child)

    def descendants(g: str) -> list[str]:
        out: list[str] = []
        stack = list(children_of.get(g, []))
        while stack:
            c = stack.pop()
            out.append(c)
            stack.extend(children_of.get(c, []))
        return out

    rollup_by_guid: dict[str, dict] = {}
    for g in {*mesh_stats_by_guid.keys(), *parent_of.values(), *children_of.keys()}:
        descs = descendants(g)
        if not descs:
            continue
        m3 = 0.0; m2 = 0.0; lm = 0.0
        m3c = 0; m2c = 0; lmc = 0
        for d in descs:
            ms = mesh_stats_by_guid.get(d)
            if not ms: continue
            v = ms.get("volume_abs"); a = ms.get("surface_area"); l = ms.get("max_extent")
            if isinstance(v, (int, float)) and v == v: m3 += v; m3c += 1
            if isinstance(a, (int, float)) and a == a: m2 += a; m2c += 1
            if isinstance(l, (int, float)) and l == l: lm += l; lmc += 1
        if m3c or m2c or lmc:
            rollup_by_guid[g] = {
                "m3": m3 if m3c else None,
                "m2": m2 if m2c else None,
                "lm": lm if lmc else None,
                "descendant_count": len(descs),
                "meshed_descendant_count": max(m3c, m2c, lmc),
            }

    # Standard pset properties we surface as first-class fields per
    # product: IsExternal (bool), LoadBearing (bool), FireRating
    # (string label). These live in pset_*_Common (Wall / Slab /
    # Door / Window / Covering / etc.). We scan every pset row for
    # the guid and pluck the first occurrence — values are
    # consistent across the *_Common variants in well-authored IFC.
    pset_attrs_by_guid: dict[str, dict] = {}
    PSET_PICK = {"IsExternal", "LoadBearing", "FireRating"}
    for rec in _df_to_records(model.psets):
        g = rec.get("guid")
        prop = rec.get("prop_name")
        if not g or prop not in PSET_PICK:
            continue
        bucket = pset_attrs_by_guid.setdefault(g, {})
        if prop not in bucket:
            v = rec.get("value")
            if prop in ("IsExternal", "LoadBearing"):
                if isinstance(v, str):
                    v = v.strip().lower() in ("true", "t", "1", ".t.")
            bucket[prop] = v

    (args.out / f"{args.prefix}.graph.json").write_text(
        json.dumps(_build_graph(model, spaces, containers, mesh_stats_by_guid, pset_attrs_by_guid, rollup_by_guid),
                   default=_json_default, indent=2)
    )

    print(f"\nLegacy sidecars regenerated alongside bundle.")
    print(f"Done — outputs in {args.out}")


if __name__ == "__main__":
    main()
