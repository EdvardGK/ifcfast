"""IDS xs:restriction matching (aligned with ifctester.facet.Restriction)."""

from __future__ import annotations

import re
from typing import Any

from xmlschema.validators import identities


def _cast_to_value(from_value: Any, to_value: Any) -> Any:
    try:
        target_type = type(to_value).__name__
        if target_type == "int":
            return float(from_value)
        if target_type == "bool":
            if from_value in ("true", "1", "True", True):
                return True
            if from_value in ("false", "0", "False", False):
                return False
        return type(to_value)(from_value)
    except (ValueError, TypeError):
        return from_value


def value_matches_restriction(other: Any, restriction: Any) -> bool:
    """Return True if ``other`` satisfies an ifctester ``Restriction`` (or exact str)."""
    if restriction is None:
        return True
    if not hasattr(restriction, "options"):
        if other is None:
            return False
        a, b = str(other), str(restriction)
        if a == b:
            return True
        try:
            return float(a) == float(b)
        except (ValueError, TypeError):
            return False

    options = restriction.options or {}
    if other is None:
        return False

    for constraint, raw in options.items():
        values = raw if isinstance(raw, list) else [raw]
        try:
            if constraint == "enumeration":
                if other not in [_cast_to_value(v, other) for v in values]:
                    if str(other) not in [str(v) for v in values]:
                        return False
            elif constraint == "pattern":
                if not isinstance(other, str):
                    return False
                matched = False
                for pattern in values:
                    pat = identities.translate_pattern(str(pattern))
                    if re.compile(pat).fullmatch(other) is not None:
                        matched = True
                        break
                if not matched:
                    return False
            elif constraint == "length":
                if len(str(other)) != int(values[0]):
                    return False
            elif constraint == "maxLength":
                if len(str(other)) > int(values[0]):
                    return False
            elif constraint == "minLength":
                if len(str(other)) < int(values[0]):
                    return False
            elif constraint == "maxExclusive":
                if float(other) >= float(values[0]):
                    return False
            elif constraint == "maxInclusive":
                if float(other) > float(values[0]):
                    return False
            elif constraint == "minExclusive":
                if float(other) <= float(values[0]):
                    return False
            elif constraint == "minInclusive":
                if float(other) < float(values[0]):
                    return False
        except (ValueError, TypeError):
            return False
    return True


def restriction_enumeration(restriction: Any) -> list[str] | None:
    if restriction is None or not hasattr(restriction, "options"):
        return None
    enum = restriction.options.get("enumeration")
    if enum is None:
        return None
    if isinstance(enum, list):
        return [str(v) for v in enum]
    return [str(enum)]
