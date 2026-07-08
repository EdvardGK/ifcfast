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
        [--tolerance-m 0.0]      # base clearance band passed to clash()
        [--rule-tol 'RULE=M']    # per-rule tolerance override (repeatable)
        [--baseline FILE]        # prior sweep JSON to diff against
        [--write-baseline FILE]  # save this sweep as a new baseline
        [--report FILE]          # full per-pair detail JSON

Per-rule tolerance (``--rule-tol``): Solibri rules differ in semantics —
a clash rule implies geometric contact (tolerance 0), a clearance rule a
band (e.g. rule 10.1 RIE–RIVv flags pairs ~54 mm apart). Each truth pair
is judged against ITS OWN rule's tolerance: matched iff the engine finds
it with ``min_distance_m <= tol(rule)``. ``RULE`` matches the topic's
rule tag exactly, or as a prefix when it ends with ``*``. Rules without
an override use ``--tolerance-m``. The engine runs once at the base
tolerance (full-set context, regression parity) plus at most ONE
supplemental run at the max rule tolerance on a **selection-scoped
mini-bundle** (only the guids of tolerance-band topics): every judged
pair has both endpoints inside the BCF selections and the narrow phase
is pure pairwise, so the mini-run distances are bit-identical for judged
pairs at a fraction of the cost (91 s vs 10.5 h measured, TMK13
3-model set at 0.1 m).
Rule tolerances are provisional until the full Solibri checking report
(rule parameters) is exported; the baseline locks them once chosen.

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
        schema = pq.read_schema(inst)
        meta = schema.metadata or {}
        cached = (meta.get(b"ifcfast.version") or b"").decode()
        # Version alone misses same-version schema changes on a dev
        # tree (GH #50 landed source_model within 0.4.42) — also
        # require the current column set's marker column.
        if cached == ifcfast.__version__ and "source_model" in schema.names:
            print(f"bundle cache hit: {out} (v{cached})")
            return out
        print(f"bundle cache stale ({cached} != {ifcfast.__version__} or pre-v29 schema): {out}")
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
        f"clash [{bundle_dir.name}]: {len(df)} pairs at tolerance {tolerance_m} m "
        f"({df.attrs.get('clash_ms', 0):.0f} ms, "
        f"{df.attrs.get('geometryless_skipped', 0)} geometryless skipped)"
    )
    return df


def parse_rule_tol(spec: str) -> tuple[str, float]:
    """``'10.1. RIE - RIVv=0.1'`` -> ``('10.1. RIE - RIVv', 0.1)``."""
    rule, sep, val = spec.rpartition("=")
    if not sep or not rule:
        raise argparse.ArgumentTypeError(
            f"--rule-tol needs 'RULE=METRES', got {spec!r}"
        )
    return rule, float(val)


def make_tol_for_rule(rule_tols: list[tuple[str, float]], base: float):
    """Effective tolerance for a Solibri rule tag: exact match first,
    then ``prefix*`` patterns in given order, else the base tolerance."""

    exact = {r: t for r, t in rule_tols if not r.endswith("*")}
    prefixes = [(r[:-1], t) for r, t in rule_tols if r.endswith("*")]

    def tol_for_rule(rule: str) -> float:
        if rule in exact:
            return exact[rule]
        for pre, t in prefixes:
            if rule.startswith(pre):
                return t
        return base

    return tol_for_rule


_EPS = 1e-6  # f32 min_distance_m vs f64 tolerance


def pair_matches(pair_meta: dict, pair, tolerance_m: float) -> bool:
    """A truth pair matches iff the engine found it within ``tolerance_m``."""
    meta = pair_meta.get(pair)
    return meta is not None and meta[2] <= tolerance_m + _EPS


def write_selection_bundle(fed_dir: Path, guids: set[str], out_dir: Path) -> int:
    """Filter a federated bundle down to ``guids`` (+ their reps).

    Every pair the sweep judges has BOTH endpoints inside the BCF topic
    selections, and the narrow phase is pure pairwise — so a clash run on
    this mini-bundle returns bit-identical ``min_distance_m`` for every
    judged pair at a fraction of the full-set cost (measured: 91 s vs
    10.5 h on TMK13 RIE+RIV+ARK at tolerance 0.1 m). ``pq.write_table``
    preserves the arrow schema + ``ifcfast.*`` metadata the strict Rust
    reader requires. Returns the instance row count.
    """
    import pyarrow as pa
    import pyarrow.compute as pc
    import pyarrow.parquet as pq

    out_dir.mkdir(parents=True, exist_ok=True)
    inst = pq.read_table(fed_dir / "instances.parquet")
    mini = inst.filter(pc.is_in(inst.column("guid"), value_set=pa.array(sorted(guids))))
    rep_ids = sorted({r for r in mini.column("rep_id").to_pylist() if r is not None})
    reps = pq.read_table(fed_dir / "representations.parquet")
    mreps = reps.filter(
        pc.is_in(
            reps.column("rep_id"),
            value_set=pa.array(rep_ids, type=reps.schema.field("rep_id").type),
        )
    )
    pq.write_table(mini, out_dir / "instances.parquet")
    pq.write_table(mreps, out_dir / "representations.parquet")
    return mini.num_rows


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
    ap.add_argument(
        "--rule-tol",
        type=parse_rule_tol,
        action="append",
        default=[],
        metavar="RULE=METRES",
        help="per-rule tolerance override; RULE matches the topic rule tag "
        "exactly, or as a prefix when it ends with '*' (repeatable)",
    )
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

    index = load_instance_index(fed_dir, unit_scale)
    tol_for_rule = make_tol_for_rule(args.rule_tol, args.tolerance_m)

    df = run_clash(fed_dir, args.tolerance_m)
    found_pairs = {frozenset((a, b)) for a, b in zip(df["guid_a"], df["guid_b"])}
    pair_meta = {
        frozenset((r.guid_a, r.guid_b)): (r.category, r.kind, float(r.min_distance_m))
        for r in df.itertuples()
    }

    # --- supplemental scoped run for tolerance-band rules ----------------
    band_topics = [t for t in topics if tol_for_rule(t.rule) > args.tolerance_m]
    supp_tol = max((tol_for_rule(t.rule) for t in band_topics), default=0.0)
    if band_topics:
        band_guids = {
            g for t in band_topics for g in t.selection_guids if g in index
        }
        if not band_guids:
            print("WARNING: no band-topic guid resolves in the bundle; "
                  "skipping supplemental run")
        else:
            mini_dir = args.cache_dir / "selection_scope" / fed_dir.name
            n = write_selection_bundle(fed_dir, band_guids, mini_dir)
            print(
                f"supplemental run for {len(band_topics)} tolerance-band topics "
                f"(rules: {sorted({t.rule for t in band_topics})}) — "
                f"selection-scoped mini-bundle, {n} instances"
            )
            df_supp = run_clash(mini_dir, supp_tol)
            for r in df_supp.itertuples():
                p = frozenset((r.guid_a, r.guid_b))
                prev = pair_meta.get(p)
                if prev is None or float(r.min_distance_m) < prev[2]:
                    pair_meta[p] = (r.category, r.kind, float(r.min_distance_m))

    def pair_within(pair, rule: str) -> bool:
        return pair_matches(pair_meta, pair, tol_for_rule(rule))

    # --- reconcile -------------------------------------------------------
    matched = {p for p in truth_pairs if pair_within(p, pair_rule.get(p, ""))}
    missed = truth_pairs - matched
    diagnoses = []
    for p in sorted(missed, key=sorted):
        d = diagnose_miss(p, index)
        meta = pair_meta.get(p)
        if meta is not None:  # engine found it, but beyond the rule's band
            d["reason"] = "outside_rule_tolerance"
            d["engine_min_distance_m"] = round(meta[2], 4)
            d["rule_tolerance_m"] = tol_for_rule(pair_rule.get(p, ""))
        diagnoses.append(d)

    topic_hits = 0
    topic_misses = []
    for t in topics:
        cands = t.all_candidate_pairs()
        if cands and any(pair_within(p, t.rule) for p in cands):
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
        print(f"  {r or '<no rule>':<32} {m}/{n}  @ tol {tol_for_rule(r)} m")

    if topic_misses:
        print("\n--- missed topics (no candidate pair within rule tolerance) ---")
        for t in topic_misses:
            gaps = [
                aabb_gap_m(index[a], index[b])
                for p in t.all_candidate_pairs()
                for a, b in [sorted(p)]
                if a in index and b in index
                and index[a]["has_geom"] and index[b]["has_geom"]
            ]
            gap_s = f"min AABB gap {min(gaps):.3f} m" if gaps else "no resolvable pair"
            print(f"  {t.topic_guid}  rule={t.rule!r}  "
                  f"{len(t.selection_guids)} guids, {len(t.all_candidate_pairs())} pairs, "
                  f"{gap_s}  title={t.title[:60]!r}")

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
        "rule_tolerances": {r: t for r, t in args.rule_tol},
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
        "rules": {
            r: {"n": n, "matched": m, "tolerance_m": tol_for_rule(r)}
            for r, (n, m) in sorted(rule_stats.items())
        },
        "matched": sorted("|".join(sorted(p)) for p in matched),
        "missed": sorted("|".join(sorted(p)) for p in missed),
    }

    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        detail = dict(result)
        detail["miss_diagnoses"] = diagnoses
        detail["topic_misses"] = [
            {
                "topic_guid": t.topic_guid,
                "title": t.title,
                "rule": t.rule,
                "n_selection_guids": len(t.selection_guids),
                "n_candidate_pairs": len(t.all_candidate_pairs()),
            }
            for t in topic_misses
        ]
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
