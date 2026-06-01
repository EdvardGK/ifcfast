"""Static lookup tables shipped with the wheel.

* :mod:`ifcfast.data.schema_supertypes` — per-schema IFC entity → immediate
  supertype maps for IFC2X3, IFC4, IFC4X3. Generated once at build time
  from ``ifcopenshell``; consumed at runtime by :mod:`ifcfast.classify`
  to walk the inheritance chain without a runtime ``ifcopenshell``
  dependency. Regenerate via ``scripts/gen_schema_supertypes.py``.
"""
