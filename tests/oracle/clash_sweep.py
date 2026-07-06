"""Clash oracle sweep: ifcfast.clash() vs Solibri BCF ground truth.

Phase 1 keystone of the coordinator-staple roadmap (GH #140 / #141) — the
#59 gate-first discipline applied to clash. Solibri coordination rounds
(TMK BCF exports) are the incumbent truth; every clash pair Solibri
reported must be found by ``ifcfast.clash()`` on a federated bundle of the
SAME model versions the round was checked against (BCF Header/File dates
= the models' internal STEP timestamps — match versions first).

This is a RECALL gate. BCF rounds are triaged subsets of Solibri's full
checking result, so ifcfast pairs absent from the BCF are attributed
(class-pair table), never failed.

Usage (repo root, venv active)::

    python -m tests.oracle.clash_sweep \
        --bcf scratch/g55/solibri/TMK13_Plan5.bcf \
        --ifc scratch/g55/solibri/models_tmk13/G55_RIE.ifc \
        --ifc scratch/g55/solibri/models_tmk13/G55_RIV.ifc \
        [--cache-dir DIR]        # bundle cache; skip re-bundling
        [--tolerance-m 0.0]      # clearance band passed to clash()
        [--baseline FILE]        # prior sweep JSON to diff against
        [--write-baseline FILE]  # save this sweep as a new baseline
        [--report FILE]          # full per-pair detail JSON

Baselines live OUTSIDE the repo (client data), convention
``scratch/<corpus>/baselines/clash_<round>.json``. Exit 1 when a pair that
matched in the baseline is missed now (regression), or when recall drops.
"""

from __future__ import annotations

import argparse
import json
import math
import sys
from collections import Counter
from pathlib import Path

from .bcf_truth import load_bcf
from .federate import federate_bundles


def ensure_bundle(ifc: Path, cache_dir: Path) -> Path:
    """Bundle ``ifc`` under the cache, reusing only same-version bundles."""
    import ifcfast
    import pyarrow.parquet as pq

    out = cache_dir / "bundles" / ifc.stem
    inst = out / "instances.parquet"
    if inst.is_file():
        meta = pq.read_schema(inst).metadata or {}
        cached = (meta.get(b"ifcfast.version") or b"").decode()
        if cached == ifcfast.__version__:
            print(f"bundle cache hit: {out} (v{cached})")
            return out
        print(f"bundle cache stale ({cached} != {ifcfast.__version__}): {out}")
    print(f"bundling {ifc.name} ...", flush=True)
    info = ifcfast.bundle(str(ifc), out_dir=str(out))
    print(
        f"  {info['products_indexed']} products, "
        f"{info['triangles']} triangles, {info['bundle_ms']:.0f} ms"
    )
    return out


def run_clash(bundle_dir: Path, tolerance_m: float):
    import ifcfast

    df = ifcfast.clash(str(bundle_dir), tolerance_m=tolerance_m, write_parquet=False)
    print(
        f"clash: {len(df)} pairs at tolerance {tolerance_m} m "
        f"({df.attrs.get('clash_ms', 0):.0f} ms, "
        f"{df.attrs.get('geometryless_skipped', 0)} geometryless skipped)"
    )
    return df


def load_instance_index(bundle_dir: Path, unit_scale: float) -> dict[str, dict]:
    """guid -> {class, name, has_geom, bbox (metres)} for miss diagnosis."""
    import pyarrow.parquet as pq

    t = pq.read_table(
        bundle_dir / "instances.parquet",
        columns=["guid", "class", "name", "rep_id", "bbox_min_xyz", "bbox_max_xyz"],
    )
    out: dict[str, dict] = {}
    for guid, cls, name, rep_id, lo, hi in zip(
        t.column("guid").to_pylist(),
        t.column("class").to_pylist(),
        t.column("name").to_pylist(),
        t.column("rep_id").to_pylist(),
        t.column("bbox_min_xyz").to_pylist(),
        t.column("bbox_max_xyz").to_pylist(),
    ):
        out[guid] = {
            "class": cls,
            "name": name,
            "has_geom": rep_id is not None,
            "bbox_min": [v * unit_scale for v in lo],
            "bbox_max": [v * unit_scale for v in hi],
        }
    return out


def aabb_gap_m(a: dict, b: dict) -> float:
    """Euclidean gap between two axis-aligned boxes (0.0 = overlapping)."""
    total = 0.0
    for i in range(3):
        gap = max(a["bbox_min"][i] - b["bbox_max"][i], b["bbox_min"][i] - a["bbox_max"][i], 0.0)
        total += gap * gap
    return math.sqrt(total)


def diagnose_miss(pair, index: dict[str, dict]) -> dict:
    a, b = sorted(pair)
    ia, ib = index.get(a), index.get(b)
    d: dict = {"guid_a": a, "guid_b": b}
    if ia is None or ib is None:
        d["reason"] = "guid_not_in_federated_bundle"
        d["missing"] = [g for g, i in ((a, ia), (b, ib)) if i is None]
        return d
    d["class_a"], d["class_b"] = ia["class"], ib["class"]
    d["name_a"], d["name_b"] = ia["name"], ib["name"]
    if not ia["has_geom"] or not ib["has_geom"]:
        d["reason"] = "geometryless_element"
        d["geometryless"] = [g for g, i in ((a, ia), (b, ib)) if not i["has_geom"]]
        return d
    gap = aabb_gap_m(ia, ib)
    d["aabb_gap_m"] = round(gap, 4)
    d["reason"] = "aabb_separated" if gap > 0 else "narrow_phase_miss"
    return d


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--bcf", type=Path, action="append", required=True)
    ap.add_argument("--ifc", type=Path, action="append", required=True)
    ap.add_argument("--cache-dir", type=Path, default=Path("scratch/clash_oracle_cache"))
    ap.add_argument("--tolerance-m", type=float, default=0.0)
    ap.add_argument("--baseline", type=Path, default=None)
    ap.add_argument("--write-baseline", type=Path, default=None)
    ap.add_argument("--report", type=Path, default=None)
    args = ap.parse_args()

    if len(args.ifc) < 2:
        print("need at least two --ifc models to federate", file=sys.stderr)
        return 2

    # --- truth ---------------------------------------------------------
    truths = [load_bcf(p) for p in args.bcf]
    truth_pairs: set[frozenset] = set()
    pair_rule: dict[frozenset, str] = {}
    topics = []
    for tr in truths:
        truth_pairs |= tr.clean_pairs()
        topics.extend(tr.topics)
        for t in tr.topics:
            for p in t.clean_pairs():
                pair_rule.setdefault(p, t.rule)
        print(f"truth {tr.path.name}: {len(tr.topics)} topics, "
              f"{len(tr.clean_pairs())} clean pairs")
        for m in tr.models[:6]:
            print(f"    {m['filename']}  {m['date']}  ({m['occurrences']}x)")

    # --- ifcfast side ---------------------------------------------------
    bundles = [ensure_bundle(p, args.cache_dir) for p in args.ifc]
    fed_dir = args.cache_dir / "federated" / "+".join(sorted(b.name for b in bundles))
    sidecar = federate_bundles(bundles, fed_dir)
    if sidecar["guid_collisions"]:
        print(f"WARNING: {len(sidecar['guid_collisions'])} guid collisions across sources")
    unit_scale = float(sidecar["unit_scale"])

    df = run_clash(fed_dir, args.tolerance_m)
    found_pairs = {frozenset((a, b)) for a, b in zip(df["guid_a"], df["guid_b"])}
    pair_meta = {
        frozenset((r.guid_a, r.guid_b)): (r.category, r.kind, float(r.min_distance_m))
        for r in df.itertuples()
    }

    # --- reconcile -------------------------------------------------------
    matched = {p for p in truth_pairs if p in found_pairs}
    missed = truth_pairs - matched
    index = load_instance_index(fed_dir, unit_scale)
    diagnoses = [diagnose_miss(p, index) for p in sorted(missed, key=sorted)]

    topic_hits = 0
    topic_misses = []
    for t in topics:
        cands = t.all_candidate_pairs()
        if cands and (cands & found_pairs):
            topic_hits += 1
        elif cands:
            topic_misses.append(t)
    n_topics_eval = sum(1 for t in topics if t.all_candidate_pairs())

    src = sidecar["guid_source"]
    cross = [p for p in found_pairs if len({src.get(g) for g in p}) > 1]
    extra_cross = [p for p in cross if p not in truth_pairs]
    extra_classes = Counter()
    for p in extra_cross:
        a, b = sorted(p)
        ca = index.get(a, {}).get("class", "?")
        cb = index.get(b, {}).get("class", "?")
        extra_classes[tuple(sorted((ca, cb)))] += 1

    pair_recall = len(matched) / len(truth_pairs) if truth_pairs else float("nan")
    topic_recall = topic_hits / n_topics_eval if n_topics_eval else float("nan")

    print(f"\n=== clash oracle @ tolerance {args.tolerance_m} m ===")
    print(f"truth clean pairs : {len(truth_pairs)}  matched {len(matched)}  "
          f"recall {pair_recall:.1%}")
    print(f"topics (any-pair) : {n_topics_eval}  matched {topic_hits}  "
          f"recall {topic_recall:.1%}")
    print(f"ifcfast pairs     : {len(found_pairs)} total, {len(cross)} cross-model, "
          f"{len(extra_cross)} cross-model not in truth (attributed, not failed)")
    rule_stats: dict[str, list[int]] = {}
    for p in truth_pairs:
        r = pair_rule.get(p, "?")
        rule_stats.setdefault(r, [0, 0])
        rule_stats[r][0] += 1
        if p in matched:
            rule_stats[r][1] += 1
    print("\n--- clean-pair recall per Solibri rule ---")
    for r, (n, m) in sorted(rule_stats.items()):
        print(f"  {r or '<no rule>':<32} {m}/{n}")

    if diagnoses:
        print("\n--- missed truth pairs ---")
        for d in diagnoses:
            rule = pair_rule.get(frozenset((d["guid_a"], d["guid_b"])), "?")
            line = f"  {d['guid_a']} x {d['guid_b']}: {d['reason']}  rule={rule!r}"
            if "class_a" in d:
                line += f"  [{d['class_a']} x {d['class_b']}]"
            if "aabb_gap_m" in d:
                line += f"  gap={d['aabb_gap_m']} m"
            print(line)
    if extra_classes:
        print("\n--- top extra cross-model class pairs (context, not failures) ---")
        for (ca, cb), n in extra_classes.most_common(10):
            print(f"  {ca:>24} x {cb:<24} {n:>6}")

    result = {
        "tolerance_m": args.tolerance_m,
        "bcf": [str(p) for p in args.bcf],
        "ifc": [str(p) for p in args.ifc],
        "n_truth_pairs": len(truth_pairs),
        "n_matched": len(matched),
        "pair_recall": pair_recall,
        "n_topics": n_topics_eval,
        "n_topics_matched": topic_hits,
        "topic_recall": topic_recall,
        "n_found_pairs": len(found_pairs),
        "n_cross_model_pairs": len(cross),
        "rules": {r: {"n": n, "matched": m} for r, (n, m) in sorted(rule_stats.items())},
        "matched": sorted("|".join(sorted(p)) for p in matched),
        "missed": sorted("|".join(sorted(p)) for p in missed),
    }

    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        detail = dict(result)
        detail["miss_diagnoses"] = diagnoses
        detail["extra_class_pairs"] = {
            f"{ca}|{cb}": n for (ca, cb), n in extra_classes.most_common()
        }
        detail["pair_meta_matched"] = {
            "|".join(sorted(p)): pair_meta[p] for p in matched
        }
        args.report.write_text(json.dumps(detail, indent=1))
        print(f"\nreport written: {args.report}")

    rc = 0
    if args.baseline:
        base = json.loads(args.baseline.read_text())
        regressed = sorted(set(base.get("matched", [])) - set(result["matched"]))
        if regressed:
            print(f"\nREGRESSION vs {args.baseline.name}: "
                  f"{len(regressed)} previously-matched pairs now missed:")
            for r in regressed:
                print(f"  {r}")
            rc = 1
        else:
            newly = sorted(set(result["matched"]) - set(base.get("matched", [])))
            print(f"\nno regression vs {args.baseline.name}"
                  + (f"; {len(newly)} newly matched" if newly else ""))

    if args.write_baseline:
        args.write_baseline.parent.mkdir(parents=True, exist_ok=True)
        args.write_baseline.write_text(json.dumps(result, indent=1))
        print(f"baseline written: {args.write_baseline}")

    return rc


if __name__ == "__main__":
    sys.exit(main())
