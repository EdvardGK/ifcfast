"""Generate the ifcfast.com "receipts" — every number the landing page shows.

Runs the shipped ifcfast against the buildingSMART Medical-Dental Clinic
sample (CC BY 4.0, five discipline models) and writes
``<out>/{parse,clash,qto,write,mcp}.json`` plus ``<out>/model/`` (one
first-floor GLB per discipline, ``instrument.json`` product index,
ATTRIBUTION.md). The contract lives in the site repo:
``ifcfast-site/docs/receipts-contract.md``.

Usage (from the parser repo, venv with ifcfast + ifcopenshell):

    python scripts/generate_receipts.py \
        --clinic .local-samples/clinic \
        --work scratch/clinic \
        --out ../ifcfast-site/public/receipts

The QTO oracle column needs ``tests.oracle.class_sweep`` to have run on
Clinic_Architectural.ifc with ``--cache-dir <work>/sweep_ark`` (it caches
the ifcopenshell volumes per guid); the script runs it if the cache is
missing. Nothing here is hand-typed: rerun after every release.
"""
from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import platform
import subprocess
import sys
import time
from collections import Counter, defaultdict
from pathlib import Path

import pandas as pd

import ifcfast

DISCIPLINES = ["Architectural", "Structural", "HVAC", "Plumbing", "Electrical"]
FLOOR_ALIASES = {"First Floor": "First Floor", "Level 1": "First Floor"}


def _machine() -> str:
    cpu = ""
    try:
        for line in open("/proc/cpuinfo"):
            if line.startswith("model name"):
                cpu = line.split(":", 1)[1].strip()
                break
    except OSError:
        pass
    return f"{platform.node()} · {cpu or platform.processor()} · {platform.system()}"


def _stamp() -> dict:
    return {
        "generated": dt.date.today().isoformat(),
        "ifcfast_version": ifcfast.__version__,
        "machine": _machine(),
        "values": "measured",
    }


def _round(x, n=3):
    return None if x is None else round(float(x), n)


# --------------------------------------------------------------------------
# parse.json
# --------------------------------------------------------------------------
def receipts_parse(clinic: Path, work: Path) -> dict:
    models = []
    tot_mb = tot_products = tot_cold = 0.0
    for d in DISCIPLINES:
        p = clinic / f"Clinic_{d}.ifc"
        t0 = time.perf_counter()
        m = ifcfast.open(p, use_cache=False, write_cache=False)
        cold = time.perf_counter() - t0
        # warm: write the cache once, then time a cached open
        ifcfast.open(p, use_cache=True, write_cache=True)
        t0 = time.perf_counter()
        ifcfast.open(p, use_cache=True, write_cache=False)
        warm = time.perf_counter() - t0
        s = m.summary()
        bdir = work / f"{d}.bundle"
        t0 = time.perf_counter()
        ifcfast.bundle(str(p), str(bdir))
        bundle_s = time.perf_counter() - t0
        size_mb = p.stat().st_size / 1e6
        models.append(
            {
                "discipline": d,
                "file": p.name,
                "size_mb": _round(size_mb, 1),
                "schema": s["schema"],
                "authoring_app": s["authoring_app"],
                "products": int(s["products"]),
                "storeys": int(s["storeys"]),
                "unit": s["length_unit"],
                "open_cold_s": _round(cold),
                "open_warm_s": _round(warm),
                "bundle_s": _round(bundle_s, 2),
                "top_types": [[k, int(v)] for k, v in list(s["top_types"].items())[:5]],
            }
        )
        tot_mb += size_mb
        tot_products += s["products"]
        tot_cold += cold
    return {
        **_stamp(),
        "models": models,
        "total_size_mb": _round(tot_mb, 1),
        "total_products": int(tot_products),
        "total_open_cold_s": _round(tot_cold, 2),
        "command": "m = ifcfast.open('Clinic_Plumbing.ifc'); m.summary()",
    }


# --------------------------------------------------------------------------
# qto.json — ifcfast mesh_qto vs the ifcopenshell oracle, per class
# --------------------------------------------------------------------------
def receipts_qto(clinic: Path, work: Path) -> dict:
    ifc = clinic / "Clinic_Architectural.ifc"
    cache_dir = work / "sweep_ark"
    sweep = cache_dir / "Clinic_Architectural_sweep.json"
    if not sweep.exists():
        cache_dir.mkdir(parents=True, exist_ok=True)
        subprocess.run(
            [sys.executable, "-m", "tests.oracle.class_sweep", str(ifc), "--cache-dir", str(cache_dir)],
            check=True,
        )
    sw = json.load(open(sweep))
    ios = sw["ios"]
    m = ifcfast.open(ifc, use_cache=False, write_cache=False)
    t0 = time.perf_counter()
    prod, _ = m.mesh_qto()
    qto_s = time.perf_counter() - t0
    prod = prod.set_index("guid")
    rows = defaultdict(lambda: {"n": 0, "fast": 0.0, "ios": 0.0, "open_shell": 0, "ref_gt_aabb": 0, "unreliable": 0})
    for guid, r in prod.iterrows():
        o = ios.get(guid)
        if o is None:
            continue
        e = rows[r["entity"]]
        e["n"] += 1
        e["fast"] += float(r["volume_m3"])
        e["ios"] += float(o["volume"])
        if r["mesh_quality"] == "open_shell":
            e["open_shell"] += 1
        if float(o["volume"]) > float(r["aabb_volume_m3"]) * 1.01:
            e["ref_gt_aabb"] += 1
        if not bool(r["volume_reliable"]):
            e["unreliable"] += 1
    classes = []
    for entity, e in sorted(rows.items(), key=lambda kv: -kv[1]["ios"]):
        classes.append(
            {
                "entity": entity,
                "n": e["n"],
                "ifcfast_m3": _round(e["fast"], 2),
                "ifcopenshell_m3": _round(e["ios"], 2),
                "ratio": _round(e["fast"] / e["ios"], 4) if e["ios"] else None,
                "open_shell": e["open_shell"],
                "reference_exceeds_aabb": e["ref_gt_aabb"],
                "flagged_unreliable": e["unreliable"],
            }
        )
    import ifcopenshell  # oracle only

    return {
        **_stamp(),
        "model": ifc.name,
        "mesh_qto_s": _round(qto_s, 2),
        "products": int(len(prod)),
        "volume_reliable_share": _round(float(prod["volume_reliable"].mean()), 3),
        "classes": classes,
        "_note": (
            "reference_exceeds_aabb counts rows where the ifcopenshell volume is larger than the "
            "element's own bounding box — physically impossible, so the reference is the wrong side "
            "there. flagged_unreliable counts rows ifcfast itself marks volume_reliable=false."
        ),
        "ifcopenshell_version": getattr(ifcopenshell, "version", "?"),
        "command": "products, surfaces = m.mesh_qto()  # volume_reliable flags the rows to route to ifcopenshell",
    }


# --------------------------------------------------------------------------
# write.json — subset / hotswap / mutate on the architectural model
# --------------------------------------------------------------------------
def _records(b: bytes) -> set[bytes]:
    return set(line for line in b.split(b"\n") if line.startswith(b"#"))


def receipts_write(clinic: Path, work: Path) -> dict:
    ifc = clinic / "Clinic_Architectural.ifc"
    original = ifc.read_bytes()
    m = ifcfast.open(ifc, use_cache=False, write_cache=False)
    first = next(s for s in m.storeys if FLOOR_ALIASES.get(s.name) == "First Floor")
    guids = [p.guid for p in m.filter(storey_guid=first.guid)]
    out_sub = work / "write" / "floor1.ifc"
    out_sub.parent.mkdir(parents=True, exist_ok=True)
    t0 = time.perf_counter()
    m.subset(guids, out_path=str(out_sub))
    sub_s = time.perf_counter() - t0
    sub_model = ifcfast.open(out_sub, use_cache=False, write_cache=False)

    # hotswap: swap a wall's own body back in (identity swap) and measure
    # how much of the file the write actually touched.
    wall = next(p for p in m.filter(entity="IfcWallStandardCase") if p.guid in set(guids))
    mesh = m.mesh(wall.guid, frame="local")
    out_hs = work / "write" / "hotswap.ifc"
    t0 = time.perf_counter()
    hs = m.hotswap(wall.guid, mesh.vertices, mesh.faces, out_path=str(out_hs))
    hs_s = time.perf_counter() - t0
    swapped = out_hs.read_bytes()
    common = 0
    for a, b in zip(original, swapped):
        if a != b:
            break
        common += 1
    changed = _records(original) ^ _records(swapped)

    out_mu = work / "write" / "mutate.ifc"
    ops = [
        {"op": "set_property", "guid": wall.guid, "pset": "Pset_WallCommon", "name": "FireRating", "value": "REI 60", "ifc_type": "IFCLABEL"},
        {"op": "rename", "guid": wall.guid, "name": "Wall — receipts demo"},
        {"op": "translate", "guid": wall.guid, "delta": [0.0, 0.0, 0.0]},
    ]
    t0 = time.perf_counter()
    mu = m.mutate(ops, out_path=str(out_mu))
    mu_s = time.perf_counter() - t0
    return {
        **_stamp(),
        "model": ifc.name,
        "subset": {
            "seed_guids": len(guids),
            "storey": first.name,
            "products_out": int(len(sub_model)),
            "bytes_in": len(original),
            "bytes_out": out_sub.stat().st_size,
            "seconds": _round(sub_s, 2),
        },
        "hotswap": {
            "guid": wall.guid,
            "class": "IfcWallStandardCase",
            "triangles": int(len(mesh.faces)),
            "seconds": _round(hs_s, 3),
            "bytes_in": len(original),
            "bytes_out": len(swapped),
            "identical_prefix_bytes": common,
            "records_changed": len(changed),
            "stats": {k: v for k, v in (hs or {}).items() if isinstance(v, (int, float, str, bool))},
        },
        "mutate": {
            "ops": len(ops),
            "seconds": _round(mu_s, 3),
            "stats": {k: v for k, v in (mu or {}).items() if isinstance(v, (int, float, str, bool))},
        },
        "command": "m.subset(guids, out_path='floor1.ifc'); m.hotswap(guid, verts, tris, out_path='out.ifc'); m.mutate(ops, out_path='edited.ifc')",
    }


# --------------------------------------------------------------------------
# mcp.json
# --------------------------------------------------------------------------
def receipts_mcp() -> dict:
    src = Path(ifcfast.__file__).with_name("mcp_server.py").read_text()
    names = []
    lines = src.splitlines()
    for i, line in enumerate(lines):
        if line.strip().startswith("@mcp.tool"):
            for j in range(i + 1, min(i + 4, len(lines))):
                if lines[j].startswith("def "):
                    names.append(lines[j][4:].split("(", 1)[0])
                    break
    groups: dict[str, list[str]] = defaultdict(list)
    for n in names:
        if n in ("open_ifc", "summary", "types", "storeys", "preview", "example_path", "system_prompt", "diff"):
            groups["model"].append(n)
        elif n in ("psets", "quantities", "materials", "classifications", "sql", "products"):
            groups["data"].append(n)
        elif n in ("children", "descendants", "ancestors", "products_in", "building_of", "storey_of", "graph"):
            groups["graph"].append(n)
        else:
            groups["other"].append(n)
    return {
        **_stamp(),
        "tools": len(names),
        "groups": dict(groups),
        "resources": ["ifcfast://agents-guide"],
        "config": {"mcpServers": {"ifcfast": {"command": "uvx", "args": ["--from", "ifcfast[mcp]", "ifcfast-mcp"]}}},
        "command": "uvx --from 'ifcfast[mcp]' ifcfast-mcp",
    }


# --------------------------------------------------------------------------
# clash.json — federate the five bundles, clash at tolerance 0
# --------------------------------------------------------------------------
def receipts_clash(work: Path, g55_baselines: Path | None) -> tuple[dict, pd.DataFrame | None]:
    bundles = [str(work / f"{d}.bundle") for d in DISCIPLINES]
    fed_dir = work / "federated.bundle"
    out: dict = {**_stamp(), "tolerance_m": 0.0}
    oracle = None
    if g55_baselines and g55_baselines.exists():
        rounds = []
        for f in sorted(g55_baselines.glob("clash_*.json")):
            b = json.load(open(f))
            rounds.append({"round": f.stem.replace("clash_", ""), "matched": b["n_matched"], "truth_pairs": b["n_truth_pairs"], "topics": [b["n_topics_matched"], b["n_topics"]]})
        oracle = {
            "project": "a live hospital project (client data, not shown)",
            "truth": "Solibri TMK BCF ground truth, version-matched by STEP header",
            "rounds": rounds,
        }
    out["oracle"] = oracle
    out["command"] = "fed = ifcfast.federate([...5 bundles...], 'clinic.fed'); df = ifcfast.clash(fed)"
    try:
        t0 = time.perf_counter()
        fed = ifcfast.federate(bundles, str(fed_dir), on_collision="warn")
        fed_s = time.perf_counter() - t0
    except ValueError as exc:
        out["pending"] = f"federate refused: {exc}"
        return out, None
    t0 = time.perf_counter()
    df = ifcfast.clash(str(fed_dir), tolerance_m=0.0)
    clash_s = time.perf_counter() - t0
    # source_model is the bundle dir stem ("HVAC.bundle") — present the discipline.
    df = df.assign(
        source_model_a=df.source_model_a.str.replace(".bundle", "", regex=False),
        source_model_b=df.source_model_b.str.replace(".bundle", "", regex=False),
    )
    cross = df[df.source_model_a != df.source_model_b]
    by_model = Counter()
    for a, b in zip(cross.source_model_a, cross.source_model_b):
        by_model[tuple(sorted((a, b)))] += 1
    by_class = Counter()
    for a, b in zip(cross.class_a, cross.class_b):
        by_class[tuple(sorted((a, b)))] += 1
    unit_scales = (fed or {}).get("unit_scales") if isinstance(fed, dict) else None
    if isinstance(unit_scales, dict):
        unit_scales = {k.replace(".bundle", ""): v for k, v in unit_scales.items()}
    out.update(
        {
            "federate_s": _round(fed_s, 2),
            "clash_s": _round(clash_s, 2),
            "unit_scales": unit_scales,
            "unit_note": "3 models authored in m, 2 in mm — federate() rescales to the finest unit (GH #169)",
            "pairs_total": int(len(df)),
            "pairs_cross_model": int(len(cross)),
            "by_category": {k: int(v) for k, v in cross.category.value_counts().items()},
            "by_kind": {k: int(v) for k, v in cross.kind.value_counts().items()} if "kind" in cross else None,
            "by_model_pair": [[a, b, int(n)] for (a, b), n in by_model.most_common(10)],
            "by_class_pair": [[a, b, int(n)] for (a, b), n in by_class.most_common(12)],
            "narrow_phase_residuals": int(df.attrs.get("narrow_phase_residuals", 0)),
            "top_rows": [
                {
                    "guid_a": r.guid_a, "class_a": r.class_a, "model_a": r.source_model_a,
                    "guid_b": r.guid_b, "class_b": r.class_b, "model_b": r.source_model_b,
                    "category": r.category, "kind": getattr(r, "kind", None),
                }
                for r in cross[cross.category == "clash"].head(12).itertuples()
            ],
        }
    )
    return out, df


# --------------------------------------------------------------------------
# model/ — first-floor GLB per discipline + instrument.json
# --------------------------------------------------------------------------
def receipts_model(clinic: Path, work: Path, out: Path, clash_df: pd.DataFrame | None) -> dict:
    mdir = out / "model"
    mdir.mkdir(parents=True, exist_ok=True)
    fdir = work / "floor1"
    fdir.mkdir(parents=True, exist_ok=True)
    models, storeys, classes, products = [], [], [], {}
    mat_counter: Counter = Counter()
    floor_guids: set[str] = set()
    for d in DISCIPLINES:
        m = ifcfast.open(clinic / f"Clinic_{d}.ifc", use_cache=False, write_cache=False)
        first = next(s for s in m.storeys if FLOOR_ALIASES.get(s.name) == "First Floor")
        guids = [p.guid for p in m.filter(storey_guid=first.guid)]
        sub_ifc = fdir / f"{d}_floor1.ifc"
        m.subset(guids, out_path=str(sub_ifc))
        ms = ifcfast.open(sub_ifc, use_cache=False, write_cache=False)
        glb = mdir / f"clinic-floor1-{d.lower()}.glb"
        # openings only matter for the architectural walls; MEP/structure
        # without the cut keeps IfcMappedItem instancing (much smaller).
        stats = ms.to_gltf(str(glb), cut_openings=(d == "Architectural"))
        prod, _ = ms.mesh_qto()
        prod = prod.set_index("guid")
        mats = ms.materials
        mat_by_guid = {}
        if len(mats):
            for g, name in zip(mats.guid, mats.material_name):
                if name and g not in mat_by_guid:
                    mat_by_guid[g] = str(name)
        per_class: dict[str, dict] = defaultdict(lambda: {"n": 0, "volume_m3": 0.0})
        for p in ms.products:
            if p.guid not in prod.index:
                continue
            r = prod.loc[p.guid]
            mat = mat_by_guid.get(p.guid)
            if mat:
                mat_counter[mat] += 1
            products[p.guid] = {
                "entity": p.entity,
                "model": d,
                "storey": "First Floor",
                "volume_m3": _round(r["volume_m3"], 4),
                "volume_reliable": bool(r["volume_reliable"]),
                "material": mat,
                "clash_partners": [],
            }
            per_class[p.entity]["n"] += 1
            per_class[p.entity]["volume_m3"] += float(r["volume_m3"])
            floor_guids.add(p.guid)
        for entity, v in per_class.items():
            classes.append({"entity": entity, "model": d, "n": v["n"], "volume_m3": _round(v["volume_m3"], 2)})
        if d == "Architectural":
            for s in m.storeys:
                storeys.append({"guid": s.guid, "name": s.name, "elevation_m": _round(s.elevation, 2), "products": len(list(m.filter(storey_guid=s.guid)))})
        models.append(
            {
                "name": d,
                "glb": f"/receipts/model/{glb.name}",
                "products": len(products) and sum(1 for v in products.values() if v["model"] == d),
                "bytes": glb.stat().st_size,
                "triangles": int(stats.get("triangles", 0)),
                "instancing": bool(stats.get("instancing", False)),
            }
        )
    if clash_df is not None:
        cross = clash_df[clash_df.source_model_a != clash_df.source_model_b]
        for r in cross.itertuples():
            if r.guid_a in products and r.guid_b in products:
                products[r.guid_a]["clash_partners"].append(r.guid_b)
                products[r.guid_b]["clash_partners"].append(r.guid_a)
    (mdir / "ATTRIBUTION.md").write_text(
        "# Medical-Dental Clinic\n\nSource: buildingSMART community sample test files "
        "(https://github.com/buildingsmart-community/Community-Sample-Test-Files), "
        "Medical-Dental Clinic, five discipline models.\n\nLicense: CC BY 4.0.\n\n"
        "All GLB and JSON files here were generated from those IFCs with ifcfast "
        f"{ifcfast.__version__} (`scripts/generate_receipts.py`).\n"
    )
    inst = {
        **_stamp(),
        "scope": "First Floor, all five disciplines",
        "models": models,
        "storeys": storeys,
        "classes": sorted(classes, key=lambda c: -c["n"]),
        "materials": [{"name": k, "n": v} for k, v in mat_counter.most_common(24)],
        "products": products,
    }
    return inst


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--clinic", type=Path, required=True)
    ap.add_argument("--work", type=Path, required=True)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--g55-baselines", type=Path, default=Path("scratch/g55/baselines"))
    ap.add_argument("--only", nargs="*", default=None, help="subset of: parse qto write mcp clash model")
    a = ap.parse_args()
    a.work.mkdir(parents=True, exist_ok=True)
    a.out.mkdir(parents=True, exist_ok=True)
    only = set(a.only or ["parse", "qto", "write", "mcp", "clash", "model"])

    def dump(name: str, obj: dict) -> None:
        (a.out / f"{name}.json").write_text(json.dumps(obj, indent=1, default=str))
        print(f"wrote {a.out / f'{name}.json'}")

    if "parse" in only:
        dump("parse", receipts_parse(a.clinic, a.work))
    if "qto" in only:
        dump("qto", receipts_qto(a.clinic, a.work))
    if "write" in only:
        dump("write", receipts_write(a.clinic, a.work))
    if "mcp" in only:
        dump("mcp", receipts_mcp())
    clash_df = None
    if "clash" in only:
        clash, clash_df = receipts_clash(a.work, a.g55_baselines)
        dump("clash", clash)
    if "model" in only:
        inst = receipts_model(a.clinic, a.work, a.out, clash_df)
        (a.out / "model" / "instrument.json").write_text(json.dumps(inst, indent=1, default=str))
        print(f"wrote {a.out / 'model' / 'instrument.json'}  ({len(inst['products'])} products)")


if __name__ == "__main__":
    main()
