"""ifcfast — fast native IFC parsing, data extraction and analytics.

Quick start::

    import ifcfast

    m = ifcfast.open("model.ifc")
    print(len(m), "products,", len(m.storeys), "storeys")
    print(m.types())                     # {'IfcWall': 1234, 'IfcDoor': 45, ...}

    walls = list(m.filter(entity="IfcWall"))

    # Long-format data layers (pandas DataFrames, lazy).
    m.psets             # property sets
    m.quantities        # base quantities
    m.materials         # material assignments
    m.classifications   # classification references
    m.drift             # placement-vs-mesh drift report (when built with `mesh`)

Public API:

* :func:`open` — open and index an IFC file (returns :class:`Model`).
* :func:`header` — parse only the STEP header (no full index).
* :class:`Model` — the parsed index with lazy data-layer properties.
* :class:`ProductRow`, :class:`StoreyRow` — the row dataclasses.
* :mod:`ifcfast.classify` — element-mode policy (count / measure / linear / skip).
* :mod:`ifcfast.cache` — parquet cache layer.
"""

from __future__ import annotations

from .header import IFCHeader, header
from .model import Model, ProductRow, StoreyRow, open_ifc as open
from . import cache, classify

__all__ = [
    "IFCHeader",
    "Model",
    "ProductRow",
    "StoreyRow",
    "cache",
    "classify",
    "header",
    "open",
]

__version__ = "0.1.0"
