"""Collect the IDS conformance harness in directory runs (``pytest tests/oracle``).

The harness lives in :mod:`tests.oracle.ids_conformance` (importable + CLI);
this shim only makes pytest's ``test_*.py`` discovery pick it up.
"""

from .ids_conformance import ids_session, test_ids_conformance, xfails  # noqa: F401
