"""Dump the four attribute tables + unit_scale per model to parquet for a bitwise A/B.

    python scripts/dump_tables_ab.py OUT_DIR MODEL.ifc [MODEL2.ifc ...]
    python scripts/dump_tables_ab.py --compare DIR_A DIR_B

Compare uses pyarrow Table.equals after replacing NaN with a sentinel
(pa.Table.equals is NaN-hostile — see the clash parity memory).
"""
import sys, pathlib
import pandas as pd
import pyarrow as pa, pyarrow.parquet as pq

TABLES = ("psets", "quantities", "materials", "classifications")

def dump(out, paths):
    import ifcfast
    out = pathlib.Path(out); out.mkdir(parents=True, exist_ok=True)
    for p in paths:
        m = ifcfast.open(p)
        stem = pathlib.Path(p).stem
        for t in TABLES:
            df = getattr(m, t)
            df.to_parquet(out / f"{stem}.{t}.parquet", index=False)
            print(f"{stem}.{t}: {len(df)} rows, {len(df.columns)} cols")
        (out / f"{stem}.unit_scale.txt").write_text(repr(m.unit_scale))
        print(f"{stem}.unit_scale = {m.unit_scale!r}")

def compare(a, b):
    a, b = pathlib.Path(a), pathlib.Path(b)
    bad = 0
    for fa in sorted(a.glob("*.parquet")):
        fb = b / fa.name
        if not fb.exists():
            print(f"MISSING in B: {fa.name}"); bad += 1; continue
        ta, tb = pq.read_table(fa), pq.read_table(fb)
        da, db = ta.to_pandas().fillna("<<NaN>>"), tb.to_pandas().fillna("<<NaN>>")
        same = da.equals(db) and ta.schema.equals(tb.schema)
        print(f"{'OK  ' if same else 'DIFF'} {fa.name}: {len(da)} vs {len(db)} rows")
        if not same:
            bad += 1
            if len(da) == len(db) and list(da.columns) == list(db.columns):
                for c in da.columns:
                    n = int((da[c].astype(str) != db[c].astype(str)).sum())
                    if n: print(f"       column {c}: {n} cells differ")
            else:
                print(f"       columns A={list(da.columns)}\n       columns B={list(db.columns)}")
    for fa in sorted(a.glob("*.unit_scale.txt")):
        fb = b / fa.name
        same = fb.exists() and fa.read_text() == fb.read_text()
        print(f"{'OK  ' if same else 'DIFF'} {fa.name}: {fa.read_text()} vs {fb.read_text() if fb.exists() else 'MISSING'}")
        bad += 0 if same else 1
    print(f"\n{'BITWISE IDENTICAL' if bad == 0 else f'{bad} DIFFERENCES'}")
    sys.exit(1 if bad else 0)

if __name__ == "__main__":
    if sys.argv[1] == "--compare": compare(sys.argv[2], sys.argv[3])
    else: dump(sys.argv[1], sys.argv[2:])
