"""Differential-testing oracle suite (GH #59).

``ifcopenshell`` is the *oracle*, never a runtime dependency: these
tests open each committed tiny fixture in **both** ``ifcfast`` and
``ifcopenshell`` and diff the agent-visible surfaces (quantities,
psets, materials — geometry once GH #58 W11 lands) through a shared
tolerance/ordering DSL (:mod:`tests.oracle.normalize`) and a typed,
collect-all diff reporter (:mod:`tests.oracle.report`).

Milestone M1 (this scaffold) lifts the quantities ground-truth check
out of ``tests/test_quantities.py`` and routes it through the reusable
machinery so psets/materials/geometry adapters can be added without
re-deriving the comparison logic each time.

The whole package skips cleanly when ``ifcopenshell`` is not installed
(it's a dev-only extra), so wheel-test containers stay green.
"""
