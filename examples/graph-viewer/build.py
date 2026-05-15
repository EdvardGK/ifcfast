"""Emit a single-file HTML graph viewer for an IFC file.

What you get
------------
A standalone graph_view.html with three panes:
  - LEFT  : type browser grouped by IFC class (instance-count weighted)
  - MID   : force-directed decomposition graph
            (parent_guid + storey_guid + storey.building_guid)
  - RIGHT : 3D preview (top, optional) + info pane (bottom)

Click a node or type to populate the info pane. With the mesh emitter
available, the 3D preview also isolates and frames the selected instance.

Required
--------
  pip install ifcfast

Optional
--------
  pip install ifcopenshell    enables the spatial crown (project/site/
                              building names), IfcRelDefinesByType-backed
                              type assignment, and crown edges. Without it
                              types fall back to entity+name clustering.

  ifcfast-mesh binary         enables the 3D preview. Build from source in
                              the upstream EdvardGK/ifc-workbench repo:
                                cargo build --release --bin ifcfast-mesh \\
                                  --no-default-features --features mesh
                              Then point --mesh-bin at the resulting exe.

Usage
-----
  python build.py path/to/model.ifc
  python build.py model.ifc --out my-graph.html
  python build.py model.ifc --no-mesh
  python build.py model.ifc --mesh-bin /path/to/ifcfast-mesh
"""
from __future__ import annotations

import argparse
import base64
import json
import math
import re
import shutil
import subprocess
import sys
import tempfile
from collections import defaultdict
from pathlib import Path

import ifcfast


try:
    import ifcopenshell
    HAVE_IOPS = True
except ImportError:
    ifcopenshell = None
    HAVE_IOPS = False


INSTANCE_SUFFIX = re.compile(r":\d+$")


def _v(x):
    if x is None:
        return None
    if isinstance(x, float) and math.isnan(x):
        return None
    return x


def build_glb(ifc_path: Path, mesh_bin: Path | None) -> bytes | None:
    """Run ifcfast-mesh to produce a GLB. Returns None if unavailable."""
    if mesh_bin is None:
        return None
    if not mesh_bin.exists():
        print(f"  [skip mesh] {mesh_bin} not found", file=sys.stderr)
        return None
    with tempfile.TemporaryDirectory() as td:
        out = Path(td) / "scene.glb"
        try:
            subprocess.run(
                [str(mesh_bin), str(ifc_path), str(out)],
                check=True, capture_output=True, text=True,
            )
        except subprocess.CalledProcessError as e:
            print(f"  [skip mesh] ifcfast-mesh failed: {e.stderr.strip()}", file=sys.stderr)
            return None
        return out.read_bytes()


def collect(ifc_path: Path):
    m = ifcfast.open(str(ifc_path))
    f = ifcopenshell.open(str(ifc_path)) if HAVE_IOPS else None

    extra = {}
    if f is not None:
        for et in ("IfcBuilding", "IfcSite", "IfcProject", "IfcSpace"):
            for e in f.by_type(et):
                extra[e.GlobalId] = (et, e.Name or "")

    psets = defaultdict(list)
    for r in m.psets.itertuples():
        psets[r.guid].append((_v(r.pset_name), _v(r.prop_name), _v(r.value)))
    qtos = defaultdict(list)
    for r in m.quantities.itertuples():
        qtos[r.guid].append((_v(r.qto_name), _v(r.quantity_name), _v(r.value)))
    mats = defaultdict(set)
    for r in m.materials.itertuples():
        n = _v(r.material_name)
        if n:
            mats[r.guid].add(n)
    cls = defaultdict(list)
    for r in m.classifications.itertuples():
        cls[r.guid].append((_v(r.system_name), _v(r.identification)))

    guid_to_type_obj = {}
    if f is not None:
        for rel in f.by_type("IfcRelDefinesByType"):
            rt = rel.RelatingType
            if rt is None:
                continue
            type_info = (rt.GlobalId, rt.is_a(), rt.Name or "")
            for child in rel.RelatedObjects or ():
                guid_to_type_obj[child.GlobalId] = type_info

    type_of = {}
    type_meta = {}
    for p in m.filter():
        if p.guid in guid_to_type_obj:
            tg, te, tn = guid_to_type_obj[p.guid]
            key = f"obj:{tg}"
            if key not in type_meta:
                type_meta[key] = dict(
                    id=key, kind="ifc_type",
                    type_guid=tg, type_entity=te, type_name=tn,
                    instance_entity=p.entity, members=[],
                )
        else:
            stripped = INSTANCE_SUFFIX.sub("", p.name or "") if p.name else ""
            key = f"name:{p.entity}|{stripped}"
            if key not in type_meta:
                type_meta[key] = dict(
                    id=key, kind="synthetic",
                    type_guid=None, type_entity=None,
                    type_name=stripped or f"(no name) {p.entity}",
                    instance_entity=p.entity, members=[],
                )
        type_of[p.guid] = key
        type_meta[key]["members"].append(p.guid)

    for tm in type_meta.values():
        members = tm["members"]
        union_mats = set()
        for g in members:
            union_mats |= mats.get(g, set())
        tm["materials"] = sorted(union_mats)

        per_inst = []
        all_keys = set()
        for g in members:
            d = {}
            for ps, pr, val in psets.get(g, []):
                d[(ps, pr)] = val
                all_keys.add((ps, pr))
            per_inst.append(d)

        common, varying = [], []
        for k in all_keys:
            vals = [d.get(k) for d in per_inst]
            if all(k in d for d in per_inst):
                first = vals[0]
                if all(v == first for v in vals):
                    common.append((k[0], k[1], first))
                else:
                    varying.append((k[0], k[1], len(set(map(repr, vals)))))
            else:
                varying.append((k[0], k[1], -len([v for v in vals if v is not None])))
        tm["common_psets"] = sorted(common)
        tm["varying_psets"] = sorted(varying, key=lambda x: (x[0] or "", x[1] or ""))
        tm["instance_count"] = len(members)

    nodes = {}
    for p in m.filter():
        nodes[p.guid] = dict(
            id=p.guid, entity=p.entity, name=p.name or "",
            predefined_type=p.predefined_type, object_type=p.object_type,
            tag=p.tag, mode=p.mode, step_id=p.step_id,
            parent_guid=p.parent_guid, storey_guid=p.storey_guid,
            storey_name=p.storey_name, type_id=type_of.get(p.guid),
            psets=psets.get(p.guid, []), qtos=qtos.get(p.guid, []),
            mats=sorted(mats.get(p.guid, set())), cls=cls.get(p.guid, []),
        )
    for s in m.storeys:
        nodes[s.guid] = dict(
            id=s.guid, entity="IfcBuildingStorey",
            name=s.name or "", elevation=s.elevation,
            parent_guid=s.building_guid,
        )
    for guid, (et, nm) in extra.items():
        nodes.setdefault(guid, dict(id=guid, entity=et, name=nm))

    links = []
    for p in m.filter():
        if p.parent_guid:
            links.append({"source": p.guid, "target": p.parent_guid, "kind": "agg"})
        elif p.storey_guid:
            links.append({"source": p.guid, "target": p.storey_guid, "kind": "contain"})
    for s in m.storeys:
        if s.building_guid:
            links.append({"source": s.guid, "target": s.building_guid, "kind": "agg"})

    if f is not None:
        existing = {(l["source"], l["target"]) for l in links}
        for rel in f.by_type("IfcRelAggregates"):
            parent = rel.RelatingObject.GlobalId
            if parent not in nodes:
                continue
            for child in rel.RelatedObjects or ():
                cg = child.GlobalId
                if (cg in nodes and (cg, parent) not in existing
                        and child.is_a() in ("IfcBuilding", "IfcSite")):
                    links.append({"source": cg, "target": parent, "kind": "agg"})
                    existing.add((cg, parent))
                    nodes[cg].setdefault("parent_guid", parent)

    types = [dict(
        id=tm["id"], kind=tm["kind"],
        type_name=tm["type_name"], type_entity=tm["type_entity"],
        instance_entity=tm["instance_entity"], type_guid=tm["type_guid"],
        instance_count=tm["instance_count"],
        materials=tm["materials"],
        common_psets=tm["common_psets"], varying_psets=tm["varying_psets"],
    ) for tm in type_meta.values()]

    return list(nodes.values()), links, types, m


HTML_TEMPLATE = (Path(__file__).parent / "template.html").read_text(encoding="utf-8")


def emit_html(ifc_path: Path, out: Path, mesh_bin: Path | None):
    nodes, links, types, m = collect(ifc_path)
    print(f"  {len(nodes):,} nodes, {len(links):,} links, {len(types):,} types")
    if HAVE_IOPS:
        real = sum(1 for t in types if t["kind"] == "ifc_type")
        print(f"    types: {real} from IfcRelDefinesByType, {len(types)-real} synthetic")
    else:
        print(f"    types: all synthetic (install ifcopenshell for IfcRelDefinesByType)")

    glb_bytes = build_glb(ifc_path, mesh_bin)
    glb_b64 = base64.b64encode(glb_bytes).decode("ascii") if glb_bytes else ""
    if glb_bytes:
        print(f"  glb: {len(glb_bytes)/1024:.1f} KB inlined")

    size_mb = ifc_path.stat().st_size / 1_048_576
    html = (HTML_TEMPLATE
        .replace("__FILENAME__", ifc_path.name)
        .replace("__SIZE__", f"{size_mb:.2f}")
        .replace("__NPROD__", f"{len(m):,}")
        .replace("__NSTOREY__", f"{len(m.storeys):,}")
        .replace("__NTYPES__", f"{len(types):,}")
        .replace("__NNODES__", f"{len(nodes):,}")
        .replace("__NLINKS__", f"{len(links):,}")
        .replace("__DATA__", json.dumps({"nodes": nodes, "links": links, "types": types}))
        .replace("__GLB_B64__", glb_b64)
    )
    out.write_text(html, encoding="utf-8")
    print(f"  wrote {out} ({out.stat().st_size/1024:.1f} KB)")


def main():
    repo_root = Path(__file__).resolve().parents[2]
    default_ifc = repo_root / "tests" / "fixtures" / "minimal.ifc"

    ap = argparse.ArgumentParser(description="Build a standalone HTML graph viewer for an IFC.")
    ap.add_argument("ifc", nargs="?", type=Path, default=default_ifc,
                    help=f"IFC file to render (default: {default_ifc})")
    ap.add_argument("--out", type=Path, default=Path("graph_view.html"),
                    help="output HTML path (default: ./graph_view.html)")
    ap.add_argument("--mesh-bin", type=Path,
                    default=Path(shutil.which("ifcfast-mesh") or ""),
                    help="path to ifcfast-mesh binary (default: search PATH)")
    ap.add_argument("--no-mesh", action="store_true",
                    help="skip the 3D preview even if ifcfast-mesh is available")
    args = ap.parse_args()

    if not args.ifc.exists():
        ap.error(f"IFC not found: {args.ifc}")

    mesh_bin = None if args.no_mesh else (args.mesh_bin if args.mesh_bin.name else None)
    print(f"ifc:  {args.ifc}")
    print(f"out:  {args.out}")
    print(f"mesh: {mesh_bin or '(disabled)'}")
    emit_html(args.ifc, args.out, mesh_bin)


if __name__ == "__main__":
    main()
