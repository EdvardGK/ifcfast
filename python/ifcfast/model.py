"""The Model class — entry point for queries against a parsed IFC file.

A Model bundles:

* Tier-0 header (path, size, schema, authoring app, cache key)
* Tier-1 index (one row per IfcProduct: guid, entity, name, storey,
  decomposition parent, classifier mode, step id)
* Lazy data layers — psets, quantities, materials, classifications and a
  drift report. Each is a pandas DataFrame loaded on first access and
  cached on the instance.

Open a model with :func:`ifcfast.open`; iterate over products via
``model.products`` / ``model.filter()``; tap into the long-format data
tables via ``model.psets``, ``model.quantities``, ``model.materials``,
``model.classifications`` and ``model.drift``.
"""

from __future__ import annotations

import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Iterable, Optional

from .header import IFCHeader, header as _header


@dataclass
class ProductRow:
    """One IfcProduct, indexed."""

    guid: str
    entity: str
    name: Optional[str]
    predefined_type: Optional[str]
    object_type: Optional[str]
    tag: Optional[str]
    storey_guid: Optional[str]
    storey_name: Optional[str]
    parent_guid: Optional[str]
    mode: str  # 'count' / 'measure' / 'linear' / 'skip'
    step_id: int


@dataclass
class StoreyRow:
    guid: str
    name: Optional[str]
    elevation: Optional[float]
    building_guid: Optional[str]


@dataclass
class Model:
    """Parsed IFC index with lazy data-layer access.

    Tier-1 storage:
      - Eager (cold parse): ``products`` is a fully-built list of ProductRow.
      - Lazy (cache hit): ``products`` is empty and ``_products_df`` holds
        the same data as a pandas DataFrame. Filters and lookups operate
        on the DataFrame directly to keep cold-decode under 500 ms.
    """

    header: IFCHeader
    schema: str
    unit_scale: Optional[float]
    project_name: Optional[str]
    authoring_app: Optional[str]
    storeys: list[StoreyRow]
    products: list[ProductRow]
    type_counts: dict[str, int]
    parse_seconds: float

    _products_df: object = field(repr=False, default=None)
    _guid_index: Optional[dict[str, int]] = field(repr=False, default=None)
    _data_layers: Optional[object] = field(repr=False, default=None)

    # ------------------------------------------------------------------
    # Tier-1 queries
    # ------------------------------------------------------------------

    def types(self) -> dict[str, int]:
        return dict(self.type_counts)

    def __len__(self) -> int:
        if self._products_df is not None:
            return len(self._products_df)
        return len(self.products)

    def product(self, guid: str) -> Optional[ProductRow]:
        if self._products_df is not None:
            if self._guid_index is None:
                self._guid_index = {
                    g: i for i, g in enumerate(self._products_df["guid"].values)
                }
            i = self._guid_index.get(guid)
            if i is None:
                return None
            return _row_to_product(self._products_df.iloc[i])
        for p in self.products:
            if p.guid == guid:
                return p
        return None

    def filter(
        self,
        *,
        entity: Optional[str] = None,
        mode: Optional[str] = None,
        storey_guid: Optional[str] = None,
    ) -> Iterable[ProductRow]:
        if self._products_df is not None:
            df = self._products_df
            mask = None
            if entity is not None:
                mask = df["entity"] == entity
            if mode is not None:
                m2 = df["mode"] == mode
                mask = m2 if mask is None else (mask & m2)
            if storey_guid is not None:
                m3 = df["storey_guid"] == storey_guid
                mask = m3 if mask is None else (mask & m3)
            sub = df if mask is None else df[mask]
            for _, row in sub.iterrows():
                yield _row_to_product(row)
            return
        for p in self.products:
            if entity is not None and p.entity != entity:
                continue
            if mode is not None and p.mode != mode:
                continue
            if storey_guid is not None and p.storey_guid != storey_guid:
                continue
            yield p

    @property
    def products_df(self):
        """Tier-1 index as a pandas DataFrame (built on demand)."""
        import pandas as pd
        from dataclasses import asdict

        if self._products_df is not None:
            return self._products_df
        if self.products:
            self._products_df = pd.DataFrame([asdict(p) for p in self.products])
        else:
            self._products_df = pd.DataFrame(
                columns=[
                    f.name for f in ProductRow.__dataclass_fields__.values()
                ]
            )
        return self._products_df

    # ------------------------------------------------------------------
    # Lazy data layers
    # ------------------------------------------------------------------

    def _ensure_data(self):
        if self._data_layers is None:
            from .cache import extract_data_layers

            self._data_layers = extract_data_layers(self.header.path)
        return self._data_layers

    @property
    def psets(self):
        return self._ensure_data().psets

    @property
    def quantities(self):
        return self._ensure_data().quantities

    @property
    def materials(self):
        return self._ensure_data().materials

    @property
    def classifications(self):
        return self._ensure_data().classifications

    @property
    def drift(self):
        return self._ensure_data().drift


# ----------------------------------------------------------------------
# Open + tier-1 indexer
# ----------------------------------------------------------------------


def open_ifc(
    path: str | Path,
    *,
    use_cache: bool = True,
    write_cache: bool = True,
) -> Model:
    """Open an IFC file and build the tier-1 index.

    On a cache hit, returns in ~50-500 ms with no native parse. On a miss
    (or ``use_cache=False``), parses fresh via the Rust indexer and
    writes the cache.

    Data layers (psets/quantities/materials/classifications/drift) are
    lazy on the returned model.
    """
    started = time.time()
    p = Path(path)
    hdr = _header(p)

    if use_cache:
        from . import cache as _cache

        if _cache.is_index_cached(hdr):
            cached = _cache.read_index(hdr)
            if cached is not None:
                cached.parse_seconds = time.time() - started
                return cached

    model = _index_native(p, hdr, started)

    if write_cache:
        from . import cache as _cache

        try:
            _cache.write_index(model)
        except Exception as exc:
            import sys

            print(f"[ifcfast] cache write failed: {exc}", file=sys.stderr)

    return model


def _index_native(p: Path, hdr: IFCHeader, started: float) -> Model:
    """Drive the native Rust tier-1 indexer."""
    from . import _core
    from .classify import classify_by_name, ElementMode

    raw = _core.index_ifc(str(p))

    schema = raw.get("schema") or hdr.schema
    type_counts: dict[str, int] = dict(raw.get("type_counts") or {})

    # Storeys.
    storeys: list[StoreyRow] = []
    storey_step_to_guid: dict[int, str] = {}
    s = raw["storeys"]
    n_st = len(s["step_id"])
    for i in range(n_st):
        sid = int(s["step_id"][i])
        guid = s["guid"][i]
        storey_step_to_guid[sid] = guid
        storeys.append(
            StoreyRow(
                guid=guid,
                name=s["name"][i],
                elevation=s["elevation"][i],
                building_guid=None,  # patched below
            )
        )

    # Storey → building.
    bldg_ids = raw["buildings"]["step_id"]
    bldg_guids = raw["buildings"]["guid"]
    bldg_step_to_guid: dict[int, str] = {
        int(i): g for i, g in zip(bldg_ids, bldg_guids)
    }
    sb = raw["storey_building"]
    storey_step_to_building_guid: dict[int, str] = {}
    for child, building in zip(sb["storey"], sb["building"]):
        ic = int(child)
        if ic in storey_step_to_guid:
            g = bldg_step_to_guid.get(int(building))
            if g is not None:
                storey_step_to_building_guid[ic] = g
    for row, sid in zip(storeys, s["step_id"]):
        row.building_guid = storey_step_to_building_guid.get(int(sid))

    # Containment: child step_id → storey guid.
    contained_raw = raw["contained_in"]
    contained_in: dict[int, str] = {}
    for child, struct in zip(contained_raw["child"], contained_raw["structure"]):
        guid = storey_step_to_guid.get(int(struct))
        if guid is not None:
            contained_in[int(child)] = guid

    # Aggregate parent map — unified across product / storey / building /
    # site / project / space step ids.
    site_step_to_guid = {
        int(i): g for i, g in zip(raw["sites"]["step_id"], raw["sites"]["guid"])
    }
    project_step_to_guid = {
        int(i): g
        for i, g in zip(
            raw.get("projects", {}).get("step_id", []),
            raw.get("projects", {}).get("guid", []),
        )
    }
    space_step_to_guid = {
        int(i): g
        for i, g in zip(
            raw.get("spaces", {}).get("step_id", []),
            raw.get("spaces", {}).get("guid", []),
        )
    }
    prod = raw["products"]
    product_step_to_guid = {
        int(sid): guid for sid, guid in zip(prod["step_id"], prod["guid"])
    }
    parent_step_to_guid: dict[int, str] = {}
    parent_step_to_guid.update(product_step_to_guid)
    parent_step_to_guid.update(storey_step_to_guid)
    parent_step_to_guid.update(bldg_step_to_guid)
    parent_step_to_guid.update(site_step_to_guid)
    parent_step_to_guid.update(project_step_to_guid)
    parent_step_to_guid.update(space_step_to_guid)

    parent_lookup: dict[int, str] = {}
    agg = raw["aggregates"]
    for child, parent in zip(agg["child"], agg["parent"]):
        g = parent_step_to_guid.get(int(parent))
        if g is not None:
            parent_lookup[int(child)] = g

    # Storey name lookup (small list, linear scan is fine).
    storey_name_by_guid = {sr.guid: sr.name for sr in storeys}

    products: list[ProductRow] = []
    pdata = raw["products"]
    n = len(pdata["step_id"])
    for i in range(n):
        sid = int(pdata["step_id"][i])
        entity = pdata["entity"][i]
        mode = classify_by_name(entity, schema or "IFC4")
        storey_guid = contained_in.get(sid)
        products.append(
            ProductRow(
                guid=pdata["guid"][i],
                entity=entity,
                name=pdata["name"][i],
                predefined_type=pdata["predefined_type"][i],
                object_type=pdata["object_type"][i],
                tag=pdata["tag"][i],
                storey_guid=storey_guid,
                storey_name=(
                    storey_name_by_guid.get(storey_guid)
                    if storey_guid
                    else None
                ),
                parent_guid=parent_lookup.get(sid),
                mode=mode.value if isinstance(mode, ElementMode) else str(mode),
                step_id=sid,
            )
        )

    return Model(
        header=hdr,
        schema=schema or "",
        unit_scale=raw.get("unit_scale"),
        project_name=raw.get("project_name"),
        authoring_app=raw.get("authoring_app"),
        storeys=storeys,
        products=products,
        type_counts=type_counts,
        parse_seconds=time.time() - started,
    )


# ----------------------------------------------------------------------
# Helpers
# ----------------------------------------------------------------------


def _row_to_product(row) -> ProductRow:
    def _v(k):
        v = row.get(k) if hasattr(row, "get") else row[k]
        if v is None:
            return None
        try:
            import math

            if isinstance(v, float) and math.isnan(v):
                return None
        except Exception:
            pass
        return v

    return ProductRow(
        guid=_v("guid"),
        entity=_v("entity"),
        name=_v("name"),
        predefined_type=_v("predefined_type"),
        object_type=_v("object_type"),
        tag=_v("tag"),
        storey_guid=_v("storey_guid"),
        storey_name=_v("storey_name"),
        parent_guid=_v("parent_guid"),
        mode=_v("mode"),
        step_id=int(_v("step_id") or 0),
    )
