"""Unit tests for the clash-sweep per-rule tolerance semantics (#141).

Pure-logic tests for the harness helpers — no corpus, no engine run.
Each Solibri rule implies its own tolerance (clash rule = contact,
clearance rule = band), so a truth pair is judged against the tolerance
of ITS rule, never a global one.
"""

from __future__ import annotations

import argparse

import pytest

from .clash_sweep import make_tol_for_rule, pair_matches, parse_rule_tol


class TestParseRuleTol:
    def test_plain(self):
        assert parse_rule_tol("RIVv=0.01") == ("RIVv", 0.01)

    def test_rule_name_with_spaces_and_dashes(self):
        assert parse_rule_tol("10.1. RIE - RIVv=0.1") == ("10.1. RIE - RIVv", 0.1)

    def test_missing_equals_rejected(self):
        with pytest.raises(argparse.ArgumentTypeError):
            parse_rule_tol("RIVv")

    def test_empty_rule_rejected(self):
        with pytest.raises(argparse.ArgumentTypeError):
            parse_rule_tol("=0.1")


class TestTolForRule:
    def test_exact_match_and_fallback(self):
        f = make_tol_for_rule([("RIVv", 0.01)], base=0.0)
        assert f("RIVv") == 0.01
        assert f("10.1. RIE - RIVv") == 0.0  # exact does NOT prefix-match
        assert f("") == 0.0

    def test_prefix_pattern(self):
        f = make_tol_for_rule([("10.*", 0.1)], base=0.0)
        assert f("10.1. RIE - RIVv") == 0.1
        assert f("2.3. ARK Div - RIE") == 0.0

    def test_exact_wins_over_prefix(self):
        f = make_tol_for_rule([("10.*", 0.1), ("10.1. RIE - RIVv", 0.05)], base=0.0)
        assert f("10.1. RIE - RIVv") == 0.05


class TestPairMatches:
    META = {frozenset(("a", "b")): ("clash", "clearance", 0.054)}

    def test_within_band(self):
        assert pair_matches(self.META, frozenset(("a", "b")), 0.1)

    def test_outside_band(self):
        assert not pair_matches(self.META, frozenset(("a", "b")), 0.01)

    def test_not_found_never_matches(self):
        assert not pair_matches(self.META, frozenset(("x", "y")), 10.0)

    def test_f32_rounding_at_band_edge(self):
        # engine distance is f32; a pair sitting exactly on the band edge
        # must not flap on the 7th decimal
        import numpy as np

        meta = {frozenset(("a", "b")): ("clash", "clearance", float(np.float32(0.1)))}
        assert pair_matches(meta, frozenset(("a", "b")), 0.1)
