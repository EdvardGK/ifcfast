"""Materials oracle test (GH #59 M1).

Differential gate for the ``materials`` surface: diff ifcfast's
``m.materials`` against ``ifcopenshell.util.element.get_material`` (the
authored material construct: direct ``IfcMaterial`` /
``IfcMaterialLayerSet`` / ``IfcMaterialProfileSet``) over a committed
tiny fixture that carries all three association shapes.

Like the quantities adapter, this drives the comparison through the
shared :mod:`tests.oracle.normalize` DSL + :mod:`tests.oracle.report`
collector, so a sweep yields a *structured* diff list (every
disagreement, not the first failing assert) that CI can triage.

Schema alignment (the load-bearing decision)
--------------------------------------------
``m.materials`` is a flat row table; each row is one *constituent* of a
product's material association:

    guid | role            | layer_index | material_name | layer_thickness_mm | category
    -----+-----------------+-------------+---------------+--------------------+---------
    W1   | direct          | -1          | Concrete      | NaN                | Concrete
    W2   | layer           | 0           | GypsumLayer   | 13.0               | Board
    W2   | layer           | 1           | InsulationLayer | 100.0            | Thermal
    B1   | profile         | 0           | SteelProfile  | NaN                | Metal

``role``/``layer_index`` mirror the IFC traversal:

* ``direct``  -> the ``IfcMaterial`` itself (``layer_index == -1``).
* ``layer``   -> one ``IfcMaterialLayer`` of an ``IfcMaterialLayerSet``,
  in declared order; ``material_name`` is the **layer's** ``Name``
  (NOT the underlying ``IfcMaterial.Name``), ``category`` is the
  underlying material's ``Category``, ``layer_thickness_mm`` is
  ``LayerThickness`` scaled to millimetres.
* ``profile`` -> one ``IfcMaterialProfile`` of an
  ``IfcMaterialProfileSet``; ``material_name`` is the **profile's**
  ``Name``, ``category`` the underlying material's ``Category``.

So the faithful oracle is **not** ``get_materials`` (which flattens to
the underlying ``IfcMaterial`` objects and would surface ``Gypsum`` /
``Steel`` instead of ``GypsumLayer`` / ``SteelProfile``). It is a
traversal of ``get_material`` that reproduces ifcfast's exact
projection: for layers/profiles, the *wrapper* ``Name`` plus the
underlying material's ``Category`` plus the (scaled) thickness.

Both sides are projected onto a single
``{guid: {"material": {<constituent-key>: <value>}}}`` shape and
diffed; an empty diff list is full agreement.
"""

from __future__ import annotations

import math
from typing import Any

from . import normalize as nz
from .report import Collector

SURFACE = "materials"
FIXTURE = "materials.ifc"

#: One material group per product; the constituents live as distinct
#: keys inside the single group payload.
GROUP = "material"

#: ifcfast scales layer thickness to millimetres. The fixture authors
#: thickness in metres (LENGTHUNIT = METRE), so the oracle multiplies by
#: 1000 to land in the same unit before tolerance comparison.
_M_TO_MM = 1000.0


# --- ifcfast side ------------------------------------------------------------

def _ours_grouped(fast) -> dict[str, dict[str, nz.NormalizedPayload]]:
    """ifcfast ``m.materials`` -> ``{guid: {"material": NormalizedPayload}}``.

    Each constituent row contributes a per-(role, index) family of leaf
    keys (``<role>[<index>].name`` / ``.category`` / ``.thickness_mm``)
    so the diff compares ifcfast's projection field-by-field rather than
    collapsing a constituent to a single opaque string. ``NaN`` thickness
    (direct / profile) is dropped — there's no thickness to compare.
    """
    rows: list[dict[str, Any]] = []
    for row in fast.materials.itertuples():
        guid = row.guid
        prefix = f"{row.role}[{row.layer_index}]"
        rows.append(
            {
                "guid": guid,
                "group": GROUP,
                "name": f"{prefix}.name",
                "value": row.material_name,
            }
        )
        rows.append(
            {
                "guid": guid,
                "group": GROUP,
                "name": f"{prefix}.category",
                "value": row.category,
            }
        )
        thick = row.layer_thickness_mm
        if thick is not None and not (isinstance(thick, float) and math.isnan(thick)):
            rows.append(
                {
                    "guid": guid,
                    "group": GROUP,
                    "name": f"{prefix}.thickness_mm",
                    "value": float(thick),
                }
            )

    return nz.normalize_grouped(
        rows,
        guid_key="guid",
        group_key="group",
        name_key="name",
        value_key="value",
    )


# --- ifcopenshell side -------------------------------------------------------

def _truth_payload(material) -> list[tuple[str, Any]]:
    """Project one ``get_material`` result into ifcfast-shaped leaf pairs.

    Reproduces ifcfast's ``role``/``layer_index`` projection exactly:

    * ``IfcMaterial``            -> ``direct[-1]`` (name + category).
    * ``IfcMaterialLayerSet``    -> ``layer[i]`` per ``MaterialLayers``,
      wrapper ``Name`` + underlying ``Material.Category`` + thickness.
    * ``IfcMaterialProfileSet``  -> ``profile[i]`` per ``MaterialProfiles``,
      wrapper ``Name`` + underlying ``Material.Category``.
    """
    pairs: list[tuple[str, Any]] = []

    if material.is_a("IfcMaterial"):
        pairs.append(("direct[-1].name", material.Name))
        pairs.append(("direct[-1].category", material.Category))
        return pairs

    if material.is_a("IfcMaterialLayerSet"):
        for i, layer in enumerate(material.MaterialLayers or []):
            pre = f"layer[{i}]"
            pairs.append((f"{pre}.name", layer.Name))
            mat = layer.Material
            pairs.append((f"{pre}.category", mat.Category if mat else None))
            thick = layer.LayerThickness
            if thick is not None:
                pairs.append((f"{pre}.thickness_mm", float(thick) * _M_TO_MM))
        return pairs

    if material.is_a("IfcMaterialProfileSet"):
        for i, prof in enumerate(material.MaterialProfiles or []):
            pre = f"profile[{i}]"
            pairs.append((f"{pre}.name", prof.Name))
            mat = prof.Material
            pairs.append((f"{pre}.category", mat.Category if mat else None))
        return pairs

    # Other constructs (IfcMaterialList, IfcMaterialConstituentSet, ...)
    # are not in this tiny fixture; surfacing them as an empty payload
    # would create a spurious key_mismatch, so leave them out entirely.
    return pairs


def _truth_grouped(ios) -> dict[str, dict[str, nz.NormalizedPayload]]:
    """ifcopenshell ground truth -> ``{guid: {"material": NormalizedPayload}}``.

    ``ifcopenshell`` is imported *inside* this helper (not at module
    scope) so the main ``pytest -q`` run collects this file cleanly
    without the dev extra and skips via the conftest gate.
    """
    from ifcopenshell.util.element import get_material

    nested: dict[str, dict[str, dict[str, Any]]] = {}
    for el in ios.by_type("IfcProduct"):
        if not hasattr(el, "GlobalId"):
            continue
        material = get_material(el)
        if material is None:
            continue
        pairs = _truth_payload(material)
        if not pairs:
            continue
        nested[el.GlobalId] = {GROUP: dict(pairs)}
    return nz.normalize_grouped_payloads(nested)


# --- the gate ----------------------------------------------------------------

def test_materials_match_ifcopenshell(corpus):
    """Differential gate: ifcfast materials == ifcopenshell ground truth.

    Collects the full divergence surface, then asserts no blocking
    record. Any disagreement is ``unknown`` (hence blocking) until a
    classifier deliberately tags it benign — none is needed here because
    the projection is exact.
    """
    fx = corpus(FIXTURE)

    ours = _ours_grouped(fx.fast)
    truth = _truth_grouped(fx.ios)

    # --- anti-vacuous-green guard -------------------------------------------
    # A green diff is only meaningful if leaves were actually compared. The
    # materials.ifc fixture authors 4 constituents (1 direct + 2 layers + 1
    # profile) across 3 products, each projected to >=2 leaf keys (name +
    # category, plus thickness_mm for layers) -> 10 leaf pairs. A column
    # rename or empty fixture collapses this to 0 and must FAIL.
    n_groups = sum(len(groups) for groups in ours.values())
    n_leaves = sum(
        len(p.items) for groups in ours.values() for p in groups.values()
    )
    assert n_groups >= 1, f"ifcfast produced no material groups: {ours!r}"
    assert n_leaves >= 10, (
        f"expected >=10 material leaf pairs compared from {FIXTURE}, "
        f"got {n_leaves}"
    )
    print(
        f"[oracle:{SURFACE}] compared {n_leaves} material leaf pair(s) "
        f"across {len(ours)} product(s)"
    )

    diffs = nz.diff_grouped(ours, truth)

    collector = Collector()
    collector.extend_from_diffs(diffs, surface=SURFACE, fixture=fx.name)
    collector.assert_clean()


def test_materials_fixture_covers_all_three_shapes(corpus):
    """Guard the fixture itself: direct + layer-set + profile-set present.

    A green differential gate is only meaningful if the fixture actually
    exercises the three association shapes. If a future fixture edit
    drops one, this fails loudly rather than letting the oracle pass
    vacuously.
    """
    fx = corpus(FIXTURE)
    roles = set(fx.fast.materials["role"].tolist())
    assert {"direct", "layer", "profile"} <= roles, roles
