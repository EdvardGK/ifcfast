"""``ifcfast`` command-line entry point.

Subcommands::

    ifcfast index   FILE         # tier-1 parse + print counts
    ifcfast extract FILE         # extract psets / quantities / materials / classifications
    ifcfast drift   FILE         # placement-vs-mesh drift report
    ifcfast cache   FILE [...]   # inspect / clear cache for a file

This is a thin wrapper around the library API — programmatic use should
call :func:`ifcfast.open` directly.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path


def _cmd_index(args: argparse.Namespace) -> int:
    import ifcfast

    m = ifcfast.open(args.file, use_cache=not args.no_cache, write_cache=not args.no_cache)
    print(f"path:          {m.header.path}")
    print(f"schema:        {m.schema}")
    print(f"authoring app: {m.authoring_app}")
    print(f"project:       {m.project_name}")
    print(f"products:      {len(m)}")
    print(f"storeys:       {len(m.storeys)}")
    print(f"parse time:    {m.parse_seconds:.3f}s")
    top = sorted(m.types().items(), key=lambda kv: -kv[1])[:10]
    if top:
        print("top types:")
        for entity, count in top:
            print(f"  {entity:<32} {count}")
    return 0


def _cmd_extract(args: argparse.Namespace) -> int:
    from ifcfast.cache import extract_data_layers

    layers = extract_data_layers(args.file, include_drift=False)
    rows = {
        "psets":           0 if layers.psets is None else len(layers.psets),
        "quantities":      0 if layers.quantities is None else len(layers.quantities),
        "materials":       0 if layers.materials is None else len(layers.materials),
        "classifications": 0 if layers.classifications is None else len(layers.classifications),
    }
    cold = layers.timing_ms.get("cold_parse", False)
    total = layers.timing_ms.get("total_ms", 0)
    print(f"{'cold parse' if cold else 'cache hit'}: {total:.0f} ms")
    for name, n in rows.items():
        print(f"  {name:<16} {n}")
    return 0


def _cmd_drift(args: argparse.Namespace) -> int:
    from ifcfast.cache import extract_data_layers

    layers = extract_data_layers(args.file, include_drift=True)
    df = layers.drift
    if df is None:
        print("drift unavailable: _core built without `mesh` feature")
        return 1
    print(f"products with geometry: {len(df)}")
    counts = df["drift_severity"].value_counts().to_dict()
    for sev in ("ok", "info", "warn", "error"):
        print(f"  {sev:<6} {counts.get(sev, 0)}")
    if args.top and counts.get("error", 0):
        print()
        print(df[df["drift_severity"] == "error"]
              .nlargest(args.top, "drift_distance")
              [["guid", "entity", "drift_distance", "max_extent", "drift_ratio"]]
              .to_string(index=False))
    return 0


def _cmd_cache(args: argparse.Namespace) -> int:
    from ifcfast.cache import cache_dir_for, has_data_cached, is_index_cached
    from ifcfast.header import header as _hdr

    hdr = _hdr(args.file)
    d = cache_dir_for(hdr)
    print(f"cache dir:       {d}")
    print(f"exists:          {d.exists()}")
    print(f"index cached:    {is_index_cached(hdr)}")
    cached = has_data_cached(hdr)
    for k, v in cached.items():
        print(f"  {k:<16} {v}")
    if args.clear and d.exists():
        import shutil

        shutil.rmtree(d)
        print(f"cleared {d}")
    return 0


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(prog="ifcfast", description="Fast IFC parser CLI")
    sub = p.add_subparsers(dest="cmd", required=True)

    pi = sub.add_parser("index", help="tier-1 index + counts")
    pi.add_argument("file", type=Path)
    pi.add_argument("--no-cache", action="store_true")
    pi.set_defaults(func=_cmd_index)

    pe = sub.add_parser("extract", help="extract data layers")
    pe.add_argument("file", type=Path)
    pe.set_defaults(func=_cmd_extract)

    pd = sub.add_parser("drift", help="placement / mesh drift report")
    pd.add_argument("file", type=Path)
    pd.add_argument("--top", type=int, default=10, help="show top-N errors")
    pd.set_defaults(func=_cmd_drift)

    pc = sub.add_parser("cache", help="inspect / clear cache")
    pc.add_argument("file", type=Path)
    pc.add_argument("--clear", action="store_true")
    pc.set_defaults(func=_cmd_cache)

    args = p.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
