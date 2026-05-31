"""IDS 1.0 validation on ifcfast columnar indexes (IfcTester loads .ids XML)."""

from __future__ import annotations

from ifcfast.ids.engine import validate, validate_loaded
from ifcfast.ids.loader import load_ids
from ifcfast.ids.report import IdsValidationReport

__all__ = ["validate", "validate_loaded", "load_ids", "IdsValidationReport", "ENGINE_VERSION"]

ENGINE_VERSION = "0.1.0"
