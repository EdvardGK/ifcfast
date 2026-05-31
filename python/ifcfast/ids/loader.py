"""Load IDS documents via IfcTester."""

from __future__ import annotations

from pathlib import Path
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from ifctester.ids import Ids


def load_ids(path: str | Path, *, validate_xml: bool = False) -> "Ids":
    from ifctester import ids as ifctester_ids

    return ifctester_ids.open(str(path), validate=validate_xml)
