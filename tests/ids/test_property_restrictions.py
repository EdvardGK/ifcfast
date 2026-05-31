"""Unit tests for restriction matching."""

from __future__ import annotations

from ifctester.facet import Restriction

from ifcfast.ids.facets.restrictions import value_matches_restriction


def test_enumeration_restriction():
    r = Restriction().parse(
        {"@base": "xs:string", "xs:enumeration": [{"@value": "A"}, {"@value": "B"}]}
    )
    assert value_matches_restriction("A", r)
    assert not value_matches_restriction("C", r)


def test_pattern_restriction():
    r = Restriction().parse({"@base": "xs:string", "xs:pattern": {"@value": "Wall.*"}})
    assert value_matches_restriction("Wall-001", r)
    assert not value_matches_restriction("Slab-1", r)
