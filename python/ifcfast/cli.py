"""``ifcfast`` command-line entry point.

Subcommands::

    ifcfast demo                 # showcase against the bundled IFC
    ifcfast index   FILE         # tier-1 parse + print counts
    ifcfast schema  FILE         # full schema introspection (JSON-friendly)
    ifcfast extract FILE         # extract data layers
    ifcfast drift   FILE         # placement-vs-mesh drift report
    ifcfast cache   FILE [...]   # inspect / clear cache for a file

All subcommands accept ``--json`` to print machine-parseable output for
agents and pipelines (pipe through ``jq`` etc). This wraps the library
API — programmatic use should call :func:`ifcfast.open` directly.
"""

from __future__ import annotations

import argparse
import json
import sys
import zipfile
from pathlib import Path
from typing import Any


# ----------------------------------------------------------------------
# Output formatting
# ----------------------------------------------------------------------


def _emit(payload: dict, args: argparse.Namespace, *, pretty_lines: list[str]) -> None:
    """Print ``payload`` as JSON (--json mode) or the pre-built text lines."""
    if getattr(args, "json", False):
        print(json.dumps(payload, indent=2, default=_json_default))
        return
    for line in pretty_lines:
        print(line)


def _json_default(o: Any) -> Any:
    from pathlib import Path as _P

    if isinstance(o, _P):
        return str(o)
    if hasattr(o, "isoformat"):
        return o.isoformat()
    raise TypeError(f"not JSON-serialisable: {type(o).__name__}")


# ----------------------------------------------------------------------
# Subcommands
# ----------------------------------------------------------------------


def _cmd_demo(args: argparse.Namespace) -> int:
    import ifcfast

    m = ifcfast.open(ifcfast.example_path(), use_cache=False, write_cache=False)
    summary = m.summary()
    summary["sample_products"] = m.preview("products", n=2)
    summary["sample_aggregates"] = m.preview("aggregates", n=3)
    summary["agent_helpers"] = [
        "ifcfast.example_path()", "ifcfast.system_prompt()",
        "m.summary()", "m.schemas", "m.preview(table, n=5)",
        "m.parent(g) / .children(g) / .ancestors(g) / .descendants(g)",
        "m.storey_of(g) / .building_of(g) / .products_in(parent_g)",
    ]
    pretty = [
        "ifcfast demo — bundled minimal IFC4 fixture",
        "-" * 50,
        f"path:          {summary['path']}",
        f"schema:        {summary['schema']}",
        f"products:      {summary['products']}",
        f"storeys:       {summary['storeys']}",
        f"parse time:    {summary['parse_seconds']*1000:.2f} ms",
        f"top types:     {summary['top_types']}",
        "",
        "Tables on this model:",
    ]
    for name, meta in summary["tables"].items():
        loaded = "yes" if meta["loaded"] else "lazy"
        pretty.append(f"  {name:<16} rows={meta['rows']:<6} loaded={loaded}")
    pretty.append("")
    pretty.append("Agent ramp-up: ifcfast.system_prompt() → paste into your prompt.")
    _emit(summary, args, pretty_lines=pretty)
    return 0


def _cmd_index(args: argparse.Namespace) -> int:
    import ifcfast

    m = ifcfast.open(args.file, use_cache=not args.no_cache, write_cache=not args.no_cache)
    summary = m.summary()
    top = list(summary["top_types"].items())[:10]
    pretty = [
        f"path:          {summary['path']}",
        f"schema:        {summary['schema']}",
        f"authoring app: {summary['authoring_app']}",
        f"project:       {summary['project_name']}",
        f"products:      {summary['products']}",
        f"storeys:       {summary['storeys']}",
        f"parse time:    {summary['parse_seconds']:.3f}s",
    ]
    if top:
        pretty.append("top types:")
        for entity, count in top:
            pretty.append(f"  {entity:<32} {count}")
    _emit(summary, args, pretty_lines=pretty)
    return 0


def _cmd_schema(args: argparse.Namespace) -> int:
    import ifcfast

    m = ifcfast.open(args.file, use_cache=not args.no_cache, write_cache=not args.no_cache)
    payload = {"path": str(m.header.path), "schemas": m.schemas}
    pretty = [f"path: {payload['path']}", ""]
    for name, info in payload["schemas"].items():
        loaded = "yes" if info["loaded"] else "lazy"
        pretty.append(f"{name}  ({info['rows']} rows, loaded={loaded})")
        for col in info["columns"]:
            pretty.append(f"  {col:<24} {info['dtypes'].get(col, '')}")
        pretty.append("")
    _emit(payload, args, pretty_lines=pretty)
    return 0


def _cmd_types(args: argparse.Namespace) -> int:
    """Type-first extraction — sprucelab-shaped TypeBank seed."""
    import ifcfast

    m = ifcfast.open(args.file, use_cache=not args.no_cache, write_cache=not args.no_cache)
    rows = m.type_bank(sample_guids=args.samples) if args.with_data else m.type_summary(
        sample_guids=args.samples
    )
    payload = {
        "path": str(args.file),
        "schema": m.schema,
        "types": rows,
        "total_types": len(rows),
        "total_products": len(m),
    }
    pretty = [f"{payload['total_types']} unique types in {payload['total_products']} products"]
    pretty.append("")
    for r in rows[: args.top]:
        line = f"  {r['entity']:<32} count={r['count']:<6}"
        if r["storeys"]:
            line += f"  storeys={len(r['storeys'])}"
        if r["predefined_types"]:
            line += f"  predef={r['predefined_types']}"
        pretty.append(line)
    _emit(payload, args, pretty_lines=pretty)
    return 0


def _cmd_extract(args: argparse.Namespace) -> int:
    from ifcfast.cache import extract_data_layers

    layers = extract_data_layers(args.file, include_drift=False)
    counts = {
        "psets":           0 if layers.psets is None else int(len(layers.psets)),
        "quantities":      0 if layers.quantities is None else int(len(layers.quantities)),
        "materials":       0 if layers.materials is None else int(len(layers.materials)),
        "classifications": 0 if layers.classifications is None else int(len(layers.classifications)),
    }
    cold = bool(layers.timing_ms.get("cold_parse", False))
    total_ms = float(layers.timing_ms.get("total_ms", 0))
    payload = {
        "path": str(args.file),
        "cold_parse": cold,
        "total_ms": total_ms,
        "row_counts": counts,
        "timings_ms": {k: float(v) for k, v in layers.timing_ms.items()},
    }
    pretty = [f"{'cold parse' if cold else 'cache hit'}: {total_ms:.0f} ms"]
    for name, n in counts.items():
        pretty.append(f"  {name:<16} {n}")
    _emit(payload, args, pretty_lines=pretty)
    return 0


def _cmd_drift(args: argparse.Namespace) -> int:
    import ifcfast

    m = ifcfast.open(args.file)
    df = m.drift
    if df is None:
        msg = "drift unavailable: _core built without `mesh` feature"
        if getattr(args, "json", False):
            print(json.dumps({"error": msg}))
        else:
            print(msg)
        return 1
    counts = {k: int(v) for k, v in df["drift_severity"].value_counts().to_dict().items()}
    baked = bool(m.world_coordinate_baked)
    payload = {
        "path": str(args.file),
        "products_with_geometry": int(len(df)),
        "severity_counts": {sev: counts.get(sev, 0) for sev in ("ok", "info", "warn", "error")},
        "world_coordinate_baked": baked,
    }
    # Top-N "drift offenders" — in a baked-pattern model all the
    # interesting rows have been demoted to `info`, so widen the pool
    # past `error` and rank by the raw drift signal (which is
    # untouched by the model-level demotion). Still useful for
    # eyeballing the worst cases even in pattern mode.
    if args.top:
        non_ok = df[df["drift_severity"] != "ok"]
        if not non_ok.empty:
            top_drift = (
                non_ok.nlargest(args.top, "drift_distance_m")
                [["guid", "entity", "drift_distance_m", "max_extent_m", "drift_ratio", "drift_severity"]]
            )
            payload["top_drift"] = top_drift.to_dict(orient="records")
    pretty = [f"products with geometry: {payload['products_with_geometry']}"]
    for sev in ("ok", "info", "warn", "error"):
        pretty.append(f"  {sev:<6} {payload['severity_counts'][sev]}")
    if baked:
        pretty.append("")
        pretty.append(
            "note: model-level drift pattern detected (>=25% of meshed "
            "products would be 'error' under the per-row rule). All "
            "would-be 'error'/'warn' rows demoted to 'info' — this is "
            "an authoring-style fingerprint (e.g. Tekla / IFC2X3 baked-"
            "in-world-coords, building-origin-anchored placements), "
            "not per-element bugs. Raw drift columns are unchanged; "
            "filter on `drift_distance_m` / `drift_ratio` for the raw "
            "signal."
        )
    if "top_drift" in payload:
        pretty.append("")
        pretty.append(
            df[df["drift_severity"] != "ok"]
            .nlargest(args.top, "drift_distance_m")
            [["guid", "entity", "drift_distance_m", "max_extent_m", "drift_ratio", "drift_severity"]]
            .to_string(index=False)
        )
    _emit(payload, args, pretty_lines=pretty)
    return 0


def _cmd_bundle(args: argparse.Namespace) -> int:
    """Write the parquet substrate (`ifcfast.clash` input) from an IFC file."""
    import ifcfast

    info = ifcfast.bundle(args.file, args.out_dir)
    ratio = (
        float(info["instances_written"]) / float(info["unique_reps_written"])
        if info["unique_reps_written"] > 0
        else 0.0
    )
    total_mb = (
        float(info["instances_parquet_bytes"]) + float(info["representations_parquet_bytes"])
    ) / 1e6
    pretty = [
        f"path:                {args.file}",
        f"bundle dir:          {info['bundle_dir']}",
        f"products seen:       {info['products_seen']}",
        f"products meshed:     {info['products_meshed']}  (deferred {info['products_deferred']})",
        f"triangles emitted:   {info['triangles']}",
        f"instances written:   {info['instances_written']}",
        f"unique reps written: {info['unique_reps_written']}  (instance/rep {ratio:.2f}x)",
        f"substrate size:      {total_mb:.1f} MB",
        f"open:                {info['open_ms']:.1f} ms",
        f"semantic pre-pass:   {info['bundle_ms']:.1f} ms",
        f"streaming mesh:      {info['stream_ms']:.1f} ms",
        "",
        "Files:",
        f"  {info['instances_parquet']}",
        f"  {info['representations_parquet']}",
        f"  {info['view_sql']}",
        "",
        f"Next: import ifcfast; ifcfast.clash({info['bundle_dir']!r})",
    ]
    _emit(info, args, pretty_lines=pretty)
    return 0


def _cmd_cache(args: argparse.Namespace) -> int:
    from ifcfast.cache import cache_dir_for, has_data_cached, is_index_cached
    from ifcfast.header import header as _hdr

    hdr = _hdr(args.file)
    d = cache_dir_for(hdr)
    cached = has_data_cached(hdr)
    payload = {
        "path": str(args.file),
        "cache_dir": str(d),
        "exists": d.exists(),
        "index_cached": is_index_cached(hdr),
        "data_cached": {k: bool(v) for k, v in cached.items()},
    }
    if args.clear and d.exists():
        import shutil

        shutil.rmtree(d)
        payload["cleared"] = str(d)
    pretty = [
        f"cache dir:       {payload['cache_dir']}",
        f"exists:          {payload['exists']}",
        f"index cached:    {payload['index_cached']}",
    ]
    for k, v in payload["data_cached"].items():
        pretty.append(f"  {k:<16} {v}")
    if "cleared" in payload:
        pretty.append(f"cleared {payload['cleared']}")
    _emit(payload, args, pretty_lines=pretty)
    return 0


# ----------------------------------------------------------------------
# Entry point
# ----------------------------------------------------------------------


def _force_utf8_stdio() -> None:
    """Reconfigure stdout/stderr to UTF-8 so non-ASCII glyphs in pretty
    output don't crash on Windows consoles (cp1252 by default).

    ``--json`` paths emit ASCII-only JSON and aren't affected; this
    only matters for the pretty-print branches that contain em-dashes,
    arrows, etc. Best-effort: silently noops on streams that don't
    expose ``reconfigure`` (older Pythons, redirected pipes that
    pre-set encoding).
    """
    for stream in (sys.stdout, sys.stderr):
        try:
            stream.reconfigure(encoding="utf-8")
        except Exception:  # pragma: no cover
            pass


def main(argv: list[str] | None = None) -> int:
    _force_utf8_stdio()
    p = argparse.ArgumentParser(
        prog="ifcfast",
        description="Fast IFC parser CLI - agent-first, JSON-friendly.",
    )
    sub = p.add_subparsers(dest="cmd", required=True)

    def _add_json(parser):
        parser.add_argument(
            "--json", action="store_true",
            help="emit machine-parseable JSON instead of text",
        )

    pd0 = sub.add_parser("demo", help="run against bundled minimal IFC")
    _add_json(pd0)
    pd0.set_defaults(func=_cmd_demo)

    pi = sub.add_parser("index", help="tier-1 index + counts")
    pi.add_argument("file", type=Path)
    pi.add_argument("--no-cache", action="store_true")
    _add_json(pi)
    pi.set_defaults(func=_cmd_index)

    ps = sub.add_parser("schema", help="dump full schema for every table")
    ps.add_argument("file", type=Path)
    ps.add_argument("--no-cache", action="store_true")
    _add_json(ps)
    ps.set_defaults(func=_cmd_schema)

    pt = sub.add_parser(
        "types",
        help="type-first extraction (TypeBank-shaped)",
    )
    pt.add_argument("file", type=Path)
    pt.add_argument("--no-cache", action="store_true")
    pt.add_argument(
        "--with-data",
        action="store_true",
        help="also extract materials + classifications per type (slower)",
    )
    pt.add_argument(
        "--top", type=int, default=20,
        help="show top-N types in pretty mode (no effect in --json)",
    )
    pt.add_argument(
        "--samples", type=int, default=3,
        help="sample GUIDs per type",
    )
    _add_json(pt)
    pt.set_defaults(func=_cmd_types)

    pe = sub.add_parser("extract", help="extract data layers")
    pe.add_argument("file", type=Path)
    _add_json(pe)
    pe.set_defaults(func=_cmd_extract)

    pdc = sub.add_parser("drift", help="placement / mesh drift report")
    pdc.add_argument("file", type=Path)
    pdc.add_argument("--top", type=int, default=10, help="show top-N errors")
    _add_json(pdc)
    pdc.set_defaults(func=_cmd_drift)

    pc = sub.add_parser("cache", help="inspect / clear cache")
    pc.add_argument("file", type=Path)
    pc.add_argument("--clear", action="store_true")
    _add_json(pc)
    pc.set_defaults(func=_cmd_cache)

    pb = sub.add_parser(
        "bundle",
        help="write the parquet substrate (instances/representations) for ifcfast.clash",
    )
    pb.add_argument("file", type=Path, help="path to IFC (.ifc or .ifczip)")
    pb.add_argument(
        "out_dir",
        type=Path,
        nargs="?",
        default=None,
        help="output directory; defaults to {stem}.bundle/ next to FILE",
    )
    _add_json(pb)
    pb.set_defaults(func=_cmd_bundle)

    args = p.parse_args(argv)
    try:
        return args.func(args)
    except FileNotFoundError as e:
        # Clean message to stderr; no traceback. GH #42.
        print(f"ifcfast: {e}", file=sys.stderr)
        return 1
    except (PermissionError, IsADirectoryError) as e:
        print(f"ifcfast: {type(e).__name__}: {e}", file=sys.stderr)
        return 1
    except (ValueError, zipfile.BadZipFile) as e:
        # Bad *content* (not a STEP file, truncated, empty archive, …)
        # gets the same clean treatment as bad paths — agents parse
        # stderr/exit codes, not tracebacks. GH #84.
        print(f"ifcfast: {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
