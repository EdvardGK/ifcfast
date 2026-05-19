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
    """One IfcProduct, indexed.

    Type linkage (``type_guid`` / ``type_name`` / ``type_source``) comes
    from one of three sources, in order of preference:

    * ``"ifctype"``    — formal ``IfcRelDefinesByType`` link to an
                         ``IfcTypeObject``. Strongest.
    * ``"objecttype"`` — only ``IfcRoot.ObjectType`` is populated (no
                         formal type relation). Common Revit export
                         pattern. Use ``type_name`` for display, but
                         downstream consumers expecting an
                         ``IfcTypeObject`` GUID will see ``None``.
    * ``"none"``       — neither.
    """

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
    type_guid: Optional[str] = None
    type_name: Optional[str] = None
    type_source: str = "none"  # 'ifctype' / 'objecttype' / 'none'


@dataclass
class StoreyRow:
    guid: str
    name: Optional[str]
    elevation: Optional[float]
    building_guid: Optional[str]


@dataclass
class SpaceRow:
    """One IfcSpace, indexed.

    Spaces are kept in their own collection rather than as IfcProduct
    subclasses so that ``m.products`` stays "things you build" and
    ``m.spaces`` stays "rooms / zones". Both extracted by the Rust
    indexer in a single pass.
    """

    guid: str
    step_id: int


@dataclass
class TypeObjectRow:
    """One ``IfcTypeObject`` (or any ``IfcXxxType`` subclass).

    Captured so ``IfcRelDefinesByType`` can be fully resolved by the
    parser without an ifcopenshell sidecar. The Rust indexer picks these
    up via a byte-suffix match on ``IFC*TYPE``.
    """

    guid: str
    entity: str
    name: Optional[str]
    step_id: int


@dataclass
class Model:
    """Parsed IFC index with lazy data-layer access.

    Tier-1 storage:
      - Eager (cold parse): ``products`` is a fully-built list of ProductRow.
      - Lazy (cache hit): ``products`` is empty and ``_products_df`` holds
        the same data as a pandas DataFrame. Filters and lookups operate
        on the DataFrame directly to keep cold-decode under 500 ms.

    Relationship tables (``contained_in`` / ``aggregates`` /
    ``storey_building``) are long-format DataFrames built alongside the
    product index. The traversal helpers (``parent`` / ``children`` /
    ``ancestors`` / ``descendants`` / ``storey_of`` / ``building_of`` /
    ``products_in``) lazily compute inverse maps from those tables on
    first access.
    """

    header: IFCHeader
    schema: str
    unit_scale: Optional[float]
    project_name: Optional[str]
    authoring_app: Optional[str]
    storeys: list[StoreyRow]
    products: list[ProductRow]
    spaces: list[SpaceRow]
    type_objects: list[TypeObjectRow]
    type_counts: dict[str, int]
    parse_seconds: float

    # All of these are typed as Optional[pandas.DataFrame] in practice;
    # using `object` keeps the import-graph cheap (model.py shouldn't
    # force pandas at import time).
    _products_df: Optional["object"] = field(repr=False, default=None)
    _guid_index: Optional[dict[str, int]] = field(repr=False, default=None)
    _data_layers: Optional["object"] = field(repr=False, default=None)
    _contained_in_df: Optional["object"] = field(repr=False, default=None)
    _aggregates_df: Optional["object"] = field(repr=False, default=None)
    _storey_building_df: Optional["object"] = field(repr=False, default=None)
    _voids_df: Optional["object"] = field(repr=False, default=None)
    _spaces_df: Optional["object"] = field(repr=False, default=None)
    _type_objects_df: Optional["object"] = field(repr=False, default=None)
    _graph: Optional["object"] = field(repr=False, default=None)

    # ------------------------------------------------------------------
    # Tier-1 queries
    # ------------------------------------------------------------------

    def types(self) -> dict[str, int]:
        return dict(self.type_counts)

    def by_type(self, entity: str) -> list[ProductRow]:
        """All products of a given entity type.

        Mirrors ``ifcopenshell.file.by_type(entity)`` — the single most
        common ifcopenshell pattern. Drop-in replacement; matches case
        on the canonical title-case name (e.g. ``"IfcWall"``,
        ``"IfcWallStandardCase"``).
        """
        return list(self.filter(entity=entity))

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

    @property
    def segments(self):
        """Long-format per-mesh-segment table — one row per representation
        item that contributed triangles. For an ``IfcWall`` whose body is
        a single extrusion this is one row; for an ``IfcBooleanResult``
        each operand contributes its own row, so the consumer can colour,
        filter, or split by structural role.

        Columns: ``guid`` (parent product), ``product_index`` (row index
        in ``drift``), ``segment_index`` (segment ordinal within the
        product), ``source`` (compound ``role|leaf`` tag such as
        ``boolean_second_operand|halfspace_bounded``), ``triangle_count``,
        ``index_start`` (first index into the product's triangle list
        belonging to this segment).
        """
        return self._ensure_data().segments

    # ------------------------------------------------------------------
    # Spatial hierarchy & relationships
    # ------------------------------------------------------------------

    @property
    def contained_in(self):
        """Long-format ``IfcRelContainedInSpatialStructure`` edges.

        DataFrame with columns ``product_guid`` and ``storey_guid`` —
        one row per (product, storey) containment. Empty DataFrame if
        the source IFC had no spatial containment.
        """
        import pandas as pd

        if self._contained_in_df is None:
            self._contained_in_df = pd.DataFrame(
                columns=["product_guid", "storey_guid"]
            )
        return self._contained_in_df

    @property
    def aggregates(self):
        """Long-format ``IfcRelAggregates`` edges.

        DataFrame with columns ``child_guid``, ``parent_guid``,
        ``parent_kind`` — one row per decomposition relationship.
        ``parent_kind`` is one of ``product`` / ``storey`` / ``building``
        / ``site`` / ``project`` / ``space``.
        """
        import pandas as pd

        if self._aggregates_df is None:
            self._aggregates_df = pd.DataFrame(
                columns=["child_guid", "parent_guid", "parent_kind"]
            )
        return self._aggregates_df

    @property
    def storey_building(self):
        """Storey → building edges as a DataFrame.

        Columns: ``storey_guid``, ``building_guid``. One row per
        (storey, building) pair derived from aggregates.
        """
        import pandas as pd

        if self._storey_building_df is None:
            self._storey_building_df = pd.DataFrame(
                columns=["storey_guid", "building_guid"]
            )
        return self._storey_building_df

    @property
    def voids(self):
        """Long-format ``IfcRelVoidsElement`` edges.

        DataFrame with columns ``opening_guid`` and ``host_guid`` —
        one row per (opening, host) relation. Openings are typically
        ``IfcOpeningElement`` instances; hosts are walls, slabs, doors,
        windows etc. Empty DataFrame if the source IFC declared no
        voids.

        Use this to put openings back in the spatial tree: an opening
        belongs to its host, not to any storey directly.
        """
        import pandas as pd

        if self._voids_df is None:
            self._voids_df = pd.DataFrame(
                columns=["opening_guid", "host_guid"]
            )
        return self._voids_df

    @property
    def spaces_df(self):
        """Tier-1 space index as a pandas DataFrame.

        Columns: ``guid``, ``step_id``. Built from ``self.spaces`` on
        first access. Mirrors ``products_df`` for spaces.
        """
        import pandas as pd
        from dataclasses import asdict

        if self._spaces_df is not None:
            return self._spaces_df
        if self.spaces:
            self._spaces_df = pd.DataFrame([asdict(s) for s in self.spaces])
        else:
            self._spaces_df = pd.DataFrame(
                columns=[
                    f.name for f in SpaceRow.__dataclass_fields__.values()
                ]
            )
        return self._spaces_df

    @property
    def type_objects_df(self):
        """``IfcTypeObject`` table as a pandas DataFrame.

        Columns: ``guid``, ``entity``, ``name``, ``step_id``. One row per
        ``IfcXxxType`` instance in the file. Used by the indexer to
        resolve ``IfcRelDefinesByType`` per-product; exposed here so QA
        scripts can audit the type catalogue directly.
        """
        import pandas as pd
        from dataclasses import asdict

        if self._type_objects_df is not None:
            return self._type_objects_df
        if self.type_objects:
            self._type_objects_df = pd.DataFrame(
                [asdict(t) for t in self.type_objects]
            )
        else:
            self._type_objects_df = pd.DataFrame(
                columns=[
                    f.name for f in TypeObjectRow.__dataclass_fields__.values()
                ]
            )
        return self._type_objects_df

    def _graph_index(self) -> "_GraphIndex":
        if self._graph is None:
            if self._products_df is not None:
                product_guids = set(self._products_df["guid"].values)
            else:
                product_guids = {p.guid for p in self.products}
            self._graph = _GraphIndex(
                self.contained_in,
                self.aggregates,
                self.storey_building,
                product_guids,
            )
        return self._graph

    def parent(self, guid: str) -> Optional[str]:
        """Unified parent guid.

        Returns the ``IfcRelAggregates`` parent if one exists (typical
        for assemblies and the spatial chain storey→building→site→
        project). Otherwise falls back to the spatial container — the
        storey a product is in. ``None`` if neither.
        """
        g = self._graph_index()
        p = g.parent_of.get(guid)
        if p is not None:
            return p
        return g.storey_of.get(guid)

    def children(self, guid: str) -> list[str]:
        """Unified direct children.

        For spatial containers (storeys), returns the contained
        products plus any aggregate children (e.g., IfcSpaces) — products
        first to keep ordering stable. For aggregates (buildings, sites,
        projects, assemblies), returns the ``IfcRelAggregates`` children.
        Empty if there are no children or the guid is unknown.
        """
        g = self._graph_index()
        spatial_kids = g.products_in.get(guid, ())
        agg_kids = g.children_of.get(guid, ())
        if not spatial_kids:
            return list(agg_kids)
        if not agg_kids:
            return list(spatial_kids)
        seen: set[str] = set(spatial_kids)
        out: list[str] = list(spatial_kids)
        for k in agg_kids:
            if k not in seen:
                seen.add(k)
                out.append(k)
        return out

    def ancestors(self, guid: str) -> list[str]:
        """Chain from ``guid`` to root, exclusive of ``guid``.

        Walks the unified parent chain (aggregates first, falling back
        to the spatial containment hop product→storey). Order: nearest
        parent first, project (or whatever the chain ends at) last.
        Cycle-safe — repeats short-circuit the walk.
        """
        out: list[str] = []
        seen: set[str] = set()
        cur = self.parent(guid)
        while cur is not None and cur not in seen:
            seen.add(cur)
            out.append(cur)
            cur = self.parent(cur)
        return out

    def descendants(self, guid: str) -> list[str]:
        """BFS over the unified-children tree under ``guid``, exclusive of ``guid``.

        Walks both ``IfcRelAggregates`` (decomposition) and
        ``IfcRelContainedInSpatialStructure`` (storey → products) so a
        single call on a project guid yields the whole model.
        """
        out: list[str] = []
        seen: set[str] = set()
        queue: list[str] = list(self.children(guid))
        for ch in queue:
            seen.add(ch)
        head = 0
        while head < len(queue):
            cur = queue[head]
            head += 1
            out.append(cur)
            for ch in self.children(cur):
                if ch not in seen:
                    seen.add(ch)
                    queue.append(ch)
        return out

    def storey_of(self, guid: str) -> Optional[str]:
        """Storey guid that contains ``guid`` (via spatial containment)."""
        return self._graph_index().storey_of.get(guid)

    def building_of(self, guid: str) -> Optional[str]:
        """Building guid for a product, storey, or itself.

        Resolution order: storey→building map (fastest), then walk
        ancestors looking for a guid that is itself a building.
        """
        g = self._graph_index()
        if guid in g.storeys_in:  # already a building
            return guid
        if guid in g.building_of:  # guid is a storey
            return g.building_of[guid]
        s = g.storey_of.get(guid)
        if s is not None and s in g.building_of:
            return g.building_of[s]
        for a in self.ancestors(guid):
            if a in g.storeys_in:
                return a
        return None

    def products_in(self, parent_guid: str) -> list[str]:
        """Product guids under ``parent_guid``.

        Walks the unified-children tree (aggregates + spatial
        containment) and returns just the guids that are products
        according to the tier-1 index. Order: BFS from ``parent_guid``.

        Examples: ``products_in(storey)`` is the contained products;
        ``products_in(building)`` is all products in all storeys of the
        building; ``products_in(project)`` covers the whole model.
        """
        g = self._graph_index()
        # Fast path: storey with directly-contained products only.
        if parent_guid in g.products_in and not g.children_of.get(parent_guid):
            return [pg for pg in g.products_in[parent_guid] if pg in g.product_guids]
        out: list[str] = []
        for guid in self.descendants(parent_guid):
            if guid in g.product_guids:
                out.append(guid)
        return out

    # ------------------------------------------------------------------
    # Agent-facing introspection
    # ------------------------------------------------------------------

    def summary(self) -> dict:
        """Single-call snapshot: everything an agent needs to plan.

        Returns a plain JSON-friendly dict with the model's identity,
        counts, top entity types, and the shape + loaded-state of every
        available table. Cheap — no data-layer extraction is triggered;
        only the already-built tier-1 index and relationship tables are
        inspected.

        Pattern: an agent calls ``m.summary()`` first, decides which
        tables it needs (``m.psets``? ``m.contained_in``?), then asks
        for them. Avoids paying a multi-second extract just to "peek".
        """
        top_types = sorted(self.type_counts.items(), key=lambda kv: -kv[1])
        tables: dict[str, dict] = {
            "products": {
                "rows": len(self),
                "columns": [
                    f.name for f in ProductRow.__dataclass_fields__.values()
                ],
                "loaded": True,
            },
            "storeys": {
                "rows": len(self.storeys),
                "columns": [
                    f.name for f in StoreyRow.__dataclass_fields__.values()
                ],
                "loaded": True,
            },
            "spaces": {
                "rows": len(self.spaces),
                "columns": [
                    f.name for f in SpaceRow.__dataclass_fields__.values()
                ],
                "loaded": True,
            },
            "type_objects": {
                "rows": len(self.type_objects),
                "columns": [
                    f.name for f in TypeObjectRow.__dataclass_fields__.values()
                ],
                "loaded": True,
            },
            "contained_in": _df_meta(self._contained_in_df),
            "aggregates": _df_meta(self._aggregates_df),
            "storey_building": _df_meta(self._storey_building_df),
            "voids": _df_meta(self._voids_df),
        }
        for name in ("psets", "quantities", "materials", "classifications", "drift", "segments"):
            tables[name] = _data_layer_meta(self._data_layers, name)

        return {
            "path": str(self.header.path),
            "size_bytes": getattr(self.header, "size_bytes", None),
            "schema": self.schema,
            "project_name": self.project_name,
            "authoring_app": self.authoring_app,
            "unit_scale": self.unit_scale,
            "cache_key": getattr(self.header, "cache_key", None),
            "products": len(self),
            "storeys": len(self.storeys),
            "type_counts_total": len(self.type_counts),
            "top_types": dict(top_types[:20]),
            "tables": tables,
            "parse_seconds": self.parse_seconds,
        }

    @property
    def schemas(self) -> dict:
        """Column-level introspection of every table on the model.

        Returns ``{table_name: {columns: [...], dtypes: {col: dtype},
        rows: int, loaded: bool}}`` for every table. Useful when an
        agent wants to plan a pandas operation without running
        ``df.head()`` (which on the lazy layers triggers extract).
        """
        out: dict[str, dict] = {
            "products": _df_schema_from_dataclass(ProductRow, rows=len(self)),
            "storeys": _df_schema_from_dataclass(StoreyRow, rows=len(self.storeys)),
            "spaces": _df_schema_from_dataclass(SpaceRow, rows=len(self.spaces)),
            "type_objects": _df_schema_from_dataclass(
                TypeObjectRow, rows=len(self.type_objects)
            ),
            "contained_in": _df_schema(self._contained_in_df),
            "aggregates": _df_schema(self._aggregates_df),
            "storey_building": _df_schema(self._storey_building_df),
            "voids": _df_schema(self._voids_df),
        }
        for name in ("psets", "quantities", "materials", "classifications", "drift", "segments"):
            df = (
                getattr(self._data_layers, name, None)
                if self._data_layers is not None
                else None
            )
            out[name] = _df_schema(df, loaded=df is not None)
        return out

    def type_summary(self, *, sample_guids: int = 3) -> list[dict]:
        """Type-first view of the model — one dict per IFC entity type.

        Returns a list of ``{entity, count, storeys, predefined_types,
        object_types, sample_guids}`` sorted by descending count.

        Designed for type-centric workflows like sprucelab's TypeBank:
        types are the unit of coordination ("50,000 entities, only 300-
        500 unique types"). All fields derive from the already-built
        tier-1 index + spatial graph — no data-layer extraction, no
        materials/classification cost. Add those via ``type_bank()``
        when you actually need them.
        """
        from collections import defaultdict

        per_type: dict[str, dict] = defaultdict(
            lambda: {
                "count": 0,
                "storeys": set(),
                "predefined_types": set(),
                "object_types": set(),
                "sample_guids": [],
            }
        )

        if self._products_df is not None:
            it = self._products_df.itertuples(index=False)
            fields = list(self._products_df.columns)
            for row in it:
                rec = dict(zip(fields, row))
                _accumulate_type(per_type, rec, sample_guids)
        else:
            for p in self.products:
                rec = {
                    "entity": p.entity,
                    "guid": p.guid,
                    "predefined_type": p.predefined_type,
                    "object_type": p.object_type,
                    "storey_name": p.storey_name,
                    "storey_guid": p.storey_guid,
                }
                _accumulate_type(per_type, rec, sample_guids)

        out: list[dict] = []
        for entity, agg in per_type.items():
            out.append({
                "entity": entity,
                "count": agg["count"],
                "storeys": sorted(s for s in agg["storeys"] if s),
                "predefined_types": sorted(
                    t for t in agg["predefined_types"] if t
                ),
                "object_types": sorted(
                    t for t in agg["object_types"] if t
                ),
                "sample_guids": agg["sample_guids"],
            })
        out.sort(key=lambda r: (-r["count"], r["entity"]))
        return out

    def type_bank(self, *, sample_guids: int = 3) -> list[dict]:
        """Superset of ``type_summary()`` with materials + classifications.

        Triggers the lazy materials and classifications extracts on
        first call (one-time ~150 ms per layer on a 200 MB IFC, cached
        afterwards). Each row gains ``materials`` (sorted unique
        names) and ``classifications`` (sorted unique
        ``system:identification`` pairs).

        Shape designed for sprucelab's TypeBank: drop this output into
        a Django bulk_create and you have your type catalogue.
        """
        rows = self.type_summary(sample_guids=sample_guids)
        by_entity = {r["entity"]: r for r in rows}
        # Build product_guid → entity lookup once for the join below.
        guid_to_entity: dict[str, str] = {}
        if self._products_df is not None:
            for guid, ent in zip(
                self._products_df["guid"].values,
                self._products_df["entity"].values,
            ):
                guid_to_entity[guid] = ent
        else:
            for p in self.products:
                guid_to_entity[p.guid] = p.entity

        mats = self.materials  # triggers lazy extract
        if mats is not None and len(mats) > 0:
            from collections import defaultdict
            mat_by_entity: dict[str, set] = defaultdict(set)
            for guid, name in zip(mats["guid"].values, mats["material_name"].values):
                ent = guid_to_entity.get(guid)
                if ent is None or name is None:
                    continue
                if isinstance(name, float):  # NaN
                    continue
                mat_by_entity[ent].add(str(name))
            for entity, names in mat_by_entity.items():
                if entity in by_entity:
                    by_entity[entity]["materials"] = sorted(names)
        for row in rows:
            row.setdefault("materials", [])

        cls = self.classifications  # triggers lazy extract
        if cls is not None and len(cls) > 0:
            from collections import defaultdict
            cls_by_entity: dict[str, set] = defaultdict(set)
            for guid, system, ident in zip(
                cls["guid"].values,
                cls["system_name"].values,
                cls["identification"].values,
            ):
                ent = guid_to_entity.get(guid)
                if ent is None:
                    continue
                sys_s = "" if system is None or isinstance(system, float) else str(system)
                id_s = "" if ident is None or isinstance(ident, float) else str(ident)
                if not (sys_s or id_s):
                    continue
                cls_by_entity[ent].add(f"{sys_s}:{id_s}")
            for entity, refs in cls_by_entity.items():
                if entity in by_entity:
                    by_entity[entity]["classifications"] = sorted(refs)
        for row in rows:
            row.setdefault("classifications", [])

        return rows

    def diff(self, other: "Model | str", *, sample: int = 5) -> dict:
        """Compare this model against another (or against a path).

        Returns a JSON-friendly dict::

            {
                "left":  {"path": ..., "schema": ..., "products": N},
                "right": {"path": ..., "schema": ..., "products": M},
                "products": {
                    "added":     [guid, ...],     # in right, not in left
                    "removed":   [guid, ...],     # in left,  not in right
                    "kept":      int,             # count
                    "changed":   [
                        {"guid": ..., "entity": ..., "fields":
                          {"name": ["old", "new"], "predefined_type": ...}},
                        ...
                    ],
                },
                "type_deltas": {
                    "IfcWall":   {"left": 12, "right": 14, "delta":  2},
                    "IfcDoor":   {"left":  3, "right":  3, "delta":  0},
                    ...
                },
                "storey_deltas": [
                    {"guid": ..., "name": ..., "elevation": [old, new]},
                    ...
                ],
            }

        Designed for "what changed since v3?" feature surfaces. Lists
        are truncated to ``sample`` for the ``added``/``removed``/
        ``changed`` arrays in pretty/JSON output; counts are always
        exact. Set ``sample=None`` (or 0) to keep full lists.
        """
        if isinstance(other, str):
            other_model = open_ifc(other)
        else:
            other_model = other

        # Build guid → product-row dicts on both sides.
        left = _index_products_by_guid(self)
        right = _index_products_by_guid(other_model)
        left_guids = set(left.keys())
        right_guids = set(right.keys())
        added = sorted(right_guids - left_guids)
        removed = sorted(left_guids - right_guids)
        kept = left_guids & right_guids

        changed: list[dict] = []
        watched_fields = (
            "entity", "name", "predefined_type", "object_type",
            "tag", "storey_name", "storey_guid",
        )
        for guid in kept:
            l, r = left[guid], right[guid]
            field_changes: dict[str, list] = {}
            for f in watched_fields:
                lv, rv = l.get(f), r.get(f)
                if lv != rv:
                    field_changes[f] = [lv, rv]
            if field_changes:
                changed.append({
                    "guid": guid,
                    "entity": r.get("entity") or l.get("entity"),
                    "fields": field_changes,
                })

        # Type cardinality deltas.
        type_deltas: dict[str, dict] = {}
        all_types = set(self.type_counts) | set(other_model.type_counts)
        for t in sorted(all_types):
            lc = int(self.type_counts.get(t, 0))
            rc = int(other_model.type_counts.get(t, 0))
            if lc != rc:
                type_deltas[t] = {"left": lc, "right": rc, "delta": rc - lc}

        # Storey elevation deltas (matched on guid).
        l_storeys = {s.guid: s for s in self.storeys}
        r_storeys = {s.guid: s for s in other_model.storeys}
        storey_deltas: list[dict] = []
        for guid in l_storeys.keys() & r_storeys.keys():
            ls, rs = l_storeys[guid], r_storeys[guid]
            if ls.elevation != rs.elevation or ls.name != rs.name:
                storey_deltas.append({
                    "guid": guid,
                    "name": [ls.name, rs.name],
                    "elevation": [ls.elevation, rs.elevation],
                })

        def _trim(lst, n):
            if not n:
                return lst
            return lst[: n]

        return {
            "left": {
                "path": str(self.header.path),
                "schema": self.schema,
                "products": len(self),
            },
            "right": {
                "path": str(other_model.header.path),
                "schema": other_model.schema,
                "products": len(other_model),
            },
            "products": {
                "added": _trim(added, sample),
                "added_count": len(added),
                "removed": _trim(removed, sample),
                "removed_count": len(removed),
                "kept": len(kept),
                "changed": _trim(changed, sample),
                "changed_count": len(changed),
            },
            "type_deltas": type_deltas,
            "storey_deltas": storey_deltas,
        }

    def preview(self, table: str, n: int = 5) -> list[dict]:
        """Sample rows from any table as a plain list-of-dicts.

        Supported tables: ``products`` / ``storeys`` / ``contained_in``
        / ``aggregates`` / ``storey_building`` / ``psets`` /
        ``quantities`` / ``materials`` / ``classifications`` /
        ``drift`` / ``segments``. Triggers lazy extraction for the four
        data layers, drift, and the per-product mesh segments table;
        pure DataFrame slice for the rest. Returns ``[]`` for an
        unknown table or one that's empty.
        """
        from dataclasses import asdict

        if table == "products":
            rows: list[dict] = []
            if self._products_df is not None:
                for row in self._products_df.head(n).to_dict(orient="records"):
                    rows.append({k: _none_if_nan_simple(v) for k, v in row.items()})
            else:
                for p in self.products[:n]:
                    rows.append(asdict(p))
            return rows
        if table == "storeys":
            return [asdict(s) for s in self.storeys[:n]]
        if table == "spaces":
            return [asdict(s) for s in self.spaces[:n]]
        if table == "type_objects":
            return [asdict(t) for t in self.type_objects[:n]]
        df_attr = {
            "contained_in": "_contained_in_df",
            "aggregates": "_aggregates_df",
            "storey_building": "_storey_building_df",
            "voids": "_voids_df",
        }.get(table)
        if df_attr is not None:
            df = getattr(self, df_attr)
            if df is None or len(df) == 0:
                return []
            return df.head(n).to_dict(orient="records")
        # Lazy fall-through for the materialised properties that may not
        # have been touched yet (voids/spaces_df). Letting the property
        # build them on demand keeps preview() honest about emptiness.
        if table in {"voids"} and df_attr is not None and getattr(self, df_attr) is None:
            self.voids  # trigger build
            df = getattr(self, df_attr)
            if df is not None and len(df) > 0:
                return df.head(n).to_dict(orient="records")
            return []
        if table in {"psets", "quantities", "materials", "classifications", "drift", "segments"}:
            df = getattr(self, table)  # triggers extract for data layers
            if df is None or len(df) == 0:
                return []
            rows = []
            for row in df.head(n).to_dict(orient="records"):
                rows.append({k: _none_if_nan_simple(v) for k, v in row.items()})
            return rows
        return []


# ----------------------------------------------------------------------
# Lazy inverse-map index for traversal helpers
# ----------------------------------------------------------------------


class _GraphIndex:
    """Built once on first traversal-method access, then cached.

    Single pass over the three relationship DataFrames; O(N) memory.
    """

    __slots__ = (
        "parent_of",
        "children_of",
        "storey_of",
        "products_in",
        "building_of",
        "storeys_in",
        "product_guids",
    )

    def __init__(self, contained_in, aggregates, storey_building, product_guids):
        self.parent_of: dict[str, str] = {}
        self.children_of: dict[str, list[str]] = {}
        self.storey_of: dict[str, str] = {}
        self.products_in: dict[str, list[str]] = {}
        self.building_of: dict[str, str] = {}
        self.storeys_in: dict[str, list[str]] = {}
        self.product_guids: set[str] = product_guids

        if len(aggregates) > 0:
            for child, parent in zip(
                aggregates["child_guid"].values,
                aggregates["parent_guid"].values,
            ):
                if child is None or parent is None:
                    continue
                self.parent_of[child] = parent
                self.children_of.setdefault(parent, []).append(child)

        if len(contained_in) > 0:
            for product, storey in zip(
                contained_in["product_guid"].values,
                contained_in["storey_guid"].values,
            ):
                if product is None or storey is None:
                    continue
                self.storey_of[product] = storey
                self.products_in.setdefault(storey, []).append(product)

        if len(storey_building) > 0:
            for storey, building in zip(
                storey_building["storey_guid"].values,
                storey_building["building_guid"].values,
            ):
                if storey is None or building is None:
                    continue
                self.building_of[storey] = building
                self.storeys_in.setdefault(building, []).append(storey)


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
    storey_building_pairs: list[tuple[str, str]] = []
    for child, building in zip(sb["storey"], sb["building"]):
        ic = int(child)
        if ic in storey_step_to_guid:
            g = bldg_step_to_guid.get(int(building))
            if g is not None:
                storey_step_to_building_guid[ic] = g
                storey_building_pairs.append((storey_step_to_guid[ic], g))
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
    # Step-id → (guid, kind). Build with deliberate precedence so the
    # "most specific" kind wins if a step id appears in multiple sources
    # (shouldn't happen in valid IFC but be defensive).
    parent_step_to_guid: dict[int, str] = {}
    parent_kind_by_step: dict[int, str] = {}
    for sid, g in space_step_to_guid.items():
        parent_step_to_guid[sid] = g
        parent_kind_by_step[sid] = "space"
    for sid, g in site_step_to_guid.items():
        parent_step_to_guid[sid] = g
        parent_kind_by_step[sid] = "site"
    for sid, g in project_step_to_guid.items():
        parent_step_to_guid[sid] = g
        parent_kind_by_step[sid] = "project"
    for sid, g in bldg_step_to_guid.items():
        parent_step_to_guid[sid] = g
        parent_kind_by_step[sid] = "building"
    for sid, g in storey_step_to_guid.items():
        parent_step_to_guid[sid] = g
        parent_kind_by_step[sid] = "storey"
    for sid, g in product_step_to_guid.items():
        parent_step_to_guid[sid] = g
        parent_kind_by_step[sid] = "product"

    parent_lookup: dict[int, str] = {}
    aggregates_rows: list[tuple[str, str, str]] = []
    agg = raw["aggregates"]
    for child, parent in zip(agg["child"], agg["parent"]):
        parent_sid = int(parent)
        child_sid = int(child)
        pguid = parent_step_to_guid.get(parent_sid)
        cguid = parent_step_to_guid.get(child_sid)
        if pguid is None or cguid is None:
            continue
        parent_lookup[child_sid] = pguid
        aggregates_rows.append(
            (cguid, pguid, parent_kind_by_step.get(parent_sid, "unknown"))
        )

    # Build the long-format containment table. Use product-step-to-guid
    # to filter out the rare case where a contained child isn't in our
    # product table (e.g. an IfcSpace that ifcfast doesn't index).
    contained_in_rows: list[tuple[str, str]] = []
    for child, struct in zip(contained_raw["child"], contained_raw["structure"]):
        s_guid = storey_step_to_guid.get(int(struct))
        c_guid = product_step_to_guid.get(int(child))
        if s_guid is not None and c_guid is not None:
            contained_in_rows.append((c_guid, s_guid))

    # Storey name lookup (small list, linear scan is fine).
    storey_name_by_guid = {sr.guid: sr.name for sr in storeys}

    # Type linkage from IfcRelDefinesByType. Build two lookups: type
    # step_id → (type_guid, type_name) and product step_id → that pair.
    type_meta_by_step: dict[int, tuple[str, Optional[str]]] = {}
    type_objects_raw = raw.get("type_objects") or {}
    for tsid, tguid, tname in zip(
        type_objects_raw.get("step_id", []),
        type_objects_raw.get("guid", []),
        type_objects_raw.get("name", []),
    ):
        type_meta_by_step[int(tsid)] = (tguid, tname)

    product_type_by_step: dict[int, tuple[str, Optional[str]]] = {}
    dbt_raw = raw.get("defines_by_type") or {}
    for psid, tsid in zip(
        dbt_raw.get("product", []), dbt_raw.get("type", [])
    ):
        meta = type_meta_by_step.get(int(tsid))
        if meta is not None:
            product_type_by_step[int(psid)] = meta

    products: list[ProductRow] = []
    pdata = raw["products"]
    n = len(pdata["step_id"])
    for i in range(n):
        sid = int(pdata["step_id"][i])
        entity = pdata["entity"][i]
        mode = classify_by_name(entity, schema or "IFC4")
        storey_guid = contained_in.get(sid)
        object_type = pdata["object_type"][i]
        # Three-tier resolution: IfcRelDefinesByType wins, then
        # IfcRoot.ObjectType as the Revit-export fallback, then nothing.
        ifc_type = product_type_by_step.get(sid)
        if ifc_type is not None:
            type_guid, type_name = ifc_type
            type_source = "ifctype"
        elif object_type:
            type_guid, type_name, type_source = None, object_type, "objecttype"
        else:
            type_guid, type_name, type_source = None, None, "none"
        products.append(
            ProductRow(
                guid=pdata["guid"][i],
                entity=entity,
                name=pdata["name"][i],
                predefined_type=pdata["predefined_type"][i],
                object_type=object_type,
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
                type_guid=type_guid,
                type_name=type_name,
                type_source=type_source,
            )
        )

    # Spaces — Rust core emits step_id + guid only; richer fields (name,
    # elevation, long_name) land later as the indexer learns them.
    spaces: list[SpaceRow] = []
    raw_spaces = raw.get("spaces", {})
    for sid, sguid in zip(
        raw_spaces.get("step_id", []), raw_spaces.get("guid", [])
    ):
        spaces.append(SpaceRow(guid=sguid, step_id=int(sid)))

    # Type objects (IfcWallType, IfcDoorType, …) — caught by the Rust
    # byte-suffix fallback. Schema mirrors SpaceRow plus entity + name.
    type_objects: list[TypeObjectRow] = []
    raw_types = raw.get("type_objects", {})
    for tsid, tent, tguid, tname in zip(
        raw_types.get("step_id", []),
        raw_types.get("entity", []),
        raw_types.get("guid", []),
        raw_types.get("name", []),
    ):
        type_objects.append(
            TypeObjectRow(guid=tguid, entity=tent, name=tname, step_id=int(tsid))
        )

    # Voids — IfcRelVoidsElement (opening_step_id, host_step_id) marshalled
    # from Rust. Resolve to guids via the product step→guid lookup; openings
    # are products too, so the same map works for both sides.
    voids_rows: list[tuple[str, str]] = []
    voids_raw = raw.get("voids") or {}
    for opening, host in zip(
        voids_raw.get("opening", []), voids_raw.get("host", [])
    ):
        og = product_step_to_guid.get(int(opening))
        hg = product_step_to_guid.get(int(host))
        if og is not None and hg is not None:
            voids_rows.append((og, hg))

    import pandas as pd

    contained_in_df = pd.DataFrame(
        contained_in_rows, columns=["product_guid", "storey_guid"]
    )
    aggregates_df = pd.DataFrame(
        aggregates_rows, columns=["child_guid", "parent_guid", "parent_kind"]
    )
    storey_building_df = pd.DataFrame(
        storey_building_pairs, columns=["storey_guid", "building_guid"]
    )
    voids_df = pd.DataFrame(
        voids_rows, columns=["opening_guid", "host_guid"]
    )

    return Model(
        header=hdr,
        schema=schema or "",
        unit_scale=raw.get("unit_scale"),
        project_name=raw.get("project_name"),
        authoring_app=raw.get("authoring_app"),
        storeys=storeys,
        products=products,
        spaces=spaces,
        type_objects=type_objects,
        type_counts=type_counts,
        parse_seconds=time.time() - started,
        _contained_in_df=contained_in_df,
        _aggregates_df=aggregates_df,
        _storey_building_df=storey_building_df,
        _voids_df=voids_df,
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
        type_guid=_v("type_guid"),
        type_name=_v("type_name"),
        type_source=_v("type_source") or "none",
    )


# ---- Introspection helpers (used by Model.summary/schemas/preview) ----


def _df_meta(df) -> dict:
    """Shape + column list of a relationship DataFrame (never None — empty
    DataFrames are returned as ``{rows: 0, columns: [...], loaded: True}``)."""
    if df is None:
        return {"rows": 0, "columns": [], "loaded": False}
    return {
        "rows": int(len(df)),
        "columns": list(df.columns),
        "loaded": True,
    }


def _df_schema(df, loaded: Optional[bool] = None) -> dict:
    """Shape + column dtypes of a DataFrame, or a not-loaded stub."""
    if df is None:
        return {
            "rows": 0,
            "columns": [],
            "dtypes": {},
            "loaded": False if loaded is None else loaded,
        }
    return {
        "rows": int(len(df)),
        "columns": list(df.columns),
        "dtypes": {col: str(dtype) for col, dtype in df.dtypes.items()},
        "loaded": True if loaded is None else loaded,
    }


def _df_schema_from_dataclass(cls, rows: int = 0) -> dict:
    """Schema entry for a dataclass-backed row collection (products/storeys)."""
    columns: list[str] = []
    dtypes: dict[str, str] = {}
    for f in cls.__dataclass_fields__.values():
        columns.append(f.name)
        dtypes[f.name] = str(f.type)
    return {
        "rows": rows,
        "columns": columns,
        "dtypes": dtypes,
        "loaded": True,
    }


def _data_layer_meta(data_layers, name: str) -> dict:
    """Shape of a lazy data layer without forcing an extract."""
    df = getattr(data_layers, name, None) if data_layers is not None else None
    if df is None:
        return {"rows": 0, "columns": [], "loaded": False}
    return {
        "rows": int(len(df)),
        "columns": list(df.columns),
        "loaded": True,
    }


def _index_products_by_guid(m) -> dict[str, dict]:
    """Build ``{guid: {field: value, ...}}`` lookup over a Model's products.

    Works on both eager (cold-parse) and lazy (cache-hit) Models.
    """
    out: dict[str, dict] = {}
    if getattr(m, "_products_df", None) is not None:
        cols = list(m._products_df.columns)
        for row in m._products_df.itertuples(index=False):
            rec = dict(zip(cols, row))
            guid = rec.get("guid")
            if guid:
                out[guid] = rec
    else:
        from dataclasses import asdict

        for p in m.products:
            out[p.guid] = asdict(p)
    return out


def _accumulate_type(per_type: dict, rec: dict, sample_guids: int) -> None:
    """Fold one product row into the type-summary accumulator."""
    entity = rec.get("entity") or ""
    if not entity:
        return
    agg = per_type[entity]
    agg["count"] += 1
    storey = rec.get("storey_name")
    if storey is not None and not (isinstance(storey, float) and storey != storey):
        agg["storeys"].add(storey)
    pt = rec.get("predefined_type")
    if pt is not None and not (isinstance(pt, float) and pt != pt):
        agg["predefined_types"].add(pt)
    ot = rec.get("object_type")
    if ot is not None and not (isinstance(ot, float) and ot != ot):
        agg["object_types"].add(ot)
    if len(agg["sample_guids"]) < sample_guids:
        guid = rec.get("guid")
        if guid:
            agg["sample_guids"].append(guid)


def _none_if_nan_simple(v):
    """NaN-aware ``None`` coercion for serialising preview rows."""
    if v is None:
        return None
    try:
        import math
        if isinstance(v, float) and math.isnan(v):
            return None
    except Exception:
        pass
    return v
