#!/usr/bin/env python3
"""Hybrid QTO routing — ifcfast for speed, an authoritative kernel for edge cases.

ifcfast computes per-product geometric quantities ~14-46x faster than a
geometry kernel, but a small fraction of products carry geometry that
ifcfast can't volume reliably (open shells, non-watertight
``IfcFaceBasedSurfaceModel``, inverted/inconsistent winding, ...). Rather
than silently emit a wrong number, ifcfast *flags* those rows — so a
pipeline can **escalate only the flagged rows** to an authoritative tool
(``ifcopenshell`` here; Solibri, an in-house kernel, or a human-review
queue plug in the same way) and keep ifcfast's speed on the trustworthy
majority.

This is the canonical agent-first pattern: **the fast tool self-labels
its confidence, and the orchestrator routes on the label.** Because the
flagged set is tiny (~1% on typical models), you call the slow kernel a
fraction of the time and the speedup survives the hybrid. The same shape
drops into n8n (IF node), Power Automate (Condition), a cron/Python job,
or an MCP agent flow.

Usage
-----
    python examples/hybrid_qto_routing.py [path/to/model.ifc]

With no argument it runs on the bundled demo model. Escalation requires
``ifcopenshell`` and ``numpy`` (dev extras); the fast pass needs only
``ifcfast``.
"""

from __future__ import annotations

import sys
import time

import ifcfast


def fast_pass(path: str) -> list[dict]:
    """Run ifcfast's QTO and read the per-row reliability flag.

    ifcfast self-labels volume confidence as a first-class column
    (``volume_reliable``, since cache schema v16 / GH #60): ``False``
    means the signed-tetra mesh volume isn't trustworthy (open shell,
    degenerate rep, inverted winding). For those rows ifcfast also
    substitutes a ``footprint x height`` prism estimate into
    ``volume_m3`` and records ``volume_method = "prism_fallback"`` — so
    even with **no** escalation you get a sane number instead of garbage.
    The raw mesh value stays on ``volume_mesh_m3`` for transparency.

    This pass just reads the flag; ``main`` decides whether the prism
    fallback is good enough or the row warrants a kernel-grade volume.
    """
    q, _ = ifcfast.open(path).mesh_qto()
    rows: list[dict] = []
    for guid, entity, vol, mesh_vol, method, reliable in zip(
        q["guid"],
        q["entity"],
        q["volume_m3"],
        q["volume_mesh_m3"],
        q["volume_method"],
        q["volume_reliable"],
    ):
        rows.append(
            {
                "guid": guid,
                "entity": entity,
                "volume_m3": float(vol),  # best estimate (mesh or prism)
                "volume_mesh_m3": float(mesh_vol),  # raw mesh value
                "volume_method": str(method),
                "volume_reliable": bool(reliable),
            }
        )
    return rows


def authoritative_volume(path: str, guids: list[str]) -> dict[str, float | None]:
    """True closed-solid volume for specific GUIDs, via ifcopenshell's kernel.

    Called ONLY for the flagged rows, so the slow kernel runs on a
    fraction of the model. Returns ``None`` for a GUID whose geometry the
    kernel also can't resolve (route those to human review).
    """
    try:
        import numpy as np
        import ifcopenshell
        import ifcopenshell.geom as geom
        import ifcopenshell.util.shape as ushape
    except ImportError as exc:  # pragma: no cover - optional dependency
        raise SystemExit(
            "Escalation needs ifcopenshell + numpy: pip install ifcopenshell numpy"
        ) from exc

    f = ifcopenshell.open(path)
    settings = geom.settings()
    settings.set(settings.USE_WORLD_COORDS, True)

    out: dict[str, float | None] = {}
    for guid in guids:
        try:
            shape = geom.create_shape(settings, f.by_guid(guid))
            out[guid] = float(ushape.get_volume(shape.geometry))
        except Exception:  # kernel failed too -> needs a human
            out[guid] = None
    return out


def main(path: str) -> int:
    # --- 1. Fast pass: every product, with a confidence flag. -----------
    t0 = time.perf_counter()
    rows = fast_pass(path)
    t_fast = time.perf_counter() - t0

    flagged = [r for r in rows if not r["volume_reliable"]]
    pct = len(flagged) / max(len(rows), 1) * 100
    print(
        f"ifcfast: {len(rows)} products volumed in {t_fast * 1000:.0f} ms; "
        f"{len(flagged)} flagged for review ({pct:.1f}%)"
    )

    if not flagged:
        print("All volumes within their bounding box — nothing to escalate.")
        return 0

    # --- 2. Escalate ONLY the flagged rows to the authoritative kernel. -
    t1 = time.perf_counter()
    authoritative = authoritative_volume(path, [r["guid"] for r in flagged])
    t_slow = time.perf_counter() - t1
    print(
        f"ifcopenshell escalation: {len(flagged)} products in "
        f"{t_slow * 1000:.0f} ms\n"
    )

    print(
        f"{'guid':24} {'entity':20} {'mesh (raw)':>12} "
        f"{'prism fallbk':>12} {'authoritative':>14}"
    )
    for r in flagged:
        a = authoritative.get(r["guid"])
        a_str = "kernel-failed" if a is None else f"{a:.3f}"
        print(
            f"{r['guid']:24} {r['entity']:20} "
            f"{r['volume_mesh_m3']:12.3f} {r['volume_m3']:12.3f} {a_str:>14}"
        )

    # --- 3. Merge: kernel-grade where escalation succeeded, else keep -----
    # ifcfast's own number (mesh volume on reliable rows; prism fallback on
    # flagged rows the kernel couldn't resolve — already in volume_m3).
    for r in rows:
        a = authoritative.get(r["guid"])
        if not r["volume_reliable"] and a is not None:
            r["final_volume_m3"] = a
        else:
            r["final_volume_m3"] = r["volume_m3"]

    total = sum(r["final_volume_m3"] for r in rows)
    print(f"\nMerged total volume: {total:.1f} m3 across {len(rows)} products.")
    return 0


if __name__ == "__main__":
    target = sys.argv[1] if len(sys.argv) > 1 else ifcfast.example_path()
    raise SystemExit(main(target))
