"""Parquet cache for parsed IFC indices and data layers.

Codec philosophy: encode once (the slow parse), decode many (every
subsequent open). Layout per file:

    ~/.cache/ifcfast/{cache_key}/
        meta.json              — manifest
        index.parquet          — one row per IfcProduct (tier 1)
        storeys.parquet        — one row per IfcBuildingStorey
        contained_in.parquet   — product → storey edges
        aggregates.parquet     — child → parent edges (decomposition)
        storey_building.parquet — storey → building edges
        psets.parquet          — long-format property sets
        quantities.parquet     — long-format base quantities
        materials.parquet      — material assignments
        classifications.parquet — classification references
        drift.parquet          — placement/mesh drift report (optional)

All parquet writes use zstd. Cache invalidates if cache_key, schema, or
``CACHE_VERSION`` change.
"""

from __future__ import annotations

import hashlib
import json
import os
import time
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Optional

from .header import IFCHeader

# Bumped whenever on-disk layout changes incompatibly.
# v2 (2026-05-15): added contained_in / aggregates / storey_building parquets.
# v3 (2026-05-18): added spaces.parquet + voids.parquet; SpaceRow surfaces
#                  IfcSpace as a first-class table, voids_df surfaces
#                  IfcRelVoidsElement edges.
CACHE_VERSION = 3

CONTAINED_IN_FILE = "contained_in.parquet"
AGGREGATES_FILE = "aggregates.parquet"
STOREY_BUILDING_FILE = "storey_building.parquet"
VOIDS_FILE = "voids.parquet"
SPACES_FILE = "spaces.parquet"
TYPE_OBJECTS_FILE = "type_objects.parquet"

PARQUET_COMPRESSION = "zstd"
PARQUET_COMPRESSION_LEVEL = 3


def cache_root() -> Path:
    root = os.environ.get("IFCFAST_CACHE")
    if root:
        return Path(root).expanduser()
    return Path.home() / ".cache" / "ifcfast"


def cache_dir_for(hdr: IFCHeader) -> Path:
    return cache_root() / hdr.cache_key


# ----------------------------------------------------------------------
# Manifest
# ----------------------------------------------------------------------


def _manifest_path(d: Path) -> Path:
    return d / "meta.json"


def _read_manifest(d: Path) -> Optional[dict]:
    p = _manifest_path(d)
    if not p.exists():
        return None
    try:
        return json.loads(p.read_text(encoding="utf-8"))
    except Exception:
        return None


def _write_manifest(d: Path, manifest: dict) -> None:
    d.mkdir(parents=True, exist_ok=True)
    _manifest_path(d).write_text(
        json.dumps(manifest, indent=2, sort_keys=True),
        encoding="utf-8",
    )


def _classifier_signature() -> str:
    """Hash of the classify.py entity sets. Cache invalidates on change."""
    from . import classify

    parts = []
    for setname in (
        "COUNT_ENTITIES",
        "MEASURE_ENTITIES",
        "LINEAR_ENTITIES",
        "SKIP_ENTITIES",
    ):
        s = getattr(classify, setname, set())
        parts.append(setname + ":" + ",".join(sorted(s)))
    h = hashlib.sha256("|".join(parts).encode()).hexdigest()
    return h[:12]


# ----------------------------------------------------------------------
# Index cache (tier 1)
# ----------------------------------------------------------------------


def is_index_cached(hdr: IFCHeader) -> bool:
    d = cache_dir_for(hdr)
    m = _read_manifest(d)
    if m is None:
        return False
    if m.get("cache_version") != CACHE_VERSION:
        return False
    if m.get("cache_key") != hdr.cache_key:
        return False
    if m.get("classifier_signature") != _classifier_signature():
        return False
    return (d / "index.parquet").exists()


# ----------------------------------------------------------------------
# Data layers (psets, quantities, materials, classifications, drift)
# ----------------------------------------------------------------------


PSETS_FILE = "psets.parquet"
QUANTITIES_FILE = "quantities.parquet"
MATERIALS_FILE = "materials.parquet"
CLASSIFICATIONS_FILE = "classifications.parquet"
DRIFT_FILE = "drift.parquet"
SEGMENTS_FILE = "segments.parquet"


@dataclass
class DataLayers:
    """Bundle of extractor outputs + drift report.

    Each field is None until the matching layer is computed or read from
    cache.
    """

    cache_dir: Path
    cache_key: str
    psets: Optional[object] = None           # pandas.DataFrame
    quantities: Optional[object] = None
    materials: Optional[object] = None
    classifications: Optional[object] = None
    drift: Optional[object] = None
    segments: Optional[object] = None
    timing_ms: dict = field(default_factory=dict)


def _data_file_present(d: Path, filename: str) -> bool:
    return (d / filename).exists() and (d / filename).stat().st_size > 0


def has_data_cached(hdr: IFCHeader) -> dict[str, bool]:
    d = cache_dir_for(hdr)
    return {
        "psets": _data_file_present(d, PSETS_FILE),
        "quantities": _data_file_present(d, QUANTITIES_FILE),
        "materials": _data_file_present(d, MATERIALS_FILE),
        "classifications": _data_file_present(d, CLASSIFICATIONS_FILE),
        "drift": _data_file_present(d, DRIFT_FILE),
        "segments": _data_file_present(d, SEGMENTS_FILE),
    }


def extract_data_layers(
    path: str | Path,
    *,
    use_cache: bool = True,
    write_cache: bool = True,
    include_drift: bool = True,
) -> DataLayers:
    """Materialise extractor outputs (+ optional drift) with parquet caching.

    Hot reload (cache hit on every layer): typically <200 ms even on a
    200+ MB IFC. Cold parse: a few seconds (depends on file size).
    """
    import pandas as pd

    from . import _core
    from .header import header as _header

    p = Path(path)
    hdr = _header(p)
    cache_dir = cache_dir_for(hdr)
    cache_dir.mkdir(parents=True, exist_ok=True)

    out = DataLayers(cache_dir=cache_dir, cache_key=hdr.cache_key)
    t_total = time.perf_counter()

    if use_cache:
        t0 = time.perf_counter()
        if _data_file_present(cache_dir, PSETS_FILE):
            out.psets = pd.read_parquet(cache_dir / PSETS_FILE)
        if _data_file_present(cache_dir, QUANTITIES_FILE):
            out.quantities = pd.read_parquet(cache_dir / QUANTITIES_FILE)
        if _data_file_present(cache_dir, MATERIALS_FILE):
            out.materials = pd.read_parquet(cache_dir / MATERIALS_FILE)
        if _data_file_present(cache_dir, CLASSIFICATIONS_FILE):
            out.classifications = pd.read_parquet(cache_dir / CLASSIFICATIONS_FILE)
        if include_drift and _data_file_present(cache_dir, DRIFT_FILE):
            out.drift = pd.read_parquet(cache_dir / DRIFT_FILE)
        if include_drift and _data_file_present(cache_dir, SEGMENTS_FILE):
            out.segments = pd.read_parquet(cache_dir / SEGMENTS_FILE)
        out.timing_ms["cache_read_ms"] = (time.perf_counter() - t0) * 1000

        all_cached = (
            out.psets is not None
            and out.quantities is not None
            and out.materials is not None
            and out.classifications is not None
            and (not include_drift or (out.drift is not None and out.segments is not None))
        )
        if all_cached:
            out.timing_ms["total_ms"] = (time.perf_counter() - t_total) * 1000
            out.timing_ms["cold_parse"] = False
            return out

    out.timing_ms["cold_parse"] = True

    t0 = time.perf_counter()
    raw = _core.extract_all(str(p))
    out.timing_ms["extract_all_ms"] = (time.perf_counter() - t0) * 1000
    for k in (
        "entity_table_ms",
        "guid_index_ms",
        "psets_extract_ms",
        "quantities_extract_ms",
        "materials_extract_ms",
        "classifications_extract_ms",
        "marshal_ms",
    ):
        if k in raw:
            out.timing_ms[k] = raw[k]

    t0 = time.perf_counter()
    out.psets = pd.DataFrame(raw["psets"])
    out.quantities = pd.DataFrame(raw["quantities"])
    out.materials = pd.DataFrame(raw["materials"])
    out.classifications = pd.DataFrame(raw["classifications"])
    out.timing_ms["df_build_ms"] = (time.perf_counter() - t0) * 1000

    if include_drift:
        try:
            t0 = time.perf_counter()
            drift_raw = _core.analyse_drift(str(p))
            df_cols = {
                k: drift_raw[k]
                for k in (
                    "guid", "entity", "source", "triangle_count",
                    "surface_area", "volume_abs", "aabb_volume",
                    "placement_x", "placement_y", "placement_z",
                    "centroid_x", "centroid_y", "centroid_z",
                    "drift_distance", "max_extent", "drift_ratio",
                    "drift_severity",
                    # Volume-validity classifier: "closed" / "open_shell"
                    # / "degenerate". Open-shell volumes are physically
                    # impossible (mesh isn't a closed manifold) — filter
                    # with `m.drift[m.drift.mesh_quality == "closed"]`
                    # before any volume aggregation.
                    "mesh_quality",
                )
            }
            out.drift = pd.DataFrame(df_cols)
            seg_cols = {
                "guid": drift_raw["seg_guid"],
                "product_index": drift_raw["seg_product_index"],
                "segment_index": drift_raw["seg_index"],
                "source": drift_raw["seg_source"],
                "triangle_count": drift_raw["seg_triangle_count"],
                "index_start": drift_raw["seg_index_start"],
            }
            out.segments = pd.DataFrame(seg_cols)
            out.timing_ms["drift_ms"] = (time.perf_counter() - t0) * 1000
        except AttributeError:
            # Built without the `mesh` Cargo feature.
            out.drift = None
            out.segments = None

    if write_cache:
        t0 = time.perf_counter()
        _write_data_parquets(cache_dir, out)
        _patch_data_manifest(hdr, cache_dir, out, include_drift=include_drift)
        out.timing_ms["cache_write_ms"] = (time.perf_counter() - t0) * 1000

    out.timing_ms["total_ms"] = (time.perf_counter() - t_total) * 1000
    return out


def _write_data_parquets(cache_dir: Path, out: DataLayers) -> None:
    for df, name in (
        (out.psets, PSETS_FILE),
        (out.quantities, QUANTITIES_FILE),
        (out.materials, MATERIALS_FILE),
        (out.classifications, CLASSIFICATIONS_FILE),
        (out.drift, DRIFT_FILE),
        (out.segments, SEGMENTS_FILE),
    ):
        if df is None:
            continue
        df.to_parquet(
            cache_dir / name,
            compression=PARQUET_COMPRESSION,
            compression_level=PARQUET_COMPRESSION_LEVEL,
            index=False,
        )


def _patch_data_manifest(
    hdr: IFCHeader,
    cache_dir: Path,
    out: DataLayers,
    include_drift: bool,
) -> None:
    m = _read_manifest(cache_dir) or {
        "cache_version": CACHE_VERSION,
        "classifier_signature": _classifier_signature(),
        "cache_key": hdr.cache_key,
        "source_path": hdr.path,
        "size_bytes": hdr.size_bytes,
        "mtime_ns": hdr.mtime_ns,
        "schema": hdr.schema,
    }
    if out.psets is not None:
        m["has_psets"] = True
        m["pset_count"] = int(len(out.psets))
    if out.quantities is not None:
        m["has_quantities"] = True
        m["quantity_count"] = int(len(out.quantities))
    if out.materials is not None:
        m["has_materials"] = True
        m["material_assignment_count"] = int(len(out.materials))
    if out.classifications is not None:
        m["has_classifications"] = True
        m["classification_count"] = int(len(out.classifications))
    if include_drift and out.drift is not None:
        m["has_drift"] = True
        m["drift_product_count"] = int(len(out.drift))
        m["drift_error_count"] = int(
            (out.drift["drift_severity"] == "error").sum()
        )
        m["drift_warn_count"] = int(
            (out.drift["drift_severity"] == "warn").sum()
        )
    if include_drift and out.segments is not None:
        m["has_segments"] = True
        m["segment_count"] = int(len(out.segments))
    _write_manifest(cache_dir, m)


# ----------------------------------------------------------------------
# Index parquet read/write (tier 1)
# ----------------------------------------------------------------------


def write_index(model) -> Path:
    """Write tier-1 index + storeys + manifest. Returns the cache dir."""
    import pandas as pd

    from .model import ProductRow, SpaceRow, StoreyRow, TypeObjectRow

    d = cache_dir_for(model.header)
    d.mkdir(parents=True, exist_ok=True)

    if model.products:
        df = pd.DataFrame([asdict(p) for p in model.products])
    else:
        df = pd.DataFrame(
            columns=[f.name for f in ProductRow.__dataclass_fields__.values()]
        )
    df.to_parquet(
        d / "index.parquet",
        compression=PARQUET_COMPRESSION,
        compression_level=PARQUET_COMPRESSION_LEVEL,
        index=False,
    )

    if model.storeys:
        sdf = pd.DataFrame([asdict(s) for s in model.storeys])
    else:
        sdf = pd.DataFrame(
            columns=[f.name for f in StoreyRow.__dataclass_fields__.values()]
        )
    sdf.to_parquet(
        d / "storeys.parquet",
        compression=PARQUET_COMPRESSION,
        compression_level=PARQUET_COMPRESSION_LEVEL,
        index=False,
    )

    # Relationship tables. Each is a small long-format DataFrame; even on
    # a 200K-product file they sum to <500 KB compressed.
    for df, name in (
        (model._contained_in_df, CONTAINED_IN_FILE),
        (model._aggregates_df, AGGREGATES_FILE),
        (model._storey_building_df, STOREY_BUILDING_FILE),
        (model._voids_df, VOIDS_FILE),
    ):
        if df is None:
            continue
        df.to_parquet(
            d / name,
            compression=PARQUET_COMPRESSION,
            compression_level=PARQUET_COMPRESSION_LEVEL,
            index=False,
        )

    # Spaces — tier-1 entities, written even when empty so the manifest
    # accurately reflects "the parser saw zero IfcSpace" vs "the cache is
    # too old to have a spaces table".
    if model.spaces:
        spdf = pd.DataFrame([asdict(s) for s in model.spaces])
    else:
        spdf = pd.DataFrame(
            columns=[f.name for f in SpaceRow.__dataclass_fields__.values()]
        )
    spdf.to_parquet(
        d / SPACES_FILE,
        compression=PARQUET_COMPRESSION,
        compression_level=PARQUET_COMPRESSION_LEVEL,
        index=False,
    )

    if model.type_objects:
        todf = pd.DataFrame([asdict(t) for t in model.type_objects])
    else:
        todf = pd.DataFrame(
            columns=[
                f.name for f in TypeObjectRow.__dataclass_fields__.values()
            ]
        )
    todf.to_parquet(
        d / TYPE_OBJECTS_FILE,
        compression=PARQUET_COMPRESSION,
        compression_level=PARQUET_COMPRESSION_LEVEL,
        index=False,
    )

    m = _read_manifest(d) or {}
    m.update({
        "cache_version": CACHE_VERSION,
        "classifier_signature": _classifier_signature(),
        "cache_key": model.header.cache_key,
        "source_path": model.header.path,
        "size_bytes": model.header.size_bytes,
        "mtime_ns": model.header.mtime_ns,
        "schema": model.schema,
        "project_name": model.project_name,
        "authoring_app": model.authoring_app,
        "unit_scale": model.unit_scale,
        "product_count": len(model.products),
        "storey_count": len(model.storeys),
        "type_counts": model.type_counts,
        "encoded_at": time.time(),
        "has_index": True,
        "has_storeys": True,
        "has_contained_in": model._contained_in_df is not None,
        "has_aggregates": model._aggregates_df is not None,
        "has_storey_building": model._storey_building_df is not None,
        "has_voids": model._voids_df is not None,
        "has_spaces": True,
        "space_count": len(model.spaces),
        "contained_in_count": (
            int(len(model._contained_in_df))
            if model._contained_in_df is not None else 0
        ),
        "aggregates_count": (
            int(len(model._aggregates_df))
            if model._aggregates_df is not None else 0
        ),
        "voids_count": (
            int(len(model._voids_df))
            if model._voids_df is not None else 0
        ),
    })
    _write_manifest(d, m)
    return d


def read_index(hdr: IFCHeader):
    """Reconstruct a Model from cached parquet. Returns None on miss."""
    import pandas as pd

    from .model import Model, SpaceRow, StoreyRow, TypeObjectRow

    d = cache_dir_for(hdr)
    m = _read_manifest(d)
    if m is None or m.get("cache_version") != CACHE_VERSION:
        return None
    if m.get("cache_key") != hdr.cache_key:
        return None
    if m.get("classifier_signature") != _classifier_signature():
        return None
    idx_path = d / "index.parquet"
    sty_path = d / "storeys.parquet"
    if not idx_path.exists():
        return None

    started = time.time()
    df = pd.read_parquet(idx_path)

    storeys: list[StoreyRow] = []
    if sty_path.exists():
        sdf = pd.read_parquet(sty_path)
        for row in sdf.to_dict(orient="records"):
            storeys.append(
                StoreyRow(**{k: _none_if_nan(v) for k, v in row.items()})
            )

    contained_in_df = None
    aggregates_df = None
    storey_building_df = None
    voids_df = None
    if (d / CONTAINED_IN_FILE).exists():
        contained_in_df = pd.read_parquet(d / CONTAINED_IN_FILE)
    if (d / AGGREGATES_FILE).exists():
        aggregates_df = pd.read_parquet(d / AGGREGATES_FILE)
    if (d / STOREY_BUILDING_FILE).exists():
        storey_building_df = pd.read_parquet(d / STOREY_BUILDING_FILE)
    if (d / VOIDS_FILE).exists():
        voids_df = pd.read_parquet(d / VOIDS_FILE)

    spaces: list[SpaceRow] = []
    sp_path = d / SPACES_FILE
    if sp_path.exists():
        spdf = pd.read_parquet(sp_path)
        for row in spdf.to_dict(orient="records"):
            spaces.append(
                SpaceRow(**{k: _none_if_nan(v) for k, v in row.items()})
            )

    type_objects: list[TypeObjectRow] = []
    to_path = d / TYPE_OBJECTS_FILE
    if to_path.exists():
        todf = pd.read_parquet(to_path)
        for row in todf.to_dict(orient="records"):
            type_objects.append(
                TypeObjectRow(**{k: _none_if_nan(v) for k, v in row.items()})
            )

    return Model(
        header=hdr,
        schema=m.get("schema", ""),
        unit_scale=m.get("unit_scale"),
        project_name=m.get("project_name"),
        authoring_app=m.get("authoring_app"),
        storeys=storeys,
        products=[],
        spaces=spaces,
        type_objects=type_objects,
        type_counts=dict(m.get("type_counts", {})),
        parse_seconds=time.time() - started,
        _products_df=df,
        _contained_in_df=contained_in_df,
        _aggregates_df=aggregates_df,
        _storey_building_df=storey_building_df,
        _voids_df=voids_df,
    )


def _none_if_nan(v):
    try:
        import math

        if isinstance(v, float) and math.isnan(v):
            return None
    except Exception:
        pass
    return v
