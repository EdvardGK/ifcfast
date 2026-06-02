"""IFC -> parquet substrate (the input to :func:`ifcfast.clash`).

Writes ``instances.parquet`` + ``representations.parquet`` + ``view.sql``
to a bundle directory. The split is what makes the substrate chip-class
capable: a 5000-window facade whose families share an
``IfcRepresentationMap`` writes ~1 representation row + 5000 instance
rows instead of 5000 baked-geometry rows.

Example::

    import ifcfast
    info = ifcfast.bundle("model.ifc")        # -> model.bundle/
    df   = ifcfast.clash(info["bundle_dir"])  # broad + narrow phase

CLI parity::

    ifcfast bundle model.ifc                  # writes ./model.bundle/
    ifcfast bundle model.ifc out/             # writes ./out/
"""

from __future__ import annotations

from os import PathLike
from pathlib import Path

from . import _core


def bundle(
    ifc_path: str | PathLike[str],
    out_dir: str | PathLike[str] | None = None,
) -> dict:
    """Build the parquet substrate from an IFC file.

    Args:
        ifc_path: path to an IFC file (plain ``.ifc`` or ``.ifczip``).
        out_dir: target directory. ``None`` (default) writes to
            ``{stem}.bundle/`` next to ``ifc_path``.

    Returns:
        Dict with paths and counters. Notable keys:

        * ``bundle_dir`` — the output directory (pass this to
          :func:`ifcfast.clash`).
        * ``instances_parquet`` / ``representations_parquet`` /
          ``view_sql`` — the three files written.
        * ``products_seen`` / ``products_meshed`` /
          ``products_deferred`` — streaming stats.
        * ``instances_written`` / ``unique_reps_written`` — final row
          counts. The instance/rep ratio is the chip-class win.
        * ``open_ms`` / ``bundle_ms`` / ``stream_ms`` — timings (ms).
    """
    info = _core.bundle(
        str(ifc_path),
        str(out_dir) if out_dir is not None else None,
    )
    return dict(info)


__all__ = ["bundle"]
